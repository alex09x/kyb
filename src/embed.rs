//! Semantic search with a small multilingual embedding model
//! (multilingual-e5-small, int8 ONNX) served locally through ONNX Runtime.
//!
//! Lexical search cannot bridge synonyms or languages: the base keeps one
//! canonical language, the questions often arrive in another, and a colloquial
//! name for a database shares no token with the entry that documents it.
//! Reranking a lexical top-K with vectors is a common pattern;
//! here the vectors also *retrieve*, because a question with no lexical anchor at all
//! gives BM25 nothing to rank — an empty result set cannot be reordered into a
//! good one. Both lists are then fused by reciprocal rank.
//!
//! Entirely optional: with no model on disk the service runs lexical-only.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// e5 models are trained with these prefixes and degrade noticeably without them.
const QUERY_PREFIX: &str = "query: ";
const PASSAGE_PREFIX: &str = "passage: ";
/// Long bodies are truncated: the head of an entry carries its topic.
const MAX_TOKENS: usize = 512;

pub struct Embedder {
    session: ort::session::Session,
    tokenizer: tokenizers::Tokenizer,
    /// BERT/XLM-R exports take a third input; others do not. Detected at load
    /// time rather than assumed, so swapping the model does not break this.
    needs_token_types: bool,
}

impl Embedder {
    pub fn load(dir: &Path) -> Result<Embedder> {
        let model = dir.join("model.onnx");
        let tok = dir.join("tokenizer.json");
        if !model.exists() || !tok.exists() {
            return Err(anyhow!("no model.onnx/tokenizer.json in {}", dir.display()));
        }
        let session = ort::session::Session::builder()
            .map_err(|e| anyhow!("ort builder: {e}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("ort opt level: {e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow!("ort threads: {e}"))?
            .commit_from_file(&model)
            .map_err(|e| anyhow!("load {}: {e}", model.display()))?;
        let mut tokenizer = tokenizers::Tokenizer::from_file(&tok)
            .map_err(|e| anyhow!("load {}: {e}", tok.display()))?;
        let truncation = tokenizers::TruncationParams {
            max_length: MAX_TOKENS,
            ..Default::default()
        };
        tokenizer
            .with_truncation(Some(truncation))
            .map_err(|e| anyhow!("truncation: {e}"))?;
        let needs_token_types =
            session.inputs().iter().any(|i| i.name() == "token_type_ids");
        Ok(Embedder { session, tokenizer, needs_token_types })
    }

    pub fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[format!("{QUERY_PREFIX}{text}")])?;
        Ok(v.pop().unwrap_or_default())
    }

    pub fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> =
            texts.iter().map(|t| format!("{PASSAGE_PREFIX}{t}")).collect();
        self.embed(&prefixed)
    }

    /// Mean-pooled, L2-normalized vectors, so cosine is a plain dot product.
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let encodings: Vec<tokenizers::Encoding> = texts
            .iter()
            .map(|t| self.tokenizer.encode(t.as_str(), true).map_err(|e| anyhow!("encode: {e}")))
            .collect::<Result<_>>()?;
        let batch = encodings.len();
        let maxlen = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(1);
        let mut ids = ndarray::Array2::<i64>::zeros((batch, maxlen));
        let mut mask = ndarray::Array2::<i64>::zeros((batch, maxlen));
        for (row, enc) in encodings.iter().enumerate() {
            for (col, (&id, &m)) in enc.get_ids().iter().zip(enc.get_attention_mask()).enumerate() {
                ids[[row, col]] = id as i64;
                mask[[row, col]] = m as i64;
            }
        }
        let ids_t = ort::value::TensorRef::from_array_view(&ids)
            .map_err(|e| anyhow!("ids tensor: {e}"))?;
        let mask_t = ort::value::TensorRef::from_array_view(&mask)
            .map_err(|e| anyhow!("mask tensor: {e}"))?;
        let types = ndarray::Array2::<i64>::zeros((batch, maxlen));
        let outputs = if self.needs_token_types {
            let types_t = ort::value::TensorRef::from_array_view(&types)
                .map_err(|e| anyhow!("types tensor: {e}"))?;
            self.session
                .run(ort::inputs![
                    "input_ids" => ids_t,
                    "attention_mask" => mask_t,
                    "token_type_ids" => types_t
                ])
                .map_err(|e| anyhow!("onnx run: {e}"))?
        } else {
            self.session
                .run(ort::inputs!["input_ids" => ids_t, "attention_mask" => mask_t])
                .map_err(|e| anyhow!("onnx run: {e}"))?
        };
        let hidden = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("extract last_hidden_state: {e}"))?;
        let hidden = hidden.into_dimensionality::<ndarray::Ix3>().context("expected 3d output")?;
        let dim = hidden.shape()[2];

        let mut out = Vec::with_capacity(batch);
        for row in 0..batch {
            let mut acc = vec![0f32; dim];
            let mut n = 0f32;
            for (col, enc) in encodings[row].get_attention_mask().iter().enumerate() {
                if *enc == 0 {
                    continue;
                }
                n += 1.0;
                for d in 0..dim {
                    acc[d] += hidden[[row, col, d]];
                }
            }
            if n > 0.0 {
                for v in acc.iter_mut() {
                    *v /= n;
                }
            }
            let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in acc.iter_mut() {
                    *v /= norm;
                }
            }
            out.push(acc);
        }
        Ok(out)
    }
}

