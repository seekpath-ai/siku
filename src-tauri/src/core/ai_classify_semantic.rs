//! Semantic draft engine for AI 文献分类: cluster the paper set in embedding
//! space, then let the model name the clusters once.
//!
//! This is an *alternative draft generator*, not a separate pipeline: it
//! produces the same `(groups, per-paper assignments)` shape as the batch
//! engine, so the rest of the flow (user edit → closed-set assign → apply →
//! undo) is unchanged.
//!
//! Cost profile is the opposite of the batch engine: instead of
//! `ceil(N/24) + 1` LLM calls it spends **one** call, but it needs a real
//! embeddings endpoint (the built-in lexical placeholder cannot be clustered —
//! [`available`] gates on exactly that) and paper-level vectors take the mean
//! of the paper's chunk vectors, which is a blunt topical summary.
//!
//! `k` is chosen from the set size (`sqrt(N/2)`, clamped) because the caller
//! cannot know how many topics a library holds; the user can still merge or
//! split the result in the draft, which is why no expensive model-selection
//! heuristic is worth it here.

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::ai::llm::{ChatMessage, LlmClient};

/// Repeated k-means restarts; we keep the tightest result.
const KMEANS_RESTARTS: usize = 4;
const KMEANS_ITERATIONS: usize = 25;
/// Representative papers fed to the naming call, per cluster.
const TITLES_PER_CLUSTER: usize = 5;

/// True when the configured embeddings backend can actually be clustered.
pub fn available() -> bool {
    crate::ai::retriever::vector_leg_enabled()
}

fn chat_msg(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        reasoning_content: None,
        role: role.into(),
        content,
        attachments: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Paper-level vectors: mean of the paper's chunk vectors, L2-normalised.
/// Papers whose chunks were never embedded are skipped (they stay 待定 and can
/// be picked up by a closed-set pass afterwards).
async fn paper_vectors(
    db: &SqlitePool,
    paper_ids: &[String],
) -> Result<Vec<(String, Vec<f32>)>, String> {
    if paper_ids.is_empty() {
        return Ok(Vec::new());
    }
    let model = crate::ai::embedder::embedding_model_label();
    let ph = paper_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT c.paper_id, e.vector FROM chunks c
         JOIN embeddings e ON e.chunk_id = c.id
         WHERE c.paper_id IN ({ph}) AND e.model = ?"
    );
    let mut q = sqlx::query_as::<_, (String, Vec<u8>)>(&sql);
    for id in paper_ids {
        q = q.bind(id);
    }
    let q = q.bind(&model);
    let rows = q.fetch_all(db).await.map_err(|e| format!("db: {e}"))?;

    let mut sums: std::collections::HashMap<String, (Vec<f32>, usize)> =
        std::collections::HashMap::new();
    for (paper_id, blob) in rows {
        let v = crate::ai::embedder::blob_to_vector(&blob);
        if v.is_empty() {
            continue;
        }
        let entry = sums.entry(paper_id).or_insert_with(|| (vec![0.0f32; v.len()], 0));
        if entry.0.len() != v.len() {
            // Mixed dimensions (backend changed mid-flight): ignore the odd one.
            continue;
        }
        for (slot, x) in entry.0.iter_mut().zip(v.iter()) {
            *slot += *x;
        }
        entry.1 += 1;
    }

    let mut out: Vec<(String, Vec<f32>)> = Vec::new();
    // Keep the caller's order so the UI stays stable.
    for id in paper_ids {
        if let Some((mut sum, count)) = sums.remove(id) {
            if count == 0 {
                continue;
            }
            for x in sum.iter_mut() {
                *x /= count as f32;
            }
            normalize(&mut sum);
            out.push((id.clone(), sum));
        }
    }
    Ok(out)
}

/// Deterministic-ish initialisation: spread the seeds across the input order so
/// a rerun on the same library lands in comparable clusters.
fn initial_centroids(vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
    let n = vectors.len();
    (0..k)
        .map(|i| {
            let idx = (i * n / k.max(1)).min(n.saturating_sub(1));
            vectors[idx].clone()
        })
        .collect()
}

fn assign_all(vectors: &[Vec<f32>], centroids: &[Vec<f32>]) -> (Vec<usize>, f32) {
    let mut assignment = Vec::with_capacity(vectors.len());
    let mut inertia = 0.0f32;
    for v in vectors {
        let mut best = 0usize;
        let mut best_sim = f32::NEG_INFINITY;
        for (i, c) in centroids.iter().enumerate() {
            let sim = crate::ai::embedder::cosine_similarity(v, c);
            if sim > best_sim {
                best_sim = sim;
                best = i;
            }
        }
        assignment.push(best);
        inertia += 1.0 - best_sim;
    }
    (assignment, inertia)
}

