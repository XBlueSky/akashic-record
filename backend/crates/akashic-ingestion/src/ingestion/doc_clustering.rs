use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use uuid::Uuid;

use akashic_domain::ports::{DocClusterGraphRepo, DocClusterRepo};
use akashic_llm::LlmProvider;
use akashic_store_neo4j::{Neo4jDocClusterGraphRepo, Neo4jPool};
use akashic_store_pg::PgDocClusterRepo;

/// Cluster sections of a website repo by embedding similarity,
/// then use LLM to name each cluster.
pub struct DocClustering {
    doc_cluster: Arc<dyn DocClusterRepo>,
    doc_cluster_graph: Arc<dyn DocClusterGraphRepo>,
    llm: Arc<dyn LlmProvider>,
}

struct SectionEmbedding {
    id: Uuid,
    heading: String,
    embedding: Vec<f32>,
}

impl DocClustering {
    pub fn new(pg: sqlx::PgPool, neo4j: Neo4jPool, llm: Arc<dyn LlmProvider>) -> Self {
        let doc_cluster: Arc<dyn DocClusterRepo> = Arc::new(PgDocClusterRepo::new(pg));
        let doc_cluster_graph: Arc<dyn DocClusterGraphRepo> =
            Arc::new(Neo4jDocClusterGraphRepo::new(neo4j));
        Self {
            doc_cluster,
            doc_cluster_graph,
            llm,
        }
    }

    /// Cluster all sections for a repo and store the results.
    /// Returns the number of clusters created.
    pub async fn cluster_repo_sections(&self, repo_name: &str) -> Result<usize> {
        // 1. Clean existing clusters for this repo
        self.clean_clusters(repo_name).await?;

        // 2. Fetch section embeddings
        let raw_rows = self.doc_cluster.fetch_section_embeddings(repo_name).await?;
        let sections: Vec<SectionEmbedding> = raw_rows
            .into_iter()
            .map(|r| SectionEmbedding {
                id: r.id,
                heading: r.heading,
                embedding: r.embedding,
            })
            .collect();
        if sections.len() < 4 {
            info!(
                repo_name,
                sections = sections.len(),
                "Too few sections to cluster"
            );
            return Ok(0);
        }

        // 3. Determine k and run k-means
        let k = determine_k(sections.len());
        let assignments = cluster_embeddings(&sections, k)?;

        // 4. Group sections by cluster
        let mut clusters: Vec<Vec<&SectionEmbedding>> = vec![Vec::new(); k];
        for (i, &cluster_idx) in assignments.iter().enumerate() {
            clusters[cluster_idx].push(&sections[i]);
        }

        // 5. Name and store each cluster
        let mut created = 0;
        // Names used this run, so two clusters that derive the same name get
        // distinct rows instead of colliding on doc_clusters' UNIQUE(repo_name,
        // name) and being merged by store_cluster's ON CONFLICT.
        let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (i, cluster_sections) in clusters.iter().enumerate() {
            if cluster_sections.is_empty() {
                continue;
            }

            // Compute centroid
            let centroid = compute_centroid(cluster_sections);

            // Name cluster: try LLM first, fallback to most representative heading
            let headings: Vec<&str> = cluster_sections
                .iter()
                .map(|s| s.heading.as_str())
                .filter(|h| {
                    !h.is_empty()
                        && *h != "results matching \" \""
                        && *h != "No results matching \" \""
                })
                .take(15)
                .collect();
            let name = if headings.is_empty() {
                let fallback = format!("Cluster {}", i + 1);
                info!(
                    cluster_idx = i,
                    "No valid headings, using fallback: {}", fallback
                );
                fallback
            } else {
                let llm_name = self.name_cluster(repo_name, &headings).await.ok();
                info!(
                    cluster_idx = i,
                    ?llm_name,
                    headings_count = headings.len(),
                    "LLM naming result"
                );
                match llm_name {
                    Some(ref n)
                        if !n.contains('{')
                            && !n.starts_with("Topic")
                            && !n.starts_with("Cluster")
                            // FINDING #1 FIX: the 3..=50 bound is a CHARACTER intent,
                            // so count chars() not String::len() (BYTES) — otherwise a
                            // valid short CJK name (e.g. 3 chars = 9 bytes) is wrongly
                            // rejected and a borderline one wrongly accepted.
                            && n.chars().count() >= 3
                            && n.chars().count() <= 50 =>
                    {
                        n.clone()
                    }
                    _ => {
                        let derived = derive_cluster_name(&headings);
                        info!(cluster_idx = i, derived_name = %derived, "Using heading-derived name");
                        derived
                    }
                }
            };

            // Disambiguate a name already used this run before storing.
            let name = unique_cluster_name(&mut used_names, name);

            // Store in PG via DocClusterRepo port
            let cluster_id = self
                .doc_cluster
                .store_cluster(repo_name, &name, cluster_sections.len() as i32, &centroid)
                .await?;

            // Update section cluster_id via DocClusterRepo port
            let section_ids: Vec<Uuid> = cluster_sections.iter().map(|s| s.id).collect();
            self.doc_cluster
                .assign_sections_to_cluster(cluster_id, &section_ids)
                .await?;

            // Store in Neo4j via DocClusterGraphRepo port
            self.doc_cluster_graph
                .create_cluster_node(cluster_id, &name, repo_name)
                .await?;

            for sid in &section_ids {
                self.doc_cluster_graph
                    .create_in_cluster_edge(*sid, cluster_id)
                    .await?;
            }

            created += 1;
        }

        info!(repo_name, clusters = created, "Doc clustering complete");
        Ok(created)
    }

