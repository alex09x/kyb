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
use std::path::{Path, PathBuf};

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

/// Everything that decides what a vector comes out as, other than the text
/// itself.
///
/// Weights are the obvious half: vectors from different models are not
/// comparable, a cosine between them is a number with no meaning, and a cache
/// that never invalidates would never throw the old ones out. The index would
/// quietly become a mixture, search would keep returning results, and nothing
/// would fail.
///
/// The recipe is the half that is easy to miss. The cache is keyed by the text
/// of an entry, but the model is not handed that text - it is handed the text
/// with a prefix on the front, truncated to a token budget. Change either and
/// the same key now stands for a different input, with the same weights and the
/// same fingerprint. That is the identical failure one level down, and the only
/// way to see it would be search quietly getting worse.
///
/// QUERY_PREFIX belongs here too even though no passage vector is built with it:
/// a query embedded under a different instruction is not comparable with the
/// passages it is scored against.
fn embedding_recipe() -> String {
    format!("q={QUERY_PREFIX}|p={PASSAGE_PREFIX}|max_tokens={MAX_TOKENS}")
}

fn fingerprint_of(weights: &str, recipe: &str) -> String {
    format!("{weights}|{recipe}")
}

fn model_fingerprint(dir: &Path) -> Result<String> {
    let mut parts = vec![];
    for name in ["model.onnx", "tokenizer.json"] {
        let bytes = std::fs::read(dir.join(name))?;
        let oid = git2::Oid::hash_object(git2::ObjectType::Blob, &bytes)
            .map_err(|e| anyhow!("fingerprint {name}: {e}"))?;
        parts.push(oid.to_string());
    }
    Ok(fingerprint_of(&parts.join("-"), &embedding_recipe()))
}

/// Content-addressed vector cache on disk.
///
/// Keyed by the hash of the embedded TEXT rather than by an entry key or a
/// commit sha, which makes it correct for free in three ways: a version's text
/// never changes, so an entry can never go stale; two versions with identical
/// text share one vector; and nothing has to be invalidated when an entry is
/// renamed, archived or rewritten.
///
/// It lives in the index directory because it is the same kind of thing as the
/// Tantivy index - derived, disposable, rebuildable from the canon. Deleting it
/// costs exactly one cold rebuild.
pub struct VectorCache {
    path: PathBuf,
    fingerprint: String,
    dim: usize,
    map: HashMap<[u8; 20], Vec<f32>>,
    dirty: bool,
}

const CACHE_MAGIC: &[u8; 8] = b"KYBVEC01";

pub fn text_hash(text: &str) -> [u8; 20] {
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, text.as_bytes())
        .expect("hashing a string cannot fail");
    let mut out = [0u8; 20];
    out.copy_from_slice(oid.as_bytes());
    out
}

impl VectorCache {
    /// Never fails: an unreadable, truncated or foreign cache is simply an empty
    /// one. A corrupt cache must cost a rebuild, never a start-up failure.
    pub fn open(path: PathBuf, fingerprint: String) -> VectorCache {
        let mut cache =
            VectorCache { path, fingerprint, dim: 0, map: HashMap::new(), dirty: false };
        match cache.read() {
            Ok(n) if n > 0 => eprintln!("kyb: vector cache: {n} entries reused"),
            Ok(_) => {}
            Err(e) => eprintln!("kyb: vector cache ignored ({e}); embeddings will be recomputed"),
        }
        cache
    }