fn recompute_centroids(
    vectors: &[Vec<f32>],
    assignment: &[usize],
    k: usize,
    dim: usize,
) -> Vec<Vec<f32>> {
    let mut sums = vec![vec![0.0f32; dim]; k];
    let mut counts = vec![0usize; k];
    for (v, &a) in vectors.iter().zip(assignment.iter()) {
        for (slot, x) in sums[a].iter_mut().zip(v.iter()) {
            *slot += *x;
        }
        counts[a] += 1;
    }
    let mut centroids = Vec::with_capacity(k);
    for i in 0..k {
        if counts[i] == 0 {
            // Empty cluster: leave a zero vector; assign_all will only pick it
            // when nothing else is closer, and naming drops it.
            centroids.push(vec![0.0f32; dim]);
        } else {
            let mut c = sums[i].clone();
            for x in c.iter_mut() {
                *x /= counts[i] as f32;
            }
            normalize(&mut c);
            centroids.push(c);
        }
    }
    centroids
}

/// k-means over cosine distance. Returns (assignment, centroids, inertia).
fn kmeans(
    vectors: &[Vec<f32>],
    k: usize,
) -> (Vec<usize>, Vec<Vec<f32>>, f32) {
    let dim = vectors.first().map(|v| v.len()).unwrap_or(0);
    let mut best: Option<(Vec<usize>, Vec<Vec<f32>>, f32)> = None;
    for restart in 0..KMEANS_RESTARTS {
        let mut centroids = initial_centroids(vectors, k);
        if restart > 0 {
            // Nudge later restarts so they are not identical to the first.
            for (i, c) in centroids.iter_mut().enumerate() {
                if let Some(offset) = vectors.get((i + restart) % vectors.len()) {
                    for (slot, x) in c.iter_mut().zip(offset.iter()) {
                        *slot = 0.5 * *slot + 0.5 * *x;
                    }
                }
            }
        }
        let mut assignment = vec![0usize; vectors.len()];
        let mut inertia = f32::MAX;
        for _ in 0..KMEANS_ITERATIONS {
            let (next_assignment, next_inertia) = assign_all(vectors, &centroids);
            let stable = next_assignment == assignment;
            assignment = next_assignment;
            inertia = next_inertia;
            centroids = recompute_centroids(vectors, &assignment, k, dim);
            if stable {
                break;
            }
        }
        if best.as_ref().map(|(_, _, i)| inertia < *i).unwrap_or(true) {
            best = Some((assignment, centroids, inertia));
        }
    }
    best.unwrap_or_else(|| (vec![0; vectors.len()], vec![vec![0.0; dim]; k], 0.0))
}

