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
        "SELECT id, title FROM papers ORDER BY imported_at DESC LIMIT 100"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    let settings = crate::core::settings_service::cached_settings();
    let label_max = settings.graph_node_label_max_chars.max(1) as usize;
    for (id, title) in &papers {
        nodes.push(GraphNode { id: id.clone(), label: title.chars().take(label_max).collect(), node_type: "paper".into(), color: "#3b82f6".into() });
    }

    // Notes
    let notes = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT id, title, paper_id FROM notes LIMIT 100"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (id, title, paper_id) in &notes {
        nodes.push(GraphNode { id: id.clone(), label: title.clone(), node_type: "note".into(), color: "#27ae60".into() });
        if let Some(pid) = paper_id {
            edges.push(GraphEdge { source: id.clone(), target: pid.clone(), edge_type: "attached_to".into() });
        }
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
        "SELECT id, title, source_type, source_id FROM knowledge_items WHERE source_type = 'paper' LIMIT 50"
    ).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    for (id, title, _, source_id) in &items {
        nodes.push(GraphNode { id: id.clone(), label: title.clone(), node_type: "knowledge_item".into(), color: "#8e44ad".into() });
        if let Some(sid) = source_id {
            edges.push(GraphEdge { source: id.clone(), target: sid.clone(), edge_type: "references".into() });
        }
    }

    Ok(GraphData { nodes, edges })
}

/// Build a local graph centered on a note, up to `depth` hops (1 or 2).
pub async fn build_local_graph(
    db: &SqlitePool,
    note_id: &str,
    depth: i32,
) -> Result<GraphData, String> {
    let depth = depth.clamp(1, 2);
    let mut node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut edge_set: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut frontier = vec![note_id.to_string()];
    let mut current_depth = 0;

    node_ids.insert(note_id.to_string());

    while current_depth < depth && !frontier.is_empty() {
        let mut next_frontier = Vec::new();
        for id in &frontier {
            // Outgoing links from this note
            let outgoing: Vec<(String,)> = sqlx::query_as(
                "SELECT target_id FROM note_links WHERE source_id = ?"
            ).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
            for (target,) in outgoing {
                edge_set.insert((id.clone(), target.clone()));
                if node_ids.insert(target.clone()) {
                    next_frontier.push(target);
                }
            }
            // Incoming links to this note
            let incoming: Vec<(String,)> = sqlx::query_as(
                "SELECT source_id FROM note_links WHERE target_id = ?"
            ).bind(id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
            for (source,) in incoming {
                edge_set.insert((source.clone(), id.clone()));
                if node_ids.insert(source.clone()) {
                    next_frontier.push(source);
                }
            }
        }
        frontier = next_frontier;
        current_depth += 1;
    }

    // Fetch note details
    let mut nodes = Vec::new();
    for id in &node_ids {
        let row: Option<(String,)> = sqlx::query_as("SELECT title FROM notes WHERE id = ?")
            .bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;
        if let Some((title,)) = row {
            let color = if id == note_id { "#E67E22".into() } else { "#27AE60".into() };
            nodes.push(GraphNode {
                id: id.clone(),
                label: title,
                node_type: "note".into(),
                color,
            });
        }
    }

    let mut edges = Vec::new();
    for (source, target) in edge_set {
        edges.push(GraphEdge { source, target, edge_type: "links_to".into() });
    }

    Ok(GraphData { nodes, edges })
}