    fn read(&mut self) -> Result<usize> {
        let Ok(bytes) = std::fs::read(&self.path) else { return Ok(0) };
        let mut cur = 0usize;
        let take = |cur: &mut usize, n: usize| -> Result<&[u8]> {
            let end = cur.checked_add(n).ok_or_else(|| anyhow!("truncated"))?;
            if end > bytes.len() {
                return Err(anyhow!("truncated"));
            }
            let out = &bytes[*cur..end];
            *cur = end;
            Ok(out)
        };
        if take(&mut cur, 8)? != CACHE_MAGIC {
            return Err(anyhow!("not a kyb vector cache"));
        }
        let fp_len = u32::from_le_bytes(take(&mut cur, 4)?.try_into()?) as usize;
        let fp = std::str::from_utf8(take(&mut cur, fp_len)?)?.to_string();
        if fp != self.fingerprint {
            return Err(anyhow!("built by a different embedding model"));
        }
        let dim = u32::from_le_bytes(take(&mut cur, 4)?.try_into()?) as usize;
        if dim == 0 || dim > 8192 {
            return Err(anyhow!("implausible dimension {dim}"));
        }
        self.dim = dim;
        // A half-written tail is expected: upserts append, and a crash can land
        // mid-record. Keep what parsed and drop the rest.
        while cur < bytes.len() {
            let Ok(key) = take(&mut cur, 20) else { break };
            let mut hash = [0u8; 20];
            hash.copy_from_slice(key);
            let Ok(raw) = take(&mut cur, dim * 4) else { break };
            let v = raw
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            self.map.insert(hash, v);
        }
        Ok(self.map.len())
    }

    pub fn get(&self, hash: &[u8; 20]) -> Option<Vec<f32>> {
        self.map.get(hash).cloned()
    }

    pub fn insert(&mut self, hash: [u8; 20], vector: Vec<f32>) {
        if self.dim == 0 {
            self.dim = vector.len();
        }
        if vector.len() != self.dim {
            return; // a model that changed under us; the fingerprint will catch it next start
        }
        if self.map.insert(hash, vector).is_none() {
            self.dirty = true;
        }
    }

    /// Rewrites the whole file. Cheap enough at this size (a few MB) and avoids
    /// the failure modes of partial updates.
    pub fn flush(&mut self) {
        if !self.dirty || self.dim == 0 {
            return;
        }
        let mut out = Vec::with_capacity(16 + self.map.len() * (20 + self.dim * 4));
        out.extend_from_slice(CACHE_MAGIC);
        out.extend_from_slice(&(self.fingerprint.len() as u32).to_le_bytes());
        out.extend_from_slice(self.fingerprint.as_bytes());
        out.extend_from_slice(&(self.dim as u32).to_le_bytes());
        for (hash, v) in &self.map {
            out.extend_from_slice(hash);
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        let tmp = self.path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, &out).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            eprintln!("kyb: vector cache not written ({e}); it will be rebuilt next start");
            return;
        }
        self.dirty = false;
    }
}

