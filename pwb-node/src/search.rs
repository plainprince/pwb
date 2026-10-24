//! Decentralized Indexing & Ranking engine (`pwb-node::search`).
//!
//! PlainWeb search is sharded across the network: every term is mapped to a
//! 256-bit shard id ([`shard_id`]) and stored on the DHT nodes XOR-closest to
//! it (see `pwb-node::federation`). This module holds the *local* postings
//! store — the inverted index a node keeps for itself and for the terms it is
//! responsible for — plus the sharding and result-aggregation math shared by
//! every node.

use pwb_core::crypto::ContentId;
use pwb_core::dht::NodeId;
use pwb_core::net::PostingWire;
use pwb_core::spec::Tag;
use pwb_core::wire::Bundle;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::persist::atomic_write;

/// Term-frequency multiplier applied when ranking hits (title terms already
/// carry a 5× boost from [`extract_terms`]).
const RANK_WEIGHT: f32 = 1.5;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
}

#[derive(Default)]
struct Index {
    /// Inverted index: term -> list of (ContentId, term frequency).
    inverted_index: HashMap<String, Vec<(ContentId, u32)>>,
    /// Document titles mapping.
    titles: HashMap<ContentId, String>,
}

pub struct SearchEngine {
    inner: Mutex<Index>,
    /// When set, every mutation rewrites this file (atomically) so the index
    /// survives restarts.
    path: Option<PathBuf>,
}

/// Serialized form of [`Index`]. Vectors (not maps) are used so the JSON stays
/// stable and content ids serialize as plain arrays.
#[derive(Serialize, Deserialize, Default)]
struct IndexSnapshot {
    titles: Vec<TitleEntry>,
    terms: Vec<TermEntry>,
}

#[derive(Serialize, Deserialize)]
struct TitleEntry {
    content_id: ContentId,
    title: String,
}

#[derive(Serialize, Deserialize)]
struct TermEntry {
    term: String,
    postings: Vec<PostingEntry>,
}

#[derive(Serialize, Deserialize)]
struct PostingEntry {
    content_id: ContentId,
    term_frequency: u32,
}

impl IndexSnapshot {
    fn from_index(index: &Index) -> Self {
        let titles = index
            .titles
            .iter()
            .map(|(content_id, title)| TitleEntry {
                content_id: *content_id,
                title: title.clone(),
            })
            .collect();
        let terms = index
            .inverted_index
            .iter()
            .map(|(term, list)| TermEntry {
                term: term.clone(),
                postings: list
                    .iter()
                    .map(|(content_id, term_frequency)| PostingEntry {
                        content_id: *content_id,
                        term_frequency: *term_frequency,
                    })
                    .collect(),
            })
            .collect();
        Self { titles, terms }
    }