#[derive(Deserialize)]
struct RawClusterName {
    index: usize,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct RawClusterNames {
    #[serde(default)]
    clusters: Vec<RawClusterName>,
}

fn parse_names(content: &str) -> Result<Vec<RawClusterName>, String> {
    let start = content.find('{').ok_or_else(|| "模型未返回 JSON".to_string())?;
    let end = content.rfind('}').ok_or_else(|| "模型未返回 JSON".to_string())?;
    if end <= start {
        return Err("模型未返回 JSON".to_string());
    }
    let raw: RawClusterNames =
        serde_json::from_str(&content[start..=end]).map_err(|e| format!("解析簇名失败: {e}"))?;
    let names: Vec<RawClusterName> = raw
        .clusters
        .into_iter()
        .filter(|c| !c.name.trim().is_empty())
        .collect();
    if names.is_empty() {
        return Err("模型未返回任何簇名".to_string());
    }
    Ok(names)
}

/// Cluster the set and name every cluster with one LLM call.
///
/// Returns `(groups, assignments)` where assignments are per paper, ordered as
/// `(paper_id, confidence)` — the caller turns them into its own `Assign` rows.
pub async fn draft(
    db: &SqlitePool,
    llm: &dyn LlmClient,
    dimension: &str,
    paper_ids: &[String],
    max_groups: usize,
    briefs: &std::collections::HashMap<String, String>,
) -> Result<
    (
        Vec<crate::core::ai_classify::ClassifyGroup>,
        Vec<(String, Vec<(String, f64)>, String)>,
    ),
    String,
> {
    if !available() {
        return Err("未配置语义向量后端，无法使用语义聚类".to_string());
    }
    let vectors = paper_vectors(db, paper_ids).await?;
    if vectors.len() < 3 {
        return Err(format!(
            "只有 {} 篇文献已生成向量，语义聚类至少需要 3 篇（可在设置里重建索引）",
            vectors.len()
        ));
    }
    let k = ((vectors.len() as f64) / 2.0).sqrt().round() as usize;
    let k = k.clamp(3, max_groups.max(3).min(vectors.len()));

    let plain: Vec<Vec<f32>> = vectors.iter().map(|(_, v)| v.clone()).collect();
    let (assignment, centroids, _inertia) = kmeans(&plain, k);

    // Group papers by cluster, preserving the input order.
    let mut clusters: Vec<Vec<(String, f32)>> = vec![Vec::new(); k];
    for (i, (paper_id, v)) in vectors.iter().enumerate() {
        let ci = assignment[i];
        let sim = crate::ai::embedder::cosine_similarity(v, &centroids[ci]);
        clusters[ci].push((paper_id.clone(), sim));
    }

    // Describe each non-empty cluster by its best-matching members.
    let mut listing = String::new();
    let mut live: Vec<usize> = Vec::new();
    for (ci, members) in clusters.iter().enumerate() {
        if members.is_empty() {
            continue;
        }
        live.push(ci);
        let mut sorted = members.clone();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let titles = sorted
            .iter()
            .take(TITLES_PER_CLUSTER)
            .map(|(id, _)| format!("- {}", briefs.get(id).cloned().unwrap_or_else(|| id.clone())))
            .collect::<Vec<_>>()
            .join("\n");
        listing.push_str(&format!(
            "\n## 簇 {}（{} 篇）代表性文献：\n{}\n",
            ci,
            members.len(),
            titles
        ));
    }

    let system = format!(
        "你是学术文献分类专家。下面是把文献按语义向量聚成若干簇后，每个簇里最具代表性的文献标题。\
         请为每个簇起一个「{dimension}」维度的类目名（简短，不超过 12 字），并给一句话界定。\
         只输出 JSON：{{\"clusters\":[{{\"index\":<簇编号>,\"name\":\"类目名\",\"description\":\"一句话界定\"}}]}}。\
         不要输出 JSON 以外的任何内容。"
    );
    let messages = vec![
        chat_msg("system", system),
        chat_msg("user", format!("【待命名的簇】{listing}")),
    ];
    let resp = llm.chat_completion(&messages, &[]).await?;
    let names = parse_names(&resp.content)?;

    let mut groups: Vec<crate::core::ai_classify::ClassifyGroup> = Vec::new();
    let mut assignments: Vec<(String, Vec<(String, f64)>, String)> = Vec::new();
    for n in &names {
        if !live.contains(&n.index) {
            continue;
        }
        let name = n.name.trim().to_string();
        if groups.iter().any(|g| g.name == name) {
            // Two clusters collapsing onto one name: merge their papers below.
            continue;
        }
        groups.push(crate::core::ai_classify::ClassifyGroup::new(
            name.clone(),
            n.description.trim(),
        ));
        let members = &clusters[n.index];
        let papers: Vec<(String, f64)> = members
            .iter()
            .map(|(id, sim)| {
                // Map cosine [0,1] onto [0.5,1]: a well-matched paper clears a
                // 0.75 threshold, a loose one lands in 待审 instead of being
                // silently written.
                (id.clone(), (0.5 + 0.5 * sim.clamp(0.0, 1.0)) as f64)
            })
            .collect();
        let reason = format!("语义簇 {} · {} 篇", n.index, members.len());
        assignments.push((name, papers, reason));
    }

    // Clusters the model did not name would otherwise lose their papers: keep
    // them as one explicit "未命名语义簇" group so nothing silently disappears.
    let named: std::collections::HashSet<usize> = names.iter().map(|n| n.index).collect();
    let orphans: Vec<(String, f64)> = live
        .iter()
        .filter(|ci| !named.contains(ci))
        .flat_map(|ci| clusters[*ci].iter().cloned())
        .map(|(id, sim)| (id, (0.5 + 0.5 * sim.clamp(0.0, 1.0)) as f64))
        .collect();
    if !orphans.is_empty() {
        let name = "未命名语义簇".to_string();
        groups.push(crate::core::ai_classify::ClassifyGroup::new(&name, ""));
        assignments.push((name, orphans, "语义簇命名缺失".to_string()));
    }

    if groups.is_empty() {
        return Err("语义聚类没有产出可用类目".to_string());
    }
    Ok((groups, assignments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmeans_separates_two_obvious_groups() {
        // Two well-separated directions in 4-d space.
        let mut vectors = Vec::new();
        for _ in 0..5 {
            let mut v = vec![1.0, 0.1, 0.0, 0.0];
            normalize(&mut v);
            vectors.push(v);
        }
        for _ in 0..5 {
            let mut v = vec![0.0, 0.0, 1.0, 0.2];
            normalize(&mut v);
            vectors.push(v);
        }
        let (assignment, centroids, inertia) = kmeans(&vectors, 2);
        assert_eq!(centroids.len(), 2);
        assert!(inertia < 0.1, "inertia was {inertia}");
        assert_eq!(assignment[0], assignment[1]);
        assert_eq!(assignment[5], assignment[6]);
        assert_ne!(assignment[0], assignment[5]);
    }

    #[test]
    fn initial_centroids_are_within_bounds() {
        let vectors: Vec<Vec<f32>> = (0..5).map(|i| vec![i as f32, 1.0]).collect();
        let c = initial_centroids(&vectors, 3);
        assert_eq!(c.len(), 3);
        // No panic, and every seed is an actual vector.
        assert!(c.iter().all(|v| v.len() == 2));
    }

    #[test]
    fn parse_names_tolerates_prose_and_fences() {
        let raw = "结果如下：```json\n{\"clusters\":[{\"index\":0,\"name\":\"聚类方法\"}]}\n```";
        let names = parse_names(raw).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].name, "聚类方法");
    }

    #[test]
    fn parse_names_rejects_empty() {
        assert!(parse_names("{\"clusters\":[]}").is_err());
        assert!(parse_names("没有 JSON").is_err());
    }

    #[test]
    fn normalize_produces_unit_length() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((len - 1.0).abs() < 1e-6);
    }
}