/// The embedder plus a vector for every current entry.
///
/// HEAD-ONLY, and deliberately so for now: one vector per key, for the version
/// in the working tree. Every consequence of that choice is downstream of this
/// struct - `search` returns keys rather than versions, the hybrid path
/// materialises a key's live document, and `/search` refuses to answer
/// `history`, `as_of` and `changed_between` semantically because no head vector
/// can stand for a past version.
///
/// Extending this to (key, sha) is the change that removes all of those
/// restrictions at once. It is not a local edit: the ticket/generation guard
/// below is keyed by key and exists to reject a stale vector landing after a
/// newer write, so versioning it needs care rather than a rename. The rebuild
/// cost also moves from one pass over the head entries to one over every
/// version in history.
///
/// Reranking alone cannot answer a question whose words appear nowhere in the
/// base — BM25 returns no candidates and there is nothing to reorder. So every
/// head entry is embedded and searched directly. The corpus is small (hundreds
/// of entries at most), so a brute-force scan is both simplest and fastest;
/// vectors live in memory and are rebuilt from the canon like everything else.
pub struct Semantic {
    embedder: tokio::sync::Mutex<Embedder>,
    vectors: tokio::sync::RwLock<VectorState>,
    cache: tokio::sync::Mutex<VectorCache>,
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
    /// `cache_path` belongs in the index directory: it is derived, disposable
    /// data of exactly the same kind as the Tantivy index.
    pub fn load(dir: &Path, cache_path: PathBuf) -> Result<Semantic> {
        let embedder = Embedder::load(dir)?;
        let fingerprint = model_fingerprint(dir)?;
        Ok(Semantic {
            embedder: tokio::sync::Mutex::new(embedder),
            vectors: tokio::sync::RwLock::new(VectorState::default()),
            cache: tokio::sync::Mutex::new(VectorCache::open(cache_path, fingerprint)),
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
        let mut hits = 0usize;
        let mut cache = self.cache.lock().await;

        // Whatever the cache already holds costs nothing: the text of a version
        // never changes, so a vector for that exact text is still the right one.
        let mut misses: Vec<(usize, &UpdateTicket, &String, [u8; 20])> = vec![];
        let mut ready: Vec<(&UpdateTicket, Vec<f32>)> = vec![];
        for (idx, (ticket, text)) in job.docs.iter().enumerate() {
            let hash = text_hash(text);
            match cache.get(&hash) {
                Some(v) => {
                    hits += 1;
                    ready.push((ticket, v));
                }
                None => misses.push((idx, ticket, text, hash)),
            }
        }
        {
            let mut state = self.vectors.write().await;
            for (ticket, vector) in ready {
                if state.install(ticket, vector) {
                    installed += 1;
                }
            }
        }

        // batch, so a big base does not build one giant tensor
        for chunk in misses.chunks(16) {
            let texts: Vec<String> = chunk.iter().map(|(_, _, text, _)| (*text).clone()).collect();
            let vectors = self.embedder.lock().await.embed_passages(&texts)?;
            let mut state = self.vectors.write().await;
            for ((_, ticket, _, hash), vector) in chunk.iter().zip(vectors) {
                cache.insert(*hash, vector.clone());
                if state.install(ticket, vector) {
                    installed += 1;
                }
            }
        }
        cache.flush();
        if hits > 0 || !misses.is_empty() {
            eprintln!("kyb: embeddings: {hits} from cache, {} computed", misses.len());
        }
        Ok(installed)
    }

    pub async fn finish_upsert(&self, ticket: UpdateTicket, text: &str) -> Result<bool> {
        let hash = text_hash(text);
        let cached = self.cache.lock().await.get(&hash);
        let v = match cached {
            Some(v) => v,
            None => {
                let v = self
                    .embedder
                    .lock()
                    .await
                    .embed_passages(&[text.to_string()])?
                    .pop()
                    .ok_or_else(|| anyhow!("empty embedding"))?;
                let mut cache = self.cache.lock().await;
                cache.insert(hash, v.clone());
                cache.flush();
                v
            }
        };
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

    /// A recorded vector for a fixed string, compared on every run.
    ///
    /// The fingerprint names the inputs that happen to be CONSTANTS - prefixes,
    /// the token budget. It cannot name what is code: mean pooling instead of
    /// CLS, the L2 normalisation, the quantisation the weights were exported
    /// with, the ORT build underneath. Change any of those and the fingerprint
    /// is unmoved while the geometry is different, and the cache goes on serving
    /// yesterday's vectors against today's queries. That is the same failure
    /// this file has now met on three separate floors, so this closes the
    /// stairwell rather than one more landing: it cannot be bypassed by adding
    /// an unnamed step, because it checks the output instead of the recipe.
    ///
    /// It fails on a legitimate model upgrade too. That is the point - the
    /// fingerprint then gets updated deliberately, instead of the mismatch being
    /// discovered later as search quietly getting worse.
    ///
    /// TWO assertions, not one. Cosine catches weights, prefix, truncation,
    /// pooling and quantisation - but cosine is scale-invariant, so it is blind
    /// to exactly one of the things worth catching: dropping the normalisation.
    /// The norm is therefore checked separately.
    ///
    /// Regenerate deliberately, never casually:
    ///     KYB_GOLDEN_REGEN=1 cargo test golden_vector -- --nocapture
    #[test]
    fn golden_vector_is_stable() {
        // Deliberately longer than MAX_TOKENS. A short string would embed
        // identically at any token budget, so the budget - one of the three
        // things the recipe claims to cover - would not be checked at all.
        let golden_text = {
            let mut t = String::from(
                "the checkout service moved from port 8080 to 9090 during the migration. ",
            );
            for i in 0..700 {
                t.push_str(&format!("clause {i} about hosts queues retention and latency. "));
            }
            t
        };
        let golden_text = golden_text.as_str();
        let dir = std::path::PathBuf::from(
            std::env::var("KYB_MODEL").unwrap_or_else(|_| "./model".into()),
        );
        if !dir.join("model.onnx").exists() {
            eprintln!("golden_vector_is_stable: no model at {}, skipped", dir.display());
            return;
        }
        let mut embedder = Embedder::load(&dir).expect("model present but unloadable");
        let got = embedder
            .embed_passages(&[golden_text.to_string()])
            .expect("embedding failed")
            .pop()
            .expect("no vector");

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden-vector.json");
        if std::env::var("KYB_GOLDEN_REGEN").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = serde_json::json!({
                "text_len": golden_text.len(),
                "recipe": embedding_recipe(),
                "dim": got.len(),
                "vector": got,
            });
            std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
            eprintln!("golden vector written to {} ({} dims)", path.display(), got.len());
            return;
        }

        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("no golden vector at {} ({e}); regenerate with KYB_GOLDEN_REGEN=1", path.display())
        });
        let want: serde_json::Value = serde_json::from_str(&raw).expect("golden vector is not JSON");
        // The recipe is already in the file; reading it turns detection into
        // diagnosis. A recipe mismatch is checked FIRST and names the component
        // that moved, because "the geometry differs" sends someone looking at
        // weights when the answer is a constant three lines away. And when the
        // recipe matches while the vector does not, that itself is the useful
        // half of the bisection: the change is in code, not in a constant.
        let recorded_recipe = want["recipe"].as_str().unwrap_or("<absent>");
        let current_recipe = embedding_recipe();
        if recorded_recipe != current_recipe {
            let moved: Vec<String> = current_recipe
                .split('|')
                .zip(recorded_recipe.split('|'))
                .filter(|(now, before)| now != before)
                .map(|(now, before)| format!("{before} -> {now}"))
                .collect();
            panic!(
                "the embedding recipe changed: {}.\n\
                 Every vector in every cache was built under the old one and is not \
                 comparable with what this build produces.\n\
                 If deliberate: regenerate with KYB_GOLDEN_REGEN=1.",
                if moved.is_empty() {
                    format!("{recorded_recipe} -> {current_recipe}")
                } else {
                    moved.join(", ")
                }
            );
        }

        let expected: Vec<f32> = want["vector"]
            .as_array()
            .expect("golden vector has no `vector`")
            .iter()
            .map(|v| v.as_f64().expect("not a number") as f32)
            .collect();

        assert_eq!(
            got.len(),
            expected.len(),
            "embedding dimension changed: {} -> {}. A different model, and every cached \
             vector is now incomparable.",
            expected.len(),
            got.len()
        );

        // Scale-invariant: catches weights, pooling and quantisation. Prefix and
        // token budget are caught exactly, above, by comparing the recipe string.
        //
        // THE THRESHOLD IS MEASURED, NOT CHOSEN. int8 kernels differ between CPU
        // architectures, so the same code over the same model file does not produce
        // the same floats everywhere. Both ends were measured on 2026-09-23 with
        // this vector, recorded on aarch64-apple-darwin:
        //
        //     noise   same code on x86_64-linux        cosine 0.999563  (4.4e-4 off)
        //     signal  mean pooling swapped for CLS     cosine 0.891519  (1.1e-1 off)
        //
        // A factor of 250 between them, so the threshold can sit far from both.
        // 0.9999 - the value this started with - is BELOW the noise floor, which is
        // why it failed on Linux CI while passing on the machine that recorded the
        // vector. 0.998 leaves 4.5x headroom over the observed spread and still
        // catches the pooling change by a factor of 54.
        let sim = cosine(&got, &expected);
        assert!(
            sim > 0.998,
            "the embedding pipeline changed: cosine with the recorded vector is {sim:.6}.\n\
             The recipe is UNCHANGED ({current_recipe}), so this is not a prefix or a token \
             budget - it is code or weights: pooling, quantisation, the model file, or the \
             ONNX runtime under it. The threshold already tolerates the spread between CPU \
             architectures, so this is a real change and not a different machine.\n\
             If that was deliberate, regenerate with KYB_GOLDEN_REGEN=1 and understand that \
             every cached vector built under the old pipeline is now incomparable."
        );

        // NOT scale-invariant, and therefore the only check that sees this one:
        // vectors are L2-normalised before they are stored, and cosine cannot
        // tell whether that step still happens.
        let norm = got.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "vectors are supposed to be L2-normalised before storage, got norm {norm:.6}. \
             Cosine similarity would never have noticed this."
        );
    }

    /// The cache is keyed by an entry's text, but the model is handed that text
    /// with a prefix and a token budget applied. Those are part of what produced
    /// the vector, so they are part of what identifies it.
    #[test]
    fn the_fingerprint_covers_the_recipe_not_only_the_weights() {
        let weights = "same-weights";
        let base = fingerprint_of(weights, &embedding_recipe());

        for changed in [
            "q=query: |p=passage: |max_tokens=1024",      // a different token budget
            "q=query: |p=represent: |max_tokens=512",     // a different passage prefix
            "q=search: |p=passage: |max_tokens=512",      // a different query prefix
        ] {
            assert_ne!(
                base,
                fingerprint_of(weights, changed),
                "changing the embedding recipe must invalidate the cache: {changed}"
            );
        }

        // and the recipe actually names the three inputs it claims to cover
        let recipe = embedding_recipe();
        assert!(recipe.contains(QUERY_PREFIX), "{recipe}");
        assert!(recipe.contains(PASSAGE_PREFIX), "{recipe}");
        assert!(recipe.contains(&MAX_TOKENS.to_string()), "{recipe}");
    }

    #[test]
    fn cache_round_trips_and_a_new_model_invalidates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.cache");
        let a = text_hash("first passage");
        let b = text_hash("second passage");

        let mut cache = VectorCache::open(path.clone(), "model-A".into());
        cache.insert(a, vec![1.0, 0.0, 0.0]);
        cache.insert(b, vec![0.0, 1.0, 0.0]);
        cache.flush();

        // same model: everything comes back
        let reopened = VectorCache::open(path.clone(), "model-A".into());
        assert_eq!(reopened.get(&a), Some(vec![1.0, 0.0, 0.0]));
        assert_eq!(reopened.get(&b), Some(vec![0.0, 1.0, 0.0]));

        // different model: vectors from two models are not comparable, and a
        // cache that never invalidates would keep serving the old ones
        let other = VectorCache::open(path.clone(), "model-B".into());
        assert_eq!(other.get(&a), None, "a cache from another model must not be reused");
        assert_eq!(other.get(&b), None);
    }

    #[test]
    fn a_damaged_cache_costs_a_rebuild_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.cache");
        let a = text_hash("passage");

        let b = text_hash("another");
        let mut cache = VectorCache::open(path.clone(), "m".into());
        cache.insert(a, vec![0.5; 8]);
        cache.insert(b, vec![0.25; 8]);
        cache.flush();

        // A crash mid-append leaves a half-written record: what parsed is kept
        // and the torn tail is dropped. Which of the two is torn depends on the
        // map's iteration order, so the assertion is about the count, not the
        // identity - a test that depends on HashMap ordering fails at random.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 7]).unwrap();
        let truncated = VectorCache::open(path.clone(), "m".into());
        let survivors =
            [truncated.get(&a), truncated.get(&b)].into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(survivors.len(), 1, "the whole record before the tear survives, the torn one does not");

        // something else entirely under that name
        std::fs::write(&path, b"this is not a vector cache at all").unwrap();
        assert_eq!(VectorCache::open(path.clone(), "m".into()).get(&a), None);

        // nothing there at all
        std::fs::remove_file(&path).unwrap();
        assert_eq!(VectorCache::open(path.clone(), "m".into()).get(&a), None);
    }

    /// The cache is keyed by the text, not by the entry: two entries that say
    /// the same thing share one vector, and renaming an entry costs nothing.
    #[test]
    fn the_cache_is_keyed_by_content() {
        assert_eq!(text_hash("same words"), text_hash("same words"));
        assert_ne!(text_hash("same words"), text_hash("same words "));
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