    async fn clean_clusters(&self, repo_name: &str) -> Result<()> {
        self.doc_cluster.clean_clusters(repo_name).await?;
        self.doc_cluster_graph
            .delete_clusters_by_repo(repo_name)
            .await?;
        Ok(())
    }

    async fn name_cluster(&self, repo_name: &str, headings: &[&str]) -> Result<String> {
        let headings_text = headings.join("\n- ");
        let prompt = format!(
            "Given these documentation section headings from '{repo_name}':\n\
             - {headings_text}\n\n\
             Provide a concise 2-4 word topic name for this group.\n\
             Return ONLY a JSON string like: \"Topic Name\"\n\
             Example: \"Nginx Config Management\""
        );
        let r = self.llm.generate_json(&prompt).await?;
        let cleaned = akashic_llm::extract_json(&r.text);
        // Parse as JSON string, fallback to raw text
        let name: String = serde_json::from_str(cleaned)
            .unwrap_or_else(|_| cleaned.trim().trim_matches('"').to_string());
        Ok(if name.is_empty() {
            "General".into()
        } else {
            name
        })
    }
}

/// Determine optimal k for k-means: aim for ~8-15 sections per cluster.
fn determine_k(n: usize) -> usize {
    let k = n.div_ceil(8); // ceil(n/8)
    k.clamp(2, 10)
}