    fn into_index(self) -> Index {
        let mut index = Index::default();
        for entry in self.titles {
            index.titles.insert(entry.content_id, entry.title);
        }
        for entry in self.terms {
            index.inverted_index.insert(
                entry.term,
                entry
                    .postings
                    .into_iter()
                    .map(|posting| (posting.content_id, posting.term_frequency))
                    .collect(),
            );
        }
        index
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Index::default()),
            path: None,
        }
    }

    /// Open a disk-backed index at `path`, loading any previous snapshot.
    ///
    /// A missing or unreadable file starts an empty index; individual
    /// postings are re-learned from peers on contact, so refusing to start
    /// would make one bad file fatal.
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let index = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<IndexSnapshot>(&bytes).ok())
            .map(IndexSnapshot::into_index)
            .unwrap_or_default();
        Self {
            inner: Mutex::new(index),
            path: Some(path),
        }
    }

    /// Force a rewrite of the on-disk snapshot. Returns an error when this
    /// engine is in-memory or the write failed.
    pub fn flush(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let snapshot = {
            let index = self.inner.lock().unwrap();
            IndexSnapshot::from_index(&index)
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        atomic_write(path, &bytes)
    }

    /// Best-effort snapshot after a mutation. Callers that need to know
    /// whether state reached disk call [`Self::flush`].
    fn persist(&self) {
        let _ = self.flush();
    }

    /// The file backing this index, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Number of distinct documents (content ids) in the index.
    pub fn document_count(&self) -> usize {
        self.inner.lock().unwrap().titles.len()
    }

    /// Number of distinct terms in the inverted index.
    pub fn term_count(&self) -> usize {
        self.inner.lock().unwrap().inverted_index.len()
    }

    /// Index a whole bundle locally, returning the postings it produced so the
    /// caller can publish them to the term-responsible DHT shards.
    pub fn index_bundle(&self, content_id: ContentId, bundle: &Bundle) -> Vec<PostingWire> {
        let mut term_counts: HashMap<String, u32> = HashMap::new();
        extract_terms(&bundle.root_node, &mut term_counts);

        let postings: Vec<PostingWire> = term_counts
            .iter()
            .map(|(term, tf)| PostingWire {
                term: term.to_lowercase(),
                content_id,
                title: bundle.title.clone(),
                term_frequency: *tf,
            })
            .collect();

        {
            let mut index = self.inner.lock().unwrap();
            index.titles.insert(content_id, bundle.title.clone());
            for posting in &postings {
                Self::upsert(
                    index
                        .inverted_index
                        .entry(posting.term.clone())
                        .or_default(),
                    content_id,
                    posting.term_frequency,
                );
            }
        }
        self.persist();
        postings
    }

    /// Record a single posting received from another node's shard write. Stored
    /// for the term it names so later [`Self::postings_wire`] calls answer
    /// read queries. Re-recording the same `(term, content_id)` is idempotent
    /// (a posting always carries a document's full term frequency).
    pub fn record_posting(&self, posting: &PostingWire) {
        {
            let mut index = self.inner.lock().unwrap();
            index
                .titles
                .entry(posting.content_id)
                .or_insert_with(|| posting.title.clone());
            Self::upsert(
                index
                    .inverted_index
                    .entry(posting.term.to_lowercase())
                    .or_default(),
                posting.content_id,
                posting.term_frequency,
            );
        }
        self.persist();
    }

    /// Insert or replace a `(content_id, term_frequency)` posting in `list`.
    fn upsert(list: &mut Vec<(ContentId, u32)>, content_id: ContentId, term_frequency: u32) {
        if let Some(entry) = list.iter_mut().find(|(cid, _)| *cid == content_id) {
            entry.1 = term_frequency;
        } else {
            list.push((content_id, term_frequency));
        }
    }

    /// Record a document title without any terms (e.g. learning a title from a
    /// fetched bundle).
    pub fn record_title(&self, content_id: ContentId, title: impl Into<String>) {
        self.inner
            .lock()
            .unwrap()
            .titles
            .insert(content_id, title.into());
        self.persist();
    }

    /// The local postings list for a term, as wire postings.
    pub fn postings_wire(&self, term: &str) -> Vec<PostingWire> {
        let term = term.to_lowercase();
        let index = self.inner.lock().unwrap();
        index
            .inverted_index
            .get(&term)
            .map(|list| {
                list.iter()
                    .map(|(cid, tf)| PostingWire {
                        term: term.clone(),
                        content_id: *cid,
                        title: index.titles.get(cid).cloned().unwrap_or_default(),
                        term_frequency: *tf,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The title of a locally known document.
    pub fn title(&self, content_id: &ContentId) -> Option<String> {
        self.inner.lock().unwrap().titles.get(content_id).cloned()
    }

    /// Exact single-query lookup over the local index (the pre-federation
    /// behavior, retained for local rendering and tests).
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let query_term = query.trim().to_lowercase();
        let index = self.inner.lock().unwrap();
        let mut results = Vec::new();

        if let Some(matches) = index.inverted_index.get(&query_term) {
            for (cid, count) in matches {
                let title = index.titles.get(cid).cloned().unwrap_or_default();
                results.push(SearchResult {
                    content_id: *cid,
                    title,
                    rank_score: *count as f32 * RANK_WEIGHT,
                });
            }
        }

        results.sort_by(|a, b| b.rank_score.partial_cmp(&a.rank_score).unwrap());
        results
    }
}

/// The DHT shard coordinate for a term: its BLAKE3 digest interpreted as a
/// Kademlia [`NodeId`]. A term's postings live on the nodes XOR-closest to it.
pub fn shard_id(term: &str) -> NodeId {
    NodeId::from_bytes(ContentId::compute(term.to_lowercase().as_bytes()).0)
}

/// Split a free-text query into the distinct normalized terms to look up. The
/// same normalization as [`extract_terms`] (lowercase, strip edge punctuation).
pub fn tokenize(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

/// Merge postings gathered from all term shards into a ranked result list:
/// documents are deduplicated by content id and ranked by summed term
/// frequency (so a document matching several query terms wins).
pub fn aggregate(postings: impl IntoIterator<Item = PostingWire>) -> Vec<SearchResult> {
    let mut by_doc: HashMap<ContentId, (String, u32)> = HashMap::new();
    for posting in postings {
        let entry = by_doc
            .entry(posting.content_id)
            .or_insert_with(|| (posting.title.clone(), 0));
        if entry.0.is_empty() {
            entry.0 = posting.title.clone();
        }
        entry.1 = entry.1.saturating_add(posting.term_frequency);
    }

    let mut results: Vec<SearchResult> = by_doc
        .into_iter()
        .map(|(content_id, (title, tf))| SearchResult {
            content_id,
            title,
            rank_score: tf as f32 * RANK_WEIGHT,
        })
        .collect();
    results.sort_by(|a, b| {
        b.rank_score
            .partial_cmp(&a.rank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    results
}

/// Extract the indexed term multiset from a node tree — shared with
/// `credit`'s canonical term hashing (TODO §4.1).
pub(crate) fn extract_terms(node: &pwb_core::spec::PlainNode, counts: &mut HashMap<String, u32>) {
    if let Some(txt) = &node.text_content {
        for word in txt.split_whitespace() {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
            if !clean.is_empty() {
                *counts.entry(clean.to_string()).or_insert(0) += 1;
            }
        }
    }

    if node.tag == Tag::Title {
        if let Some(txt) = &node.text_content {
            for word in txt.split_whitespace() {
                let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
                if !clean.is_empty() {
                    *counts.entry(clean.to_string()).or_insert(0) += 5; // Title boost
                }
            }
        }
    }

    for child in &node.children {
        extract_terms(child, counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::PlainNode;

    fn text_node(id: usize, text: &str) -> PlainNode {
        let mut node = PlainNode::new(id, Tag::Text);
        node.text_content = Some(text.into());
        node
    }

    fn title_node(id: usize, text: &str) -> PlainNode {
        let mut node = PlainNode::new(id, Tag::Title);
        node.text_content = Some(text.into());
        node
    }

    fn bundle_with(root: PlainNode) -> Bundle {
        Bundle {
            version: pwb_core::wire::CURRENT_VERSION,
            title: "Doc".into(),
            author_pubkey: None,
            root_node: root,
            script_payload: None,
            components: HashMap::new(),
            modules: HashMap::new(),
        }
    }

    #[test]
    fn test_extract_terms_counts_words_and_strips_punctuation() {
        let mut counts = HashMap::new();
        let node = text_node(0, "hello, hello world! plain-web");
        extract_terms(&node, &mut counts);
        assert_eq!(counts.get("hello"), Some(&2));
        assert_eq!(counts.get("world"), Some(&1));
        // Interior hyphens are kept (trim only strips edges).
        assert_eq!(counts.get("plain-web"), Some(&1));
        assert!(!counts.contains_key(""));
    }

    #[test]
    fn test_title_terms_get_five_x_boost() {
        let mut counts = HashMap::new();
        let mut doc = PlainNode::new(0, Tag::Document);
        doc.children.push(title_node(1, "browser"));
        doc.children.push(text_node(2, "browser engine"));
        extract_terms(&doc, &mut counts);
        // Title text is counted once generically (+1) plus the title boost
        // (+5); the body adds one more for "browser".
        assert_eq!(counts.get("browser"), Some(&7));
        assert_eq!(counts.get("engine"), Some(&1));
    }

    #[test]
    fn test_extract_terms_skips_empty_and_whitespace_only_text() {
        let mut counts = HashMap::new();
        let mut doc = PlainNode::new(0, Tag::Document);
        doc.children.push(text_node(1, "   "));
        doc.children.push(text_node(2, "!!!"));
        extract_terms(&doc, &mut counts);
        assert!(counts.is_empty());
    }

    #[test]
    fn test_search_ranks_by_term_frequency() {
        let engine = SearchEngine::new();
        let low = ContentId([1; 32]);
        let high = ContentId([2; 32]);
        engine.index_bundle(low, &bundle_with(text_node(0, "rust")));
        engine.index_bundle(
            high,
            &bundle_with({
                let mut root = PlainNode::new(0, Tag::Document);
                root.children.push(title_node(1, "rust rust rust"));
                root
            }),
        );
        let results = engine.search("rust");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content_id, high, "title-boosted doc ranks first");
        assert!(results[0].rank_score > results[1].rank_score);
        assert_eq!(results[1].content_id, low);
    }

    #[test]
    fn test_search_is_case_insensitive_and_misses_unknown_terms() {
        let engine = SearchEngine::new();
        engine.index_bundle(ContentId([3; 32]), &bundle_with(text_node(0, "Alpha beta")));
        assert_eq!(engine.search("  ALPHA ").len(), 1);
        assert!(engine.search("gamma").is_empty());
    }

    #[test]
    fn test_default_engine_is_empty() {
        let engine = SearchEngine::default();
        assert!(engine.search("anything").is_empty());
    }

    #[test]
    fn test_shard_id_is_deterministic_and_case_insensitive() {
        assert_eq!(shard_id("PlainWeb"), shard_id("plainweb"));
        assert_ne!(shard_id("plainweb"), shard_id("something-else"));
    }

    #[test]
    fn test_tokenize_normalizes_and_dedupes() {
        assert_eq!(tokenize("  Hello, hello world! "), vec!["hello", "world"]);
        assert!(tokenize("   //  ").is_empty());
    }

    #[test]
    fn test_aggregate_sums_term_frequencies_and_dedupes() {
        let doc = ContentId([7; 32]);
        let other = ContentId([8; 32]);
        let postings = vec![
            PostingWire {
                term: "rust".into(),
                content_id: doc,
                title: "Rust".into(),
                term_frequency: 2,
            },
            PostingWire {
                term: "engine".into(),
                content_id: doc,
                title: "Rust".into(),
                term_frequency: 3,
            },
            PostingWire {
                term: "rust".into(),
                content_id: other,
                title: "Other".into(),
                term_frequency: 1,
            },
        ];
        let results = aggregate(postings);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content_id, doc, "multi-term match ranks first");
        assert_eq!(results[0].rank_score, 5.0 * RANK_WEIGHT);
    }

    #[test]
    fn test_record_posting_is_idempotent_and_serves_back() {
        let engine = SearchEngine::new();
        let cid = ContentId([9; 32]);
        engine.record_posting(&PostingWire {
            term: "quantum".into(),
            content_id: cid,
            title: "Q".into(),
            term_frequency: 2,
        });
        engine.record_posting(&PostingWire {
            term: "QUANTUM".into(),
            content_id: cid,
            title: "Q".into(),
            term_frequency: 3,
        });
        let postings = engine.postings_wire("quantum");
        assert_eq!(postings.len(), 1);
        assert_eq!(
            postings[0].term_frequency, 3,
            "re-record replaces, not sums"
        );
        assert_eq!(postings[0].title, "Q");
        engine.record_posting(&PostingWire {
            term: "quantum".into(),
            content_id: cid,
            title: "Q".into(),
            term_frequency: 3,
        });
        assert_eq!(engine.postings_wire("quantum")[0].term_frequency, 3);
    }

    #[test]
    fn test_index_survives_restart() {
        let dir = std::env::temp_dir().join(format!(
            "pwb-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("index.json");
        let cid = ContentId([0x2a; 32]);

        {
            let engine = SearchEngine::open(&path);
            engine.index_bundle(cid, &bundle_with(text_node(0, "durable plainweb index")));
            engine.record_title(cid, "Durable");
            engine.flush().expect("flush index");
        }

        let reopened = SearchEngine::open(&path);
        assert_eq!(reopened.title(&cid).as_deref(), Some("Durable"));
        let postings = reopened.postings_wire("durable");
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].content_id, cid);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_missing_index_file_starts_empty() {
        let engine = SearchEngine::open("/nonexistent/pwb-test/index.json");
        assert!(engine.search("anything").is_empty());
        assert!(engine.path().is_some());
    }
}