/// Both vectors are L2-normalized, so this is their dot product.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The embedder plus a vector for every current entry.
///
/// Reranking alone cannot answer a question whose words appear nowhere in the
/// base — BM25 returns no candidates and there is nothing to reorder. So every
/// head entry is embedded and searched directly. The corpus is small (hundreds
/// of entries at most), so a brute-force scan is both simplest and fastest;
/// vectors live in memory and are rebuilt from the canon like everything else.
pub struct Semantic {
    embedder: tokio::sync::Mutex<Embedder>,
    vectors: tokio::sync::RwLock<VectorState>,
}

#[derive(Clone, Debug)]
pub struct UpdateTicket {
    key: String,
    generation: u64,
}

pub struct RebuildJob {
    docs: Vec<(UpdateTicket, String)>,
}

#[derive(Default)]
struct VectorState {
    next_generation: u64,
    expected: HashMap<String, u64>,
    vectors: Vec<(String, Vec<f32>)>,
}

impl VectorState {
    fn next_ticket(&mut self, key: &str) -> UpdateTicket {
        self.next_generation = self.next_generation.checked_add(1).expect("semantic update generation exhausted");
        let generation = self.next_generation;
        self.expected.insert(key.to_string(), generation);
        self.vectors.retain(|(stored, _)| stored != key);
        UpdateTicket { key: key.to_string(), generation }
    }

    fn install(&mut self, ticket: &UpdateTicket, vector: Vec<f32>) -> bool {
        if self.expected.get(&ticket.key) != Some(&ticket.generation) {
            return false;
        }
        match self.vectors.iter_mut().find(|(key, _)| key == &ticket.key) {
            Some(slot) => slot.1 = vector,
            None => self.vectors.push((ticket.key.clone(), vector)),
        }
        true
    }

    fn reset(&mut self, keys: impl IntoIterator<Item = String>) -> Vec<UpdateTicket> {
        self.expected.clear();
        self.vectors.clear();
        keys.into_iter().map(|key| self.next_ticket(&key)).collect()
    }
}

impl Semantic {
    pub fn load(dir: &Path) -> Result<Semantic> {
        Ok(Semantic {
            embedder: tokio::sync::Mutex::new(Embedder::load(dir)?),
            vectors: tokio::sync::RwLock::new(VectorState::default()),
        })
    }

    /// Reserve the next committed version of a key before releasing the
    /// service's writer lock. Until its embedding lands, no older vector is
    /// exposed for that key.
    pub async fn begin_upsert(&self, key: &str) -> UpdateTicket {
        self.vectors.write().await.next_ticket(key)
    }

    /// Invalidate a key before releasing the service's writer lock. Any
    /// embedding already in flight can then finish only as a no-op.
    pub async fn invalidate(&self, key: &str) {
        self.vectors.write().await.next_ticket(key);
    }

    /// Snapshot a complete rebuild while the service's writer lock is held.
    /// A concurrent write gets a newer ticket and wins even when the older
    /// rebuild finishes last.
    pub async fn begin_rebuild(&self, docs: Vec<(String, String)>) -> RebuildJob {
        let keys = docs.iter().map(|(key, _)| key.clone());
        let tickets = self.vectors.write().await.reset(keys);
        RebuildJob {
            docs: tickets.into_iter().zip(docs.into_iter().map(|(_, text)| text)).collect(),
        }
    }

    pub async fn finish_rebuild(&self, job: RebuildJob) -> Result<usize> {
        let mut installed = 0;
        // batch, so a big base does not build one giant tensor
        for chunk in job.docs.chunks(16) {
            let texts: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
            let vectors = self.embedder.lock().await.embed_passages(&texts)?;
            let mut state = self.vectors.write().await;
            for ((ticket, _), vector) in chunk.iter().zip(vectors) {
                if state.install(ticket, vector) {
                    installed += 1;
                }
            }
        }
        Ok(installed)
    }

    pub async fn finish_upsert(&self, ticket: UpdateTicket, text: &str) -> Result<bool> {
        let v = self
            .embedder
            .lock()
            .await
            .embed_passages(&[text.to_string()])?
            .pop()
            .ok_or_else(|| anyhow!("empty embedding"))?;
        Ok(self.vectors.write().await.install(&ticket, v))
    }

