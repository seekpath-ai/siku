use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub node_type: String, // "paper" | "note" | "tag" | "knowledge_item"
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String, // "links_to" | "tagged" | "attached_to" | "references"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Build the full knowledge graph
pub async fn build_graph(db: &SqlitePool) -> Result<GraphData, String> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    // Papers
    let papers = sqlx::query_as::<_, (String, String)>(
        "SELECT id, title FROM papers ORDER BY imported_at DESC"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    let settings = crate::core::settings_service::cached_settings();
    let label_max = settings.graph_node_label_max_chars.max(1) as usize;
    for (id, title) in &papers {
        nodes.push(GraphNode { id: id.clone(), label: title.chars().take(label_max).collect(), node_type: "paper".into(), color: "#3b82f6".into() });
    }

    // Notes
    let notes = sqlx::query_as::<_, (String, String, Option<String>, String)>(
        "SELECT id, title, paper_id, tags FROM notes"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    let mut tag_nodes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ensure_tag = |name: &str, nodes: &mut Vec<GraphNode>| -> String {
        let id = format!("tag:{name}");
        if tag_nodes.insert(id.clone()) {
            nodes.push(GraphNode { id: id.clone(), label: format!("#{name}"), node_type: "tag".into(), color: "#eab308".into() });
        }
        id
    };
    for (id, title, paper_id, tags_json) in &notes {
        nodes.push(GraphNode { id: id.clone(), label: title.clone(), node_type: "note".into(), color: "#27ae60".into() });
        if let Some(pid) = paper_id {
            edges.push(GraphEdge { source: id.clone(), target: pid.clone(), edge_type: "attached_to".into() });
        }
        let note_tags: Vec<String> = serde_json::from_str(tags_json).unwrap_or_default();
        for tag in note_tags {
            let tag_id = ensure_tag(&tag, &mut nodes);
            edges.push(GraphEdge { source: id.clone(), target: tag_id, edge_type: "tagged".into() });
        }
    }

    // Paper tags
    let paper_tags = sqlx::query_as::<_, (String, String)>(
        "SELECT pt.paper_id, t.name FROM paper_tags pt INNER JOIN tags t ON t.id = pt.tag_id"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (paper_id, name) in &paper_tags {
        let tag_id = ensure_tag(name, &mut nodes);
        edges.push(GraphEdge { source: paper_id.clone(), target: tag_id, edge_type: "tagged".into() });
    }

    // Note links
    let nlinks = sqlx::query_as::<_, (String, String)>(
        "SELECT source_id, target_id FROM note_links"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (source, target) in &nlinks {
        edges.push(GraphEdge { source: source.clone(), target: target.clone(), edge_type: "links_to".into() });
    }

    // Knowledge items (cross-domain)
    let items = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
        "SELECT id, title, source_type, source_id FROM knowledge_items WHERE source_type = 'paper'"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (id, title, _, source_id) in &items {
        nodes.push(GraphNode { id: id.clone(), label: title.clone(), node_type: "knowledge_item".into(), color: "#8e44ad".into() });
        if let Some(sid) = source_id {
            edges.push(GraphEdge { source: id.clone(), target: sid.clone(), edge_type: "references".into() });
        }
    }

    Ok(GraphData { nodes, edges })
}

/// Collect every edge touching `id`, as (source, target, edge_type) triples
/// with the same direction conventions as build_graph.
async fn expand_node(
    db: &SqlitePool,
    id: &str,
    out: &mut Vec<(String, String, String)>,
) -> Result<(), String> {
    if let Some(name) = id.strip_prefix("tag:") {
        // Notes carrying this inline #tag (notes.tags is a JSON array of names).
        let needle = format!("\"{name}\"");
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT id FROM notes WHERE instr(tags, ?) > 0"
        ).bind(&needle).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
        for (note_id,) in rows {
            out.push((note_id, id.to_string(), "tagged".into()));
        }
        // Papers carrying this tag.
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT pt.paper_id FROM paper_tags pt INNER JOIN tags t ON t.id = pt.tag_id WHERE t.name = ?"
        ).bind(name).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
        for (paper_id,) in rows {
            out.push((paper_id, id.to_string(), "tagged".into()));
        }
        return Ok(());
    }

    // Note links, both directions.
    let linked = sqlx::query_as::<_, (String, String)>(
        "SELECT source_id, target_id FROM note_links WHERE source_id = ? OR target_id = ?"
    ).bind(id).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (source, target) in linked {
        out.push((source, target, "links_to".into()));
    }

    // If it's a note: its paper and its inline tags.
    let note_row = sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT paper_id, tags FROM notes WHERE id = ?"
    ).bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;
    if let Some((paper_id, tags_json)) = note_row {
        if let Some(pid) = paper_id {
            out.push((id.to_string(), pid, "attached_to".into()));
        }
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        for tag in tags {
            out.push((id.to_string(), format!("tag:{tag}"), "tagged".into()));
        }
    }

    // If it's a paper: attached notes, its tags, knowledge items referencing it.
    let attached = sqlx::query_as::<_, (String,)>(
        "SELECT id FROM notes WHERE paper_id = ?"
    ).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (note_id,) in attached {
        out.push((note_id, id.to_string(), "attached_to".into()));
    }
    let ptags = sqlx::query_as::<_, (String,)>(
        "SELECT t.name FROM paper_tags pt INNER JOIN tags t ON t.id = pt.tag_id WHERE pt.paper_id = ?"
    ).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (name,) in ptags {
        out.push((id.to_string(), format!("tag:{name}"), "tagged".into()));
    }
    let kitems = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, source_id FROM knowledge_items WHERE id = ? OR (source_type = 'paper' AND source_id = ?)"
    ).bind(id).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (kid, source_id) in kitems {
        if let Some(sid) = source_id {
            out.push((kid, sid, "references".into()));
        }
    }

    Ok(())
}