/// Run k-means clustering on section embeddings (pure Rust, no external crate).
/// Uses cosine similarity for assignment, k-means++ initialization.
fn cluster_embeddings(sections: &[SectionEmbedding], k: usize) -> Result<Vec<usize>> {
    let n = sections.len();
    let embeddings: Vec<&[f32]> = sections.iter().map(|s| s.embedding.as_slice()).collect();

    // k-means++ initialization: pick first centroid randomly, rest by weighted distance
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    centroids.push(embeddings[0].to_vec()); // first centroid = first point

    for _ in 1..k {
        // For each point, find min distance to existing centroids
        let dists: Vec<f32> = embeddings
            .iter()
            .map(|e| {
                centroids
                    .iter()
                    .map(|c| 1.0 - cosine_sim(e, c))
                    .fold(f32::MAX, f32::min)
            })
            .collect();
        // Pick point with max min-distance (spread out centroids)
        // FINDING #1 FIX: f32::total_cmp instead of partial_cmp(..).unwrap() so a
        // NaN/Inf distance (from a NaN/Inf embedding component) cannot panic and
        // bypass the caller's non-fatal clustering handling.
        let max_idx = dists
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap_or(0);
        centroids.push(embeddings[max_idx].to_vec());
    }

    let mut assignments = vec![0usize; n];
    let max_iter = 50;

    for _ in 0..max_iter {
        // Assign each point to nearest centroid
        let mut changed = false;
        for (i, emb) in embeddings.iter().enumerate() {
            // FINDING #1 FIX: f32::total_cmp instead of partial_cmp(..).unwrap() so a
            // NaN/Inf similarity (from a NaN/Inf embedding component) cannot panic
            // and bypass the caller's non-fatal clustering handling.
            let best = centroids
                .iter()
                .enumerate()
                .max_by(|a, b| cosine_sim(emb, a.1).total_cmp(&cosine_sim(emb, b.1)))
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            if best != assignments[i] {
                changed = true;
                assignments[i] = best;
            }
        }
        if !changed {
            break;
        }

        // Recompute centroids
        for c in &mut centroids {
            c.fill(0.0);
        }
        let mut counts = vec![0usize; k];
        for (i, emb) in embeddings.iter().enumerate() {
            let ci = assignments[i];
            counts[ci] += 1;
            for (j, &v) in emb.iter().enumerate() {
                centroids[ci][j] += v;
            }
        }
        for (ci, c) in centroids.iter_mut().enumerate() {
            if counts[ci] > 0 {
                let n = counts[ci] as f32;
                for v in c.iter_mut() {
                    *v /= n;
                }
            }
        }
    }

    Ok(assignments)
}

/// Derive a cluster name from member section headings.
/// Uses the shortest unique heading that's at least 3 chars (likely a topic title).
/// Return a name unique within `used`, suffixing " (N)" on collision, and
/// record it. Two clusters that derive the same name must not collide on
/// doc_clusters' UNIQUE(repo_name, name) — store_cluster's ON CONFLICT would
/// merge them, corrupting section_count/centroid and under-counting.
fn unique_cluster_name(used: &mut std::collections::HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{name} ({n})");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn derive_cluster_name(headings: &[&str]) -> String {
    // Filter to headings that look like topic names (3-40 chars, not generic)
    let mut candidates: Vec<&&str> = headings
        .iter()
        .filter(|h| {
            // FINDING #1 FIX: 3..=50 is a CHARACTER gate, so count chars() not
            // String::len() (BYTES); otherwise multibyte (CJK) headings of valid
            // length are mis-filtered.
            let len = h.chars().count();
            (3..=50).contains(&len)
        })
        .collect();
    // Sort by length — shorter names are usually more topic-like
    candidates.sort_by_key(|h| h.len());
    candidates
        .first()
        .map(|h| h.to_string())
        .unwrap_or_else(|| headings[0].to_string())
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-10 { 0.0 } else { dot / denom }
}

/// Compute the centroid (mean) of a set of section embeddings.
fn compute_centroid(sections: &[&SectionEmbedding]) -> Vec<f32> {
    let dim = sections[0].embedding.len();
    let mut centroid = vec![0.0f32; dim];
    for s in sections {
        for (i, &v) in s.embedding.iter().enumerate() {
            centroid[i] += v;
        }
    }
    let n = sections.len() as f32;
    for v in &mut centroid {
        *v /= n;
    }
    centroid
}

#[cfg(test)]
mod tests {
    use super::{SectionEmbedding, cluster_embeddings, derive_cluster_name, unique_cluster_name};
    use std::collections::HashSet;
    use uuid::Uuid;

    #[test]
    fn unique_cluster_name_disambiguates_collisions() {
        let mut used = HashSet::new();
        assert_eq!(unique_cluster_name(&mut used, "Auth".into()), "Auth");
        assert_eq!(unique_cluster_name(&mut used, "Auth".into()), "Auth (2)");
        assert_eq!(unique_cluster_name(&mut used, "Auth".into()), "Auth (3)");
        assert_eq!(unique_cluster_name(&mut used, "DB".into()), "DB");
    }

