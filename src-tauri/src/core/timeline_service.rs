use sqlx::SqlitePool;
use serde::Serialize;

/// A single cross-module activity entry for the timeline page.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineItem {
    pub id: String,
    pub activity_type: String,
    pub module: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub timestamp: String,
    /// Target route (TanStack path), e.g. "/reader/$paperId".
    pub route: String,
    /// Path params for the target route.
    pub params: Option<serde_json::Value>,
    /// Search params (query) for the target route.
    pub search: Option<serde_json::Value>,
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let mut out: String = t.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Parse a JSON array of author strings and join them into one line.
fn join_authors(json: &str) -> Option<String> {
    if json.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let arr = v.as_array()?;
    let names: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn push(mut items: Vec<TimelineItem>, item: TimelineItem, module: &Option<String>) -> Vec<TimelineItem> {
    if let Some(m) = module {
        if &item.module != m {
            return items;
        }
    }
    items.push(item);
    items
}

/// Aggregate recent activities from all modules, newest first.
///
/// `module` optionally filters by one module key:
/// library | zhisi | notes | research | knowledge.
pub async fn list_timeline(
    db: &SqlitePool,
    limit: i64,
    offset: i64,
    module: Option<String>,
) -> Result<Vec<TimelineItem>, String> {
    let mut items: Vec<TimelineItem> = Vec::new();

    // ── 1. Imported papers ──
    #[derive(sqlx::FromRow)]
    struct PaperRow {
        id: String,
        title: String,
        authors: String,
        created_at: String,
    }
    let rows: Vec<PaperRow> = sqlx::query_as(
        "SELECT id, title, authors, created_at FROM papers ORDER BY created_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline papers: {e}"))?;
    for r in rows {
        items = push(
            items,
            TimelineItem {
                id: format!("paper-{}", r.id),
                activity_type: "paper_imported".into(),
                module: "library".into(),
                title: r.title,
                subtitle: join_authors(&r.authors),
                timestamp: r.created_at,
                route: "/reader/$paperId".into(),
                params: Some(serde_json::json!({ "paperId": r.id })),
                search: None,
            },
            &module,
        );
    }

    // ── 2. Snippets created / translated (智思) ──
    #[derive(sqlx::FromRow)]
    struct AnnotationRow {
        id: String,
        paper_id: String,
        text: Option<String>,
        created_at: String,
        updated_at: String,
        translation: Option<String>,
        paper_title: String,
    }
    let rows: Vec<AnnotationRow> = sqlx::query_as(
        "SELECT a.id, a.paper_id, a.text, a.created_at, a.updated_at, a.translation, p.title AS paper_title
         FROM annotations a JOIN papers p ON p.id = a.paper_id
         ORDER BY a.updated_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline annotations: {e}"))?;
    for r in rows {
        let route = "/reader/$paperId".to_string();
        let params = Some(serde_json::json!({ "paperId": r.paper_id }));
        let excerpt = r.text.as_deref().map(|t| truncate(t, 60)).unwrap_or_default();
        items = push(
            items,
            TimelineItem {
                id: format!("snippet-created-{}", r.id),
                activity_type: "snippet_created".into(),
                module: "zhisi".into(),
                title: r.paper_title.clone(),
                subtitle: if excerpt.is_empty() {
                    None
                } else {
                    Some(format!("新增摘录：{excerpt}"))
                },
                timestamp: r.created_at,
                route: route.clone(),
                params: params.clone(),
                search: None,
            },
            &module,
        );
        // Translated snippets: only when a translation exists.
        if r.translation.as_deref().is_some_and(|t| !t.trim().is_empty()) {
            items = push(
                items,
                TimelineItem {
                    id: format!("snippet-translated-{}", r.id),
                    activity_type: "snippet_translated".into(),
                    module: "zhisi".into(),
                    title: r.paper_title,
                    subtitle: if excerpt.is_empty() {
                        None
                    } else {
                        Some(format!("翻译摘录：{excerpt}"))
                    },
                    timestamp: r.updated_at,
                    route,
                    params,
                    search: None,
                },
                &module,
            );
        }
    }

    // ── 3. Notes created / human-edited / agent-edited ──
    #[derive(sqlx::FromRow)]
    struct NoteRow {
        id: String,
        title: String,
        created_at: String,
        updated_at: String,
        agent_edited_at: Option<String>,
    }
    let rows: Vec<NoteRow> = sqlx::query_as(
        "SELECT id, title, created_at, updated_at, agent_edited_at
         FROM notes WHERE is_folder = 0 AND is_system = 0
         ORDER BY updated_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline notes: {e}"))?;
    for r in rows {
        let route = "/notes".to_string();
        let search = Some(serde_json::json!({ "note": r.id }));
        // Agent edit (when the last change was made by the AI).
        if let Some(ae) = r.agent_edited_at.as_deref() {
            if r.updated_at == ae {
                items = push(
                    items,
                    TimelineItem {
                        id: format!("note-agent-{}", r.id),
                        activity_type: "note_agent_edited".into(),
                        module: "notes".into(),
                        title: r.title.clone(),
                        subtitle: Some("AI 整理笔记".into()),
                        timestamp: ae.to_string(),
                        route: route.clone(),
                        params: None,
                        search: search.clone(),
                    },
                    &module,
                );
            }
        }
        // Creation.
        items = push(
            items,
            TimelineItem {
                id: format!("note-created-{}", r.id),
                activity_type: "note_created".into(),
                module: "notes".into(),
                title: r.title.clone(),
                subtitle: None,
                timestamp: r.created_at.clone(),
                route: route.clone(),
                params: None,
                search: search.clone(),
            },
            &module,
        );
        // Human edit: an update that is not the agent edit timestamp.
        let human_edited = r.updated_at > r.created_at
            && r.agent_edited_at.as_deref() != Some(r.updated_at.as_str());
        if human_edited {
            items = push(
                items,
                TimelineItem {
                    id: format!("note-updated-{}", r.id),
                    activity_type: "note_updated".into(),
                    module: "notes".into(),
                    title: r.title,
                    subtitle: Some("编辑笔记".into()),
                    timestamp: r.updated_at,
                    route,
                    params: None,
                    search,
                },
                &module,
            );
        }
    }

    // ── 4. Research topics created ──
    #[derive(sqlx::FromRow)]
    struct TopicRow {
        id: String,
        name: String,
        created_at: String,
    }
    let rows: Vec<TopicRow> = sqlx::query_as(
        "SELECT id, name, created_at FROM research_topics ORDER BY created_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline topics: {e}"))?;
    for r in rows {
        items = push(
            items,
            TimelineItem {
                id: format!("topic-{}", r.id),
                activity_type: "research_topic_created".into(),
                module: "research".into(),
                title: r.name,
                subtitle: Some("新建科研课题".into()),
                timestamp: r.created_at,
                route: "/research/$topicId".into(),
                params: Some(serde_json::json!({ "topicId": r.id })),
                search: None,
            },
            &module,
        );
    }

    // ── 5. Research sources discovered ──
    #[derive(sqlx::FromRow)]
    struct SourceRow {
        id: String,
        topic_id: String,
        title: Option<String>,
        authors: Option<String>,
        discovered_at: String,
        topic_name: String,
    }
    let rows: Vec<SourceRow> = sqlx::query_as(
        "SELECT s.id, s.topic_id, s.title, s.authors, s.discovered_at, t.name AS topic_name
         FROM research_sources s JOIN research_topics t ON t.id = s.topic_id
         ORDER BY s.discovered_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline sources: {e}"))?;
    for r in rows {
        let title = r.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "未命名文献".into());
        let mut subtitle_parts: Vec<String> = vec![format!("课题：{}", r.topic_name)];
        if let Some(a) = r.authors.filter(|a| !a.trim().is_empty()) {
            subtitle_parts.push(truncate(&a, 60));
        }
        items = push(
            items,
            TimelineItem {
                id: format!("source-{}", r.id),
                activity_type: "research_source_discovered".into(),
                module: "research".into(),
                title,
                subtitle: Some(subtitle_parts.join(" · ")),
                timestamp: r.discovered_at,
                route: "/research/$topicId".into(),
                params: Some(serde_json::json!({ "topicId": r.topic_id })),
                search: None,
            },
            &module,
        );
    }

    // ── 6. Knowledge items created ──
    #[derive(sqlx::FromRow)]
    struct KnowledgeRow {
        id: String,
        domain_id: String,
        title: String,
        created_at: String,
        domain_name: String,
    }
    let rows: Vec<KnowledgeRow> = sqlx::query_as(
        "SELECT k.id, k.domain_id, k.title, k.created_at, d.name AS domain_name
         FROM knowledge_items k JOIN knowledge_domains d ON d.id = k.domain_id
         ORDER BY k.created_at DESC LIMIT 500"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("timeline knowledge: {e}"))?;
    for r in rows {
        items = push(
            items,
            TimelineItem {
                id: format!("knowledge-{}", r.id),
                activity_type: "knowledge_item_created".into(),
                module: "knowledge".into(),
                title: r.title,
                subtitle: Some(format!("知识库 · {}", r.domain_name)),
                timestamp: r.created_at,
                route: "/knowledge/$domainId".into(),
                params: Some(serde_json::json!({ "domainId": r.domain_id })),
                search: None,
            },
            &module,
        );
    }

    // Newest first, then paginate.
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(items
        .into_iter()
        .skip(offset.max(0) as usize)
        .take(limit.max(0) as usize)
        .collect())
}