/// Build a local graph centered on any node (note, paper, tag, knowledge
/// item), up to `depth` hops (1 or 2).
pub async fn build_local_graph(
    db: &SqlitePool,
    center_id: &str,
    depth: i32,
) -> Result<GraphData, String> {
    let depth = depth.clamp(1, 2);
    let mut node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut edge_set: std::collections::HashSet<(String, String, String)> = std::collections::HashSet::new();
    let mut frontier = vec![center_id.to_string()];

    node_ids.insert(center_id.to_string());

    for _ in 0..depth {
        if frontier.is_empty() { break; }
        let mut next_frontier = Vec::new();
        for id in &frontier {
            let mut triples = Vec::new();
            expand_node(db, id, &mut triples).await?;
            for (source, target, edge_type) in triples {
                edge_set.insert((source.clone(), target.clone(), edge_type));
                for end in [source, target] {
                    if node_ids.insert(end.clone()) {
                        next_frontier.push(end);
                    }
                }
            }
        }
        frontier = next_frontier;
    }

    // Resolve node details.
    let mut nodes = Vec::new();
    for id in &node_ids {
        if let Some(name) = id.strip_prefix("tag:") {
            nodes.push(GraphNode { id: id.clone(), label: format!("#{name}"), node_type: "tag".into(), color: "#eab308".into() });
            continue;
        }
        let note = sqlx::query_as::<_, (String,)>("SELECT title FROM notes WHERE id = ?")
            .bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;
        if let Some((title,)) = note {
            nodes.push(GraphNode { id: id.clone(), label: title, node_type: "note".into(), color: "#27ae60".into() });
            continue;
        }
        let paper = sqlx::query_as::<_, (String,)>("SELECT title FROM papers WHERE id = ?")
            .bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;
        if let Some((title,)) = paper {
            let label_max = crate::core::settings_service::cached_settings().graph_node_label_max_chars.max(1) as usize;
            nodes.push(GraphNode { id: id.clone(), label: title.chars().take(label_max).collect(), node_type: "paper".into(), color: "#3b82f6".into() });
            continue;
        }
        let kitem = sqlx::query_as::<_, (String,)>("SELECT title FROM knowledge_items WHERE id = ?")
            .bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;
        if let Some((title,)) = kitem {
            nodes.push(GraphNode { id: id.clone(), label: title, node_type: "knowledge_item".into(), color: "#8e44ad".into() });
        }
    }
    // Keep only edges whose endpoints both resolved to real nodes.
    let valid: std::collections::HashSet<&String> = nodes.iter().map(|n| &n.id).collect();
    let edges = edge_set.into_iter()
        .filter(|(s, t, _)| valid.contains(s) && valid.contains(t))
        .map(|(source, target, edge_type)| GraphEdge { source, target, edge_type })
        .collect();

    Ok(GraphData { nodes, edges })
}
