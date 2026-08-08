use crate::model::{Entry, Window, KIND_KNOWLEDGE};
use crate::store::Store;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED, STORED,
    STRING,
};
use tantivy::tokenizer::{Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use tantivy::{Index, IndexReader, IndexWriter, Order, TantivyDocument, Term};

/// Russian stemmer: bodies are mostly Russian; latin identifiers
/// (CreateOrUpdateStream, NATS) pass through unchanged after lowercasing.
const ANALYZER: &str = "ru_stem";

pub struct Fields {
    doc_id: Field,
    key: Field,
    /// Tokenized copy of the key ("nats-streams" -> "nats streams") so a plain
    /// query matches the key itself, not just the prose.
    key_text: Field,
    title: Field,
    body: Field,
    /// Exact tag terms, for filtering.
    tags: Field,
    /// Tokenized copy of the tags, so free-text search finds an entry by its
    /// topic even when the tag word never appears in the body.
    tags_text: Field,
    /// "knowledge" | "incident" — exact term, for the kind filter.
    kind: Field,
    /// Incident metadata as exact terms (filters) …
    service: Field,
    status: Field,
    severity: Field,
    hosts: Field,
    knowledge: Field,
    /// Tokenized + stored: "how did we fix X before" must be searchable.
    resolution: Field,
    /// Tokenized + stored: the "is it still happening?" check often names the
    /// exact tables/metrics people search by.
    detection: Field,
    /// Stored only (JSON): affected windows ride on the hit, nobody greps them.
    affected: Field,
    started_at: Field,
    detected_at: Field,
    mitigated_at: Field,
    resolved_at: Field,
    /// … and one tokenized copy of service+hosts+knowledge keys, so free text
    /// like "orders api" reaches an incident even when the prose never says it.
    meta_text: Field,
    sha: Field,
    committed_at: Field,
    /// Monotonic insertion order. Git commit times have 1-second granularity,
    /// so a burst of agent writes ties on committed_at and "recent" ordering
    /// becomes a coin flip; seq breaks the tie exactly.
    seq: Field,
    is_head: Field,
    updated_at: Field,
}

pub struct SearchIndex {
    pub index: Index,
    pub reader: IndexReader,
    fields: Fields,
    /// Next insertion sequence number; rebuilt by reindex(), advanced by
    /// upsert_head(). Startup always reindexes, so it is never stale.
    next_seq: AtomicU64,
}

#[derive(serde::Serialize)]
pub struct Hit {
    pub key: String,
    pub title: String,
    pub kind: String,
    pub body: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub severity: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolution: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detection: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<Window>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub started_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detected_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mitigated_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolved_at: String,
    pub sha: String,
    pub committed_at: String,
    pub is_head: bool,
    pub updated_at: String,
    pub score: f32,
}

#[derive(Default)]
pub struct SearchOpts {
    pub tags: Vec<String>,
    pub history: bool,
    pub limit: usize,
    /// Order by commit time instead of relevance ("what changed lately").
    pub recent: bool,
    /// Exact filters; None = no filter.
    pub kind: Option<String>,
    pub status: Option<String>,
    pub service: Option<String>,
}

fn text_opts() -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(ANALYZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
}

fn build_schema() -> Schema {
    let mut sb = Schema::builder();
    sb.add_text_field("doc_id", STRING | STORED);
    sb.add_text_field("key", STRING | STORED);
    sb.add_text_field("key_text", text_opts());
    sb.add_text_field("title", text_opts().set_stored());
    sb.add_text_field("body", text_opts().set_stored());
    sb.add_text_field("tags", STRING | STORED);
    sb.add_text_field("tags_text", text_opts());
    sb.add_text_field("kind", STRING | STORED);
    sb.add_text_field("service", STRING | STORED);
    sb.add_text_field("status", STRING | STORED);
    sb.add_text_field("severity", STRING | STORED);
    sb.add_text_field("hosts", STRING | STORED);
    sb.add_text_field("knowledge", STRING | STORED);
    sb.add_text_field("resolution", text_opts().set_stored());
    sb.add_text_field("detection", text_opts().set_stored());
    sb.add_text_field("affected", TextOptions::default().set_stored());
    sb.add_text_field("started_at", TextOptions::default().set_stored());
    sb.add_text_field("detected_at", TextOptions::default().set_stored());
    sb.add_text_field("mitigated_at", TextOptions::default().set_stored());
    sb.add_text_field("resolved_at", TextOptions::default().set_stored());
    sb.add_text_field("meta_text", text_opts());
    sb.add_text_field("sha", STRING | STORED);
    sb.add_i64_field("committed_at", INDEXED | STORED | FAST);
    sb.add_u64_field("seq", STORED | FAST);
    sb.add_u64_field("is_head", INDEXED | STORED | FAST);
    sb.add_text_field("updated_at", TextOptions::default().set_stored());
    sb.build()
}

fn head_doc_id(key: &str) -> String {
    format!("head/{key}")
}

fn hist_doc_id(sha: &str, key: &str) -> String {
    format!("{sha}/{key}")
}

fn iso(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

impl SearchIndex {
    pub fn open_or_create(dir: &Path) -> Result<SearchIndex> {
        std::fs::create_dir_all(dir)?;
        let schema = build_schema();
        let mmap = tantivy::directory::MmapDirectory::open(dir)?;
        let index = match Index::open_or_create(mmap, schema.clone()) {
            Ok(index) => index,
            // Schema changed since this index was written. Git is the truth and
            // startup reindexes anyway, so throwing the index away is safe.
            Err(err) => {
                eprintln!("kyb: index schema mismatch ({err}), rebuilding from git");
                std::fs::remove_dir_all(dir)?;
                std::fs::create_dir_all(dir)?;
                let mmap = tantivy::directory::MmapDirectory::open(dir)?;
                Index::open_or_create(mmap, schema.clone())?
            }
        };
        let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::Russian))
            .build();
        index.tokenizers().register(ANALYZER, analyzer);
        let f = |n: &str| schema.get_field(n).expect("field from our own schema");
        let fields = Fields {
            doc_id: f("doc_id"),
            key: f("key"),
            key_text: f("key_text"),
            title: f("title"),
            body: f("body"),
            tags: f("tags"),
            tags_text: f("tags_text"),
            kind: f("kind"),
            service: f("service"),
            status: f("status"),
            severity: f("severity"),
            hosts: f("hosts"),
            knowledge: f("knowledge"),
            resolution: f("resolution"),
            detection: f("detection"),
            affected: f("affected"),
            started_at: f("started_at"),
            detected_at: f("detected_at"),
            mitigated_at: f("mitigated_at"),
            resolved_at: f("resolved_at"),
            meta_text: f("meta_text"),
            sha: f("sha"),
            committed_at: f("committed_at"),
            seq: f("seq"),
            is_head: f("is_head"),
            updated_at: f("updated_at"),
        };
        let reader = index.reader()?;
        Ok(SearchIndex { index, reader, fields, next_seq: AtomicU64::new(1) })
    }

    pub fn writer(&self) -> Result<IndexWriter> {
        Ok(self.index.writer(50_000_000)?)
    }

    fn make_doc(
        &self,
        e: &Entry,
        sha: &str,
        committed_at: i64,
        is_head: bool,
        seq: u64,
    ) -> TantivyDocument {
        let mut d = TantivyDocument::default();
        let id = if is_head { head_doc_id(&e.key) } else { hist_doc_id(sha, &e.key) };
        d.add_text(self.fields.doc_id, id);
        d.add_text(self.fields.key, &e.key);
        d.add_text(self.fields.key_text, e.key.replace('-', " "));
        d.add_text(self.fields.title, &e.title);
        d.add_text(self.fields.body, &e.body);
        for t in &e.tags {
            d.add_text(self.fields.tags, t.to_lowercase());
        }
        d.add_text(self.fields.tags_text, e.tags.join(" ").replace('-', " "));
        d.add_text(self.fields.kind, &e.kind);
        if !e.service.is_empty() {
            d.add_text(self.fields.service, e.service.to_lowercase());
        }
        if !e.status.is_empty() {
            d.add_text(self.fields.status, &e.status);
        }
        if !e.severity.is_empty() {
            d.add_text(self.fields.severity, &e.severity);
        }
        for h in &e.hosts {
            d.add_text(self.fields.hosts, h.to_lowercase());
        }
        for k in &e.knowledge {
            d.add_text(self.fields.knowledge, k);
        }
        if !e.resolution.is_empty() {
            d.add_text(self.fields.resolution, &e.resolution);
        }
        if !e.detection.is_empty() {
            d.add_text(self.fields.detection, &e.detection);
        }
        if !e.affected.is_empty() {
            let json = serde_json::to_string(&e.affected).expect("windows always serialize");
            d.add_text(self.fields.affected, json);
        }
        for (f, v) in [
            (self.fields.started_at, &e.started_at),
            (self.fields.detected_at, &e.detected_at),
            (self.fields.mitigated_at, &e.mitigated_at),
            (self.fields.resolved_at, &e.resolved_at),
        ] {
            if !v.is_empty() {
                d.add_text(f, v);
            }
        }
        // SimpleTokenizer splits on non-alphanumerics, so orders_api and
        // orders-api-architecture both become searchable words here
        let meta = format!("{} {} {}", e.service, e.hosts.join(" "), e.knowledge.join(" "));
        if !meta.trim().is_empty() {
            d.add_text(self.fields.meta_text, meta);
        }
        d.add_text(self.fields.sha, sha);
        d.add_i64(self.fields.committed_at, committed_at);
        d.add_u64(self.fields.seq, seq);
        d.add_u64(self.fields.is_head, is_head as u64);
        d.add_text(self.fields.updated_at, &e.updated_at);
        d
    }

    /// Incremental update after a git commit: replace the head doc,
    /// append a history doc (<sha>/<key>) — history only grows.
    pub fn upsert_head(
        &self,
        w: &mut IndexWriter,
        e: &Entry,
        sha: &str,
        committed_at: i64,
    ) -> Result<()> {
        w.delete_term(Term::from_field_text(self.fields.doc_id, &head_doc_id(&e.key)));
        let seq = self.next_seq.fetch_add(1, AtomicOrdering::SeqCst);
        w.add_document(self.make_doc(e, sha, committed_at, true, seq))?;
        w.add_document(self.make_doc(e, sha, committed_at, false, seq))?;
        Ok(())
    }

    pub fn delete_head(&self, w: &mut IndexWriter, key: &str) {
        w.delete_term(Term::from_field_text(self.fields.doc_id, &head_doc_id(key)));
    }

    pub fn commit_and_reload(&self, w: &mut IndexWriter) -> Result<()> {
        w.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// Full rebuild from git: entire history, plus one "live" doc per key —
    /// the version the default search sees. Live is the working tree for keys
    /// that exist there, and the latest historical version for archived
    /// incidents/tasks (closed things stay findable). Deleted knowledge is a
    /// retraction: history-only. Returns (live, history) doc counts.
    pub fn reindex(&self, w: &mut IndexWriter, store: &Store) -> Result<(usize, usize)> {
        w.delete_all_documents()?;
        // walk_history is chronological, so seq = walk order
        let hist = store.walk_history()?;
        let mut seq = 0u64;
        let mut last: HashMap<String, (Entry, String, i64, u64)> = HashMap::new();
        for h in &hist {
            seq += 1;
            w.add_document(self.make_doc(&h.entry, &h.sha, h.committed_at, false, seq))?;
            last.insert(h.entry.key.clone(), (h.entry.clone(), h.sha.clone(), h.committed_at, seq));
        }
        let heads = store.list_head()?;
        let mut live = 0usize;
        let mut alive: HashSet<&str> = HashSet::new();
        for e in &heads {
            alive.insert(e.key.as_str());
            let (sha, at, sq) = last
                .get(&e.key)
                .map(|(_, s, a, q)| (s.clone(), *a, *q))
                .unwrap_or_default();
            w.add_document(self.make_doc(e, &sha, at, true, sq))?;
            live += 1;
        }
        for (key, (entry, sha, at, sq)) in &last {
            if alive.contains(key.as_str()) || entry.kind == KIND_KNOWLEDGE {
                continue;
            }
            w.add_document(self.make_doc(entry, sha, *at, true, *sq))?;
            live += 1;
        }
        self.next_seq.store(seq + 1, AtomicOrdering::SeqCst);
        self.commit_and_reload(w)?;
        Ok((live, hist.len()))
    }

    /// The live doc of one key (tree version, or the archived latest).
    pub fn get_live(&self, key: &str) -> Result<Option<Hit>> {
        let searcher = self.reader.searcher();
        let q = TermQuery::new(
            Term::from_field_text(self.fields.doc_id, &head_doc_id(key)),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&q, &TopDocs::with_limit(1))?;
        match top.first() {
            None => Ok(None),
            Some((score, addr)) => Ok(Some(self.to_hit(&searcher, *addr, *score)?)),
        }
    }

    pub fn search(&self, q: &str, opts: &SearchOpts) -> Result<Vec<Hit>> {
        let searcher = self.reader.searcher();
        let base: Box<dyn Query> = if q.trim().is_empty() {
            Box::new(AllQuery)
        } else {
            let mut parser = QueryParser::for_index(
                &self.index,
                vec![
                    self.fields.key_text,
                    self.fields.title,
                    self.fields.body,
                    self.fields.tags_text,
                    self.fields.meta_text,
                    self.fields.resolution,
                    self.fields.detection,
                ],
            );
            parser.set_field_boost(self.fields.title, 2.0);
            // a tag match is a strong topical signal, but weaker than the headline
            parser.set_field_boost(self.fields.tags_text, 1.5);
            parser.set_field_boost(self.fields.key_text, 1.5);
            parser.set_field_boost(self.fields.meta_text, 1.5);
            // lenient: broken query syntax from an agent must not fail the request
            let (query, _errs) = parser.parse_query_lenient(q);
            query
        };
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, base)];
        // History docs cover ALL versions (current included); head docs are the
        // HEAD slice. Filter strictly by one of the two worlds, otherwise a
        // history search duplicates the current version (head + history doc
        // sharing one sha).
        let head_val = if opts.history { 0 } else { 1 };
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_u64(self.fields.is_head, head_val),
                IndexRecordOption::Basic,
            )),
        ));
        for t in &opts.tags {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.tags, &t.to_lowercase()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let exact = [
            (self.fields.kind, opts.kind.as_deref(), false),
            (self.fields.status, opts.status.as_deref(), false),
            (self.fields.service, opts.service.as_deref(), true),
        ];
        for (field, value, fold) in exact {
            let Some(v) = value.filter(|v| !v.is_empty()) else { continue };
            let v = if fold { v.to_lowercase() } else { v.to_string() };
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(field, &v),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query = BooleanQuery::new(clauses);
        let limit = opts.limit.max(1);
        let top: Vec<(f32, tantivy::DocAddress)> = if opts.recent {
            // seq, not committed_at: git time is 1s-granular and a write burst
            // ties on it, turning "latest first" into segment-order roulette
            searcher
                .search(
                    &query,
                    &TopDocs::with_limit(limit).order_by_fast_field::<u64>("seq", Order::Desc),
                )?
                .into_iter()
                // ordering by a fast field skips scoring; report 0 rather than a fake score
                .map(|(_seq, addr)| (0.0f32, addr))
                .collect()
        } else {
            searcher.search(&query, &TopDocs::with_limit(limit))?
        };
        let mut hits = vec![];
        for (score, addr) in top {
            hits.push(self.to_hit(&searcher, addr, score)?);
        }
        Ok(hits)
    }

    fn to_hit(
        &self,
        searcher: &tantivy::Searcher,
        addr: tantivy::DocAddress,
        score: f32,
    ) -> Result<Hit> {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let text =
            |f: Field| doc.get_first(f).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let multi = |f: Field| -> Vec<String> {
            doc.get_all(f).filter_map(|v| v.as_str()).map(String::from).collect()
        };
        Ok(Hit {
            key: text(self.fields.key),
            title: text(self.fields.title),
            kind: text(self.fields.kind),
            body: text(self.fields.body),
            tags: multi(self.fields.tags),
            service: text(self.fields.service),
            hosts: multi(self.fields.hosts),
            severity: text(self.fields.severity),
            status: text(self.fields.status),
            knowledge: multi(self.fields.knowledge),
            resolution: text(self.fields.resolution),
            detection: text(self.fields.detection),
            affected: serde_json::from_str(&text(self.fields.affected)).unwrap_or_default(),
            started_at: text(self.fields.started_at),
            detected_at: text(self.fields.detected_at),
            mitigated_at: text(self.fields.mitigated_at),
            resolved_at: text(self.fields.resolved_at),
            sha: text(self.fields.sha),
            committed_at: iso(
                doc.get_first(self.fields.committed_at).and_then(|v| v.as_i64()).unwrap_or(0),
            ),
            is_head: doc.get_first(self.fields.is_head).and_then(|v| v.as_u64()).unwrap_or(0) == 1,
            updated_at: text(self.fields.updated_at),
            score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Entry;
    use rstest::rstest;

    fn entry(key: &str, title: &str, body: &str) -> Entry {
        Entry {
            key: key.into(),
            title: title.into(),
            tags: vec!["infra".into()],
            body: body.into(),
            ..Default::default()
        }
    }

    /// Mini rig: temp git repo + temp index, everything indexed.
    fn mini(docs: &[(&str, &str, &str)]) -> (SearchIndex, tempfile::TempDir, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        for (k, t, b) in docs {
            store.upsert(entry(k, t, b), "2026-07-20").unwrap();
        }
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();
        (index, data, idxd)
    }

    fn o(history: bool) -> SearchOpts {
        SearchOpts { tags: vec![], history, limit: 10, ..Default::default() }
    }

    // --- stemming/tokenization: query form vs body form ---
    // Russian pairs are the test subject here (ru_stem analyzer), data stays Russian.
    #[rstest]
    #[case("стримах", "используем стримы всегда", true)]
    #[case("стрим", "конфиг стримов переехал", true)]
    #[case("стримом", "работаем со стримами", true)]
    #[case("nats", "NATS живее всех живых", true)]
    #[case("NATS", "пишем в nats постоянно", true)]
    #[case("createorupdatestream", "вызываем CreateOrUpdateStream", true)]
    #[case("терраформе", "весь конфиг в терраформ уехал", true)]
    // snowball limitation: "деплоя" stems to "депло" while "деплой" (-й loanword) stays — no match
    #[case("деплоя", "деплой только через пайплайн", false)]
    #[case("реестра", "реестр знаний для агентов", true)]
    #[case("пайплайну", "пайплайн собирает образ", true)]
    #[case("индексы", "пересобираем индекс из гита", true)]
    #[case("докере", "собираем в докер контейнере", true)]
    #[case("знанием", "реестр знаний для агентов", true)]
    #[case("кубернетес", "у нас нет кубера", false)]
    #[case("postgres", "только clickhouse и mongo", false)]
    fn stemming_matrix(#[case] q: &str, #[case] body: &str, #[case] should_match: bool) {
        let (index, _d, _i) = mini(&[("k", "title", body)]);
        let hits = index.search(q, &o(false)).unwrap();
        assert_eq!(!hits.is_empty(), should_match, "q='{q}' vs body='{body}'");
    }

    // --- broken query syntax must never fail the search ---
    #[rstest]
    #[case("")]
    #[case("   ")]
    #[case("\"unclosed")]
    #[case("title:(((")]
    #[case("AND OR NOT")]
    #[case("a:b:c")]
    #[case("()")]
    #[case("[]")]
    #[case("*")]
    #[case("-dash")]
    #[case("+plus")]
    #[case("^boost^^")]
    #[case("word)")]
    #[case("слово)")]
    #[case("~~~")]
    fn garbage_queries_dont_crash(#[case] q: &str) {
        let (index, _d, _i) = mini(&[("k", "title", "an ordinary body")]);
        assert!(index.search(q, &o(false)).is_ok(), "failed on q={q:?}");
        assert!(index.search(q, &o(true)).is_ok(), "failed on history q={q:?}");
    }

    fn make_incident(key: &str, service: &str, status: &str) -> Entry {
        Entry {
            key: key.into(),
            title: format!("{service} incident"),
            kind: "incident".into(),
            service: service.into(),
            hosts: vec!["host-a".into()],
            severity: "high".into(),
            status: status.into(),
            knowledge: vec!["orders-api-architecture".into()],
            body: "The pipeline stalled and metrics stopped flowing.".into(),
            ..Default::default()
        }
    }

    // kind/status/service are exact filters; free text still applies on top
    #[test]
    fn incident_filters() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        store.upsert(entry("orders-api-architecture", "orders_api", "The gRPC gateway."), "2026-07-22").unwrap();
        store.upsert(make_incident("inc-2026-07-22-orders-api-oom", "orders_api", "open"), "2026-07-22").unwrap();
        store.upsert(make_incident("inc-2026-07-21-landing-gap", "web_app", "resolved"), "2026-07-22").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        let by = |kind: Option<&str>, status: Option<&str>, service: Option<&str>| {
            index
                .search("", &SearchOpts {
                    limit: 10,
                    kind: kind.map(String::from),
                    status: status.map(String::from),
                    service: service.map(String::from),
                    ..Default::default()
                })
                .unwrap()
        };
        assert_eq!(by(None, None, None).len(), 3, "no filter sees everything");
        assert_eq!(by(Some("incident"), None, None).len(), 2);
        assert_eq!(by(Some("knowledge"), None, None).len(), 1);
        assert_eq!(by(Some("incident"), Some("open"), None).len(), 1);
        assert_eq!(by(Some("incident"), None, Some("ORDERS_API")).len(), 1, "service filter is case-insensitive");
        assert_eq!(by(Some("incident"), Some("open"), Some("web_app")).len(), 0);

        // incident fields come back on the hit
        let hits = by(Some("incident"), Some("open"), None);
        let h = &hits[0];
        assert_eq!(h.kind, "incident");
        assert_eq!(h.service, "orders_api");
        assert_eq!(h.status, "open");
        assert_eq!(h.severity, "high");
        assert_eq!(h.hosts, vec!["host-a"]);
        assert_eq!(h.knowledge, vec!["orders-api-architecture"]);

        // free text reaches the incident through service/knowledge meta_text:
        // the body never says "api", the service name does
        let hits = index
            .search("orders api", &SearchOpts { limit: 10, kind: Some("incident".into()), ..Default::default() })
            .unwrap();
        assert_eq!(hits.len(), 2, "both incidents mention orders_api or link its knowledge: {:?}",
            hits.iter().map(|h| &h.key).collect::<Vec<_>>());

        // knowledge hits carry kind too, with empty incident fields
        let hits = by(Some("knowledge"), None, None);
        assert_eq!(hits[0].kind, "knowledge");
        assert!(hits[0].service.is_empty());
        assert!(hits[0].status.is_empty());
    }

    // the two deletion semantics: archived closed entries stay in the default
    // search as their latest version; deleted knowledge is a retraction and
    // drops to history-only
    #[test]
    fn archived_entries_live_in_default_search() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        store.upsert(entry("gone-note", "Wrong note", "a retracted statement"), "2026-07-20").unwrap();
        store.delete("gone-note").unwrap();
        let mut inc = make_incident("inc-2026-07-20-oom", "svc", "resolved");
        inc.resolution = "raised the memory limit".into();
        store.upsert(inc, "2026-07-20").unwrap();
        store.archive("inc-2026-07-20-oom").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        // the archived incident is a first-class default-search citizen
        let hits = index.search("memory limit", &o(false)).unwrap();
        assert_eq!(hits.len(), 1, "{:?}", hits.iter().map(|h| &h.key).collect::<Vec<_>>());
        assert_eq!(hits[0].key, "inc-2026-07-20-oom");
        assert!(hits[0].is_head);
        assert_eq!(hits[0].status, "resolved");
        assert!(index.get_live("inc-2026-07-20-oom").unwrap().is_some());

        // the retracted note is not; history still holds both
        assert!(index.search("retracted statement", &o(false)).unwrap().is_empty());
        assert_eq!(index.search("retracted statement", &o(true)).unwrap().len(), 1);
        assert!(index.get_live("gone-note").unwrap().is_none());
    }

    #[test]
    fn title_ranks_above_body() {
        let (index, _d, _i) = mini(&[
            ("in-body", "totally different", "terraform is mentioned in the body"),
            ("in-title", "terraform config", "nothing special here"),
        ]);
        let hits = index.search("terraform", &o(false)).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].key, "in-title", "title hit must rank first");
    }

    #[test]
    fn history_search_no_duplicates() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        store.upsert(entry("k", "t", "common word, first version"), "2026-07-19").unwrap();
        store.upsert(entry("k", "t", "common word, second version"), "2026-07-20").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        // history=true: exactly the versions (2), no head-world duplicate of current
        let hits = index.search("common", &o(true)).unwrap();
        assert_eq!(hits.len(), 2, "duplicates: {:?}", hits.iter().map(|h| &h.sha).collect::<Vec<_>>());
        assert!(hits.iter().all(|h| !h.is_head));
        let shas: std::collections::HashSet<_> = hits.iter().map(|h| h.sha.clone()).collect();
        assert_eq!(shas.len(), 2);
        // head: exactly one current
        let hits = index.search("common", &o(false)).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].is_head);
    }

    // --- tags: AND semantics and case-insensitivity ---
    #[test]
    fn multi_tag_and_semantics() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        let mut e = entry("k", "t", "body");
        e.tags = vec!["NATS".into(), "Infra".into()];
        store.upsert(e, "2026-07-20").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        let search_tags = |tags: &[&str]| {
            index
                .search("", &SearchOpts {
                    tags: tags.iter().map(|s| s.to_string()).collect(),
                    limit: 10,
                    ..Default::default()
                })
                .unwrap()
                .len()
        };
        assert_eq!(search_tags(&["nats"]), 1, "case must not matter");
        assert_eq!(search_tags(&["NATS", "infra"]), 1, "both tags present");
        assert_eq!(search_tags(&["nats", "prod"]), 0, "AND: prod is absent");
        assert_eq!(search_tags(&["prod"]), 0);
    }

    // A tag is a topic label; free-text search must honour it even when the
    // word never appears in the prose — otherwise you can only find a tag you
    // already guessed.
    #[test]
    fn tags_are_searchable_as_text() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        let mut e = entry("probe", "Nothing to see", "This body talks about apples and bicycles.");
        e.tags = vec!["kubernetes".into(), "multi-word-tag".into()];
        store.upsert(e, "2026-07-20").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        assert_eq!(index.search("kubernetes", &o(false)).unwrap().len(), 1, "tag must be findable");
        assert_eq!(index.search("multi word tag", &o(false)).unwrap().len(), 1, "dashed tag tokenized");
        assert!(index.search("postgres", &o(false)).unwrap().is_empty(), "no false positives");
    }

    // The key itself carries meaning; "nats streams" should reach nats-streams
    // even when the prose never spells it out.
    #[test]
    fn key_is_searchable_tokenized() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        store
            .upsert(entry("orders-api-deploy", "Shipping notes", "Ships through the pipeline."), "2026-07-20")
            .unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        assert_eq!(index.search("orders api", &o(false)).unwrap().len(), 1);
        assert_eq!(index.search("orders-api-deploy", &o(false)).unwrap().len(), 1);
    }

    #[test]
    fn sort_by_recent_overrides_relevance() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        // "shared" appears twice in the older entry, so relevance favours it
        store
            .upsert(entry("older", "Shared shared topic", "shared shared shared word"), "2026-07-19")
            .unwrap();
        store.upsert(entry("newer", "Later note", "shared word once"), "2026-07-20").unwrap();
        let mut w = index.writer().unwrap();
        index.reindex(&mut w, &store).unwrap();

        let by_score = index.search("shared", &o(false)).unwrap();
        assert_eq!(by_score[0].key, "older", "relevance should favour the denser match");

        let by_time = index
            .search("shared", &SearchOpts { tags: vec![], history: false, limit: 10, recent: true, ..Default::default() })
            .unwrap();
        assert_eq!(by_time[0].key, "newer", "recent sort must put the latest commit first");
    }

    // A burst of writes lands within one git-time second (1s granularity), so
    // committed_at ties; recent order must still be exact insertion order —
    // both incrementally and after a full reindex.
    #[test]
    fn recent_sort_stable_within_one_second() {
        let data = tempfile::tempdir().unwrap();
        let idxd = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idxd.path()).unwrap();
        let mut w = index.writer().unwrap();
        for i in 0..6 {
            let out = store.upsert(entry(&format!("burst-{i}"), "t", "same body"), &format!("2026-07-22T00:00:0{i}Z")).unwrap();
            let c = match out {
                crate::store::UpsertOutcome::Created(c) => c,
                _ => panic!(),
            };
            index.upsert_head(&mut w, &c.entry, &c.sha, c.committed_at).unwrap();
        }
        index.commit_and_reload(&mut w).unwrap();
        let opts = SearchOpts { limit: 10, recent: true, ..Default::default() };
        let keys = |index: &SearchIndex| -> Vec<String> {
            index.search("", &opts).unwrap().into_iter().map(|h| h.key).collect()
        };
        let want: Vec<String> = (0..6).rev().map(|i| format!("burst-{i}")).collect();
        assert_eq!(keys(&index), want, "incremental path");
        index.reindex(&mut w, &store).unwrap();
        assert_eq!(keys(&index), want, "after full reindex");
    }

    #[test]
    fn limit_respected() {
        let docs: Vec<(String, String, String)> = (0..7)
            .map(|i| (format!("k-{i}"), format!("title {i}"), "the same body about the service".to_string()))
            .collect();
        let refs: Vec<(&str, &str, &str)> =
            docs.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
        let (index, _d, _i) = mini(&refs);
        for limit in [1, 3, 7, 50] {
            let hits = index
                .search("service", &SearchOpts { limit, ..Default::default() })
                .unwrap();
            assert_eq!(hits.len(), limit.min(7), "limit={limit}");
        }
    }

    #[test]
    fn search_head_and_history() {
        let data = tempfile::tempdir().unwrap();
        let idx_dir = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let index = SearchIndex::open_or_create(idx_dir.path()).unwrap();
        let mut w = index.writer().unwrap();

        // v1 body is Russian on purpose — the ru-stem check below needs it
        store
            .upsert(entry("nats-streams", "NATS: CreateOrUpdateStream", "Всегда используем стримы через CreateOrUpdateStream."), "2026-07-19")
            .unwrap();
        store
            .upsert(entry("nats-streams", "NATS: CreateOrUpdateStream", "New rule: config moved to terraform."), "2026-07-20")
            .unwrap();
        let (heads, hist) = index.reindex(&mut w, &store).unwrap();
        assert_eq!(heads, 1);
        assert_eq!(hist, 2);

        let opts = |history| SearchOpts { tags: vec![], history, limit: 10, ..Default::default() };

        // russian stem: "стримах" matches "стримы" from the old version → history only
        let old_only = index.search("стримах", &opts(false)).unwrap();
        assert!(old_only.is_empty(), "old text must not be found in HEAD");
        let old_hist = index.search("стримах", &opts(true)).unwrap();
        assert_eq!(old_hist.len(), 1);
        assert!(!old_hist[0].is_head);

        // current text is found in HEAD, sha is set
        let head = index.search("terraform config", &opts(false)).unwrap();
        assert_eq!(head.len(), 1);
        assert!(head[0].is_head);
        assert_eq!(head[0].sha.len(), 40);

        // tag filter
        let tagged = index
            .search("nats", &SearchOpts { tags: vec!["infra".into()], history: false, limit: 10, recent: false, ..Default::default() })
            .unwrap();
        assert_eq!(tagged.len(), 1);
        let missing = index
            .search("nats", &SearchOpts { tags: vec!["absent".into()], history: false, limit: 10, recent: false, ..Default::default() })
            .unwrap();
        assert!(missing.is_empty());

        // incremental upsert without a full reindex
        let out = store.upsert(entry("orders-api", "orders_api deploys", "Deploys go through the pipeline only."), "2026-07-20").unwrap();
        let c = match out {
            crate::store::UpsertOutcome::Created(c) => c,
            _ => panic!(),
        };
        index.upsert_head(&mut w, &c.entry, &c.sha, c.committed_at).unwrap();
        index.commit_and_reload(&mut w).unwrap();
        let found = index.search("pipeline deploys", &opts(false)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "orders-api");
    }
}