    /// Keys most similar to the query, best first.
    pub async fn search(&self, query: &str, top: usize) -> Result<Vec<(String, f32)>> {
        let qv = self.embedder.lock().await.embed_query(query)?;
        let state = self.vectors.read().await;
        let mut scored: Vec<(String, f32)> =
            state.vectors.iter().map(|(k, v)| (k.clone(), cosine(&qv, v))).collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(top);
        Ok(scored)
    }
}

/// Reciprocal rank fusion across two candidate lists that need not agree on
/// membership: a key present in only one list still scores from that list.
/// This is what lets a semantic-only hit enter results that lexical search
/// missed entirely, while an exact lexical match keeps its footing.
pub fn fuse_lists(
    lexical: &[String],
    semantic: &[(String, f32)],
    semantic_weight: f64,
) -> Vec<String> {
    const K: f64 = 60.0;
    let mut scores: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for (i, key) in lexical.iter().enumerate() {
        *scores.entry(key.as_str()).or_default() += (1.0 - semantic_weight) / (K + (i + 1) as f64);
    }
    for (i, (key, _)) in semantic.iter().enumerate() {
        *scores.entry(key.as_str()).or_default() += semantic_weight / (K + (i + 1) as f64);
    }
    let mut keys: Vec<(&str, f64)> = scores.into_iter().collect();
    keys.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    keys.into_iter().map(|(k, _)| k.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fusion_admits_semantic_only_candidates() {
        // "c" was never found lexically; semantics rank it first
        let order = fuse_lists(&keys(&["a", "b"]), &[("c".into(), 0.9), ("a".into(), 0.4)], 0.6);
        assert!(order.contains(&"c".to_string()), "semantic-only hit must enter results");
        assert_eq!(order[0], "a", "agreed-on hit outranks a single-list hit");
        assert!(order.iter().position(|k| k == "c") < order.iter().position(|k| k == "b"));
    }

    #[test]
    fn fusion_keeps_lexical_only_hits() {
        // an exact lexical match the model does not rate must survive
        let order = fuse_lists(&keys(&["exact"]), &[("other".into(), 0.8)], 0.6);
        assert!(order.contains(&"exact".to_string()));
    }

    #[test]
    fn fusion_without_semantics_preserves_lexical_order() {
        assert_eq!(fuse_lists(&keys(&["a", "b", "c"]), &[], 0.6), keys(&["a", "b", "c"]));
    }

    #[test]
    fn cosine_of_identical_normalized_vectors_is_one() {
        let a = vec![0.6f32, 0.8];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
        let b = vec![0.8f32, -0.6];
        assert!(cosine(&a, &b).abs() < 1e-6, "orthogonal");
    }

    #[test]
    fn late_vector_cannot_replace_a_newer_committed_version() {
        let mut state = VectorState::default();
        let old = state.next_ticket("service");
        let new = state.next_ticket("service");
        assert!(state.install(&new, vec![2.0]));
        assert!(!state.install(&old, vec![1.0]));
        assert_eq!(state.vectors, vec![("service".into(), vec![2.0])]);
    }

    #[test]
    fn delete_invalidates_an_embedding_already_in_flight() {
        let mut state = VectorState::default();
        let pending = state.next_ticket("service");
        state.next_ticket("service");
        assert!(!state.install(&pending, vec![1.0]));
        assert!(state.vectors.is_empty());
    }

    #[test]
    fn rebuild_snapshot_cannot_overwrite_a_later_write() {
        let mut state = VectorState::default();
        let rebuild = state.reset(["service".to_string()]).pop().unwrap();
        let later = state.next_ticket("service");
        assert!(state.install(&later, vec![2.0]));
        assert!(!state.install(&rebuild, vec![1.0]));
        assert_eq!(state.vectors, vec![("service".into(), vec![2.0])]);
    }



    // Runs only when a model is present: CI and dev machines stay lexical-only.
    #[test]
    fn model_understands_cross_language_synonyms() {
        let dir = std::path::Path::new("model");
        if !dir.join("model.onnx").exists() {
            eprintln!("skipping: no model in ./model");
            return;
        }
        let mut e = Embedder::load(dir).unwrap();
        let q = e.embed_query("где живёт монга").unwrap();
        let docs = e
            .embed_passages(&[
                "MongoDB runs on host-a and is the authoritative database".into(),
                "Chart pattern detector pulls Binance klines every few minutes".into(),
            ])
            .unwrap();
        let mongo = cosine(&q, &docs[0]);
        let noise = cosine(&q, &docs[1]);
        assert!(mongo > noise, "russian slang must reach the english entry: {mongo} vs {noise}");
    }
}