    fn section(embedding: Vec<f32>) -> SectionEmbedding {
        SectionEmbedding {
            id: Uuid::new_v4(),
            heading: "h".to_string(),
            embedding,
        }
    }

    // FINDING #1 regression: a NaN embedding component makes cosine_sim() (and the
    // `1.0 - cosine_sim` distances in k-means++ init) return NaN. The old
    // `partial_cmp(..).unwrap()` panicked on NaN; with f32::total_cmp the routine
    // must complete and return a well-formed assignment for every section instead
    // of unwinding past the caller's non-fatal clustering handling.
    #[test]
    fn cluster_embeddings_does_not_panic_on_nan_component() {
        let sections = vec![
            section(vec![1.0, 0.0, 0.0]),
            section(vec![0.0, 1.0, 0.0]),
            section(vec![0.0, 0.0, 1.0]),
            // NaN component anywhere must not abort the run.
            section(vec![f32::NAN, 0.5, 0.5]),
            section(vec![0.9, 0.1, 0.0]),
        ];
        let k = 2;
        let assignments = cluster_embeddings(&sections, k).expect("must not panic on NaN");
        assert_eq!(assignments.len(), sections.len());
        assert!(
            assignments.iter().all(|&c| c < k),
            "every assignment must be a valid cluster index, got {assignments:?}"
        );
    }

    // FINDING #1 regression: an Inf component is the other non-total-orderable
    // case for partial_cmp. total_cmp orders it, so the run still completes.
    #[test]
    fn cluster_embeddings_does_not_panic_on_inf_component() {
        let sections = vec![
            section(vec![1.0, 0.0]),
            section(vec![0.0, 1.0]),
            section(vec![f32::INFINITY, 0.0]),
            section(vec![0.0, f32::NEG_INFINITY]),
        ];
        let k = 2;
        let assignments = cluster_embeddings(&sections, k).expect("must not panic on Inf");
        assert_eq!(assignments.len(), sections.len());
        assert!(assignments.iter().all(|&c| c < k));
    }

    // FINDING #1 regression: the 3..=50 gate is a CHARACTER intent. With the old
    // String::len() (BYTES), an 18-character CJK heading (54 bytes) was wrongly
    // rejected as "too long" (54 > 50) even though it is comfortably within the
    // 3..=50 *character* window. The chars()-based gate must accept multibyte
    // headings purely by character count.
    #[test]
    fn derive_cluster_name_gates_on_chars_not_bytes() {
        // 3 CJK chars = 9 bytes: within 3..=50 by chars; must be selectable.
        let three_cjk = "資料庫"; // "database", 3 chars / 9 bytes
        assert_eq!(three_cjk.chars().count(), 3);
        assert_eq!(three_cjk.len(), 9);
        assert_eq!(derive_cluster_name(&[three_cjk]), three_cjk);

        // 18 CJK chars = 54 bytes: > 50 by bytes, so the OLD String::len() gate
        // would have rejected it and forced the headings[0] fallback. It is only
        // 18 chars, so the chars()-based gate must KEEP it. With it as the sole
        // candidate, derive_cluster_name returns it (proving the gate passed).
        // NOTE: the shortest-by-char *ordering* tie-break (sort_by_key on len())
        // is a separate heuristic that still sorts by bytes and was out of scope
        // for this finding, so this case deliberately uses a single heading to
        // assert only the gate behaviour, not the ordering.
        let eighteen_cjk = "設定檔案管理與部署流程的詳細說明文件"; // 18 chars / 54 bytes
        assert_eq!(eighteen_cjk.chars().count(), 18);
        assert_eq!(eighteen_cjk.len(), 54);
        assert_eq!(derive_cluster_name(&[eighteen_cjk]), eighteen_cjk);

        // A genuinely > 50-char heading is filtered out; falls back to headings[0].
        let too_long: String = "a".repeat(60);
        assert_eq!(derive_cluster_name(&[too_long.as_str()]), too_long);
    }
}
