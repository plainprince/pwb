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
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::Bundle;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::persist::atomic_write;

/// Extra weight for a document whose title contains the query term. Title
/// terms already carry a 5× term-frequency boost from [`extract_terms`]; this
/// is the separate, cheaper signal that the title itself matches the query.
const TITLE_MATCH_BOOST: f32 = 1.35;

/// Ranking penalty for a result with no title. An untitled document can still
/// be a legitimate page, but it should not outrank a titled one on frequency
/// alone.
const UNTITLED_PENALTY: f32 = 0.7;

/// Most results returned for one query. Ranking is bounded so a large shard or
/// a spam run cannot make the results page arbitrarily expensive to render.
pub const MAX_RESULTS: usize = 50;

/// Longest document excerpt kept per content id, in characters. Snippets are
/// attacker-controlled on the remote path and travel on every shard response,
/// so they are capped where they enter the index.
pub const MAX_SNIPPET_LEN: usize = 240;

/// Longest term kept in the index, in bytes. Terms come from bundle text and
/// from remote shard writes, so an unbounded term is an unbounded key in a
/// file that every peer can append to.
pub const MAX_TERM_LEN: usize = 64;

/// Longest document title kept in the index, in bytes. Titles travel with
/// every posting, so they are attacker-controlled on the remote path.
pub const MAX_TITLE_LEN: usize = 512;

/// Distinct terms one document may contribute. Without a cap a hostile
/// bundle is a cheap way to grow the index and the shard load forever.
pub const MAX_TERMS_PER_BUNDLE: usize = 4096;

/// Cap on the term frequency stored for one `(term, document)` pair, from any
/// source. Term frequency and the title boost are ranking inputs: a remote
/// posting claiming `u32::MAX` occurrences would otherwise outrank everything.
pub const MAX_TERM_FREQUENCY: u32 = 1024;

/// Truncate `title` to [`MAX_TITLE_LEN`] bytes on a character boundary.
fn sanitize_title(title: &str) -> String {
    let title = title.trim();
    if title.len() <= MAX_TITLE_LEN {
        return title.to_string();
    }
    let mut end = MAX_TITLE_LEN;
    while end > 0 && !title.is_char_boundary(end) {
        end -= 1;
    }
    title[..end].to_string()
}

/// Collapse whitespace and bound a document excerpt. Snippets arrive from
/// local bundles and from remote shard responses, so this is the single
/// choke point for both.
fn sanitize_snippet(snippet: &str) -> String {
    // Bound the work before normalizing: a remote message may carry up to the
    // 4 MiB message cap, while only the first few hundred characters can ever
    // be displayed.
    let capped: String = snippet.chars().take(MAX_SNIPPET_LEN * 4).collect();
    let collapsed = capped.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SNIPPET_LEN {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(MAX_SNIPPET_LEN - 1).collect();
    truncated.push('…');
    truncated
}

/// Build a readable excerpt from a bundle's visible body text. Titles are
/// skipped (they are shown separately) and the page title is used as the
/// fallback when the body has no text.
fn extract_snippet(root: &PlainNode, title: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    collect_visible_text(root, &mut parts);
    let body = parts.join(" ");
    let body = sanitize_snippet(&body);
    if body.is_empty() {
        title.trim().to_string()
    } else {
        body
    }
}

fn collect_visible_text(node: &PlainNode, parts: &mut Vec<String>) {
    if !matches!(node.tag, Tag::Title | Tag::Head | Tag::Script) {
        if let Some(text) = &node.text_content {
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        }
    }
    for child in &node.children {
        collect_visible_text(child, parts);
    }
}

/// Normalize a term for the index; `None` for anything that must not be
/// stored (empty after normalization, or longer than [`MAX_TERM_LEN`]).
fn sanitize_term(term: &str) -> Option<String> {
    let term = term.trim().to_lowercase();
    if term.is_empty() || term.len() > MAX_TERM_LEN {
        return None;
    }
    Some(term)
}

/// Add one occurrence of `term`, honouring the per-bundle term cap, the term
/// length bound, and the frequency cap. The first-seen terms win, and the
/// walk order is deterministic, so two nodes indexing the same bundle derive
/// the same counts (credit receipts hash these counts).
fn count_term(counts: &mut HashMap<String, u32>, term: &str, weight: u32) {
    let Some(term) = sanitize_term(term) else {
        return;
    };
    match counts.get_mut(&term) {
        Some(slot) => *slot = slot.saturating_add(weight).min(MAX_TERM_FREQUENCY),
        None => {
            if counts.len() >= MAX_TERMS_PER_BUNDLE {
                return;
            }
            counts.insert(term, weight.min(MAX_TERM_FREQUENCY));
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
    /// Bounded body excerpt. Empty when the document was learned only from a
    /// posting written by an old peer or by a node that never parsed the
    /// bundle; callers should fall back to the title instead of a content id.
    pub snippet: String,
}

#[derive(Default)]
struct Index {
    /// Inverted index: term -> list of (ContentId, term frequency).
    inverted_index: HashMap<String, Vec<(ContentId, u32)>>,
    /// Document titles mapping.
    titles: HashMap<ContentId, String>,
    /// Document excerpts (`content_id` -> bounded body text).
    snippets: HashMap<ContentId, String>,
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
    /// Additive since the search-quality pass; old snapshots load with no
    /// snippets and results fall back to the title.
    #[serde(default)]
    snippets: Vec<SnippetEntry>,
}

#[derive(Serialize, Deserialize)]
struct TitleEntry {
    content_id: ContentId,
    title: String,
}

#[derive(Serialize, Deserialize)]
struct SnippetEntry {
    content_id: ContentId,
    snippet: String,
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
        let snippets = index
            .snippets
            .iter()
            .map(|(content_id, snippet)| SnippetEntry {
                content_id: *content_id,
                snippet: snippet.clone(),
            })
            .collect();
        Self {
            titles,
            terms,
            snippets,
        }
    }

    fn into_index(self) -> Index {
        let mut index = Index::default();
        for entry in self.titles {
            index.titles.insert(entry.content_id, entry.title);
        }
        for entry in self.snippets {
            index
                .snippets
                .insert(entry.content_id, sanitize_snippet(&entry.snippet));
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
        let title = sanitize_title(&bundle.title);
        let snippet = sanitize_snippet(&extract_snippet(&bundle.root_node, &title));

        let postings: Vec<PostingWire> = term_counts
            .iter()
            .map(|(term, tf)| PostingWire {
                term: term.to_lowercase(),
                content_id,
                title: title.clone(),
                term_frequency: *tf,
            })
            .collect();

        {
            let mut index = self.inner.lock().unwrap();
            index.titles.insert(content_id, title);
            if !snippet.is_empty() {
                index.snippets.insert(content_id, snippet);
            }
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
    ///
    /// Every field arrives from a remote peer, so it is sanitized here rather
    /// than trusted: empty or oversized terms are dropped, the title is
    /// truncated, and the claimed frequency is capped.
    pub fn record_posting(&self, posting: &PostingWire) {
        let Some(term) = sanitize_term(&posting.term) else {
            return;
        };
        let title = sanitize_title(&posting.title);
        let term_frequency = posting.term_frequency.min(MAX_TERM_FREQUENCY);
        {
            let mut index = self.inner.lock().unwrap();
            index.titles.entry(posting.content_id).or_insert(title);
            Self::upsert(
                index.inverted_index.entry(term).or_default(),
                posting.content_id,
                term_frequency,
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
            .insert(content_id, sanitize_title(&title.into()));
        self.persist();
    }

    /// Record a bounded document excerpt, usually learned from a shard
    /// response. An existing longer excerpt is kept; an empty or oversized
    /// incoming value is sanitized first.
    pub fn record_snippet(&self, content_id: ContentId, snippet: &str) {
        self.record_snippets(std::iter::once((content_id, snippet)));
    }

    /// Record many excerpts with a single index write. A shard response can
    /// carry dozens of excerpts; persisting once per entry would rewrite the
    /// whole snapshot for every result in a query.
    pub fn record_snippets<I, S>(&self, snippets: I)
    where
        I: IntoIterator<Item = (ContentId, S)>,
        S: AsRef<str>,
    {
        let sanitized: Vec<(ContentId, String)> = snippets
            .into_iter()
            .filter_map(|(content_id, snippet)| {
                let snippet = sanitize_snippet(snippet.as_ref());
                (!snippet.is_empty()).then_some((content_id, snippet))
            })
            .collect();
        if sanitized.is_empty() {
            return;
        }

        let mut changed = false;
        {
            let mut index = self.inner.lock().unwrap();
            for (content_id, snippet) in sanitized {
                match index.snippets.get(&content_id) {
                    Some(existing) if existing.chars().count() >= snippet.chars().count() => {}
                    _ => {
                        index.snippets.insert(content_id, snippet);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.persist();
        }
    }

    /// The excerpt for one document, if known.
    pub fn snippet(&self, content_id: &ContentId) -> Option<String> {
        self.inner.lock().unwrap().snippets.get(content_id).cloned()
    }

    /// A copy of every known excerpt. Callers rank a batch of postings against
    /// this without holding the index lock.
    pub fn snippets_snapshot(&self) -> HashMap<ContentId, String> {
        self.inner.lock().unwrap().snippets.clone()
    }

    /// The local postings list for a term, as wire postings.
    pub fn postings_wire(&self, term: &str) -> Vec<PostingWire> {
        let Some(term) = sanitize_term(term) else {
            return Vec::new();
        };
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

    /// Local multi-term search over this node's index. Uses the same ranking
    /// as federated results so a local lookup and a merged network query order
    /// documents the same way.
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let (postings, snippets) = {
            let index = self.inner.lock().unwrap();
            let mut postings = Vec::new();
            for term in &terms {
                let Some(list) = index.inverted_index.get(term) else {
                    continue;
                };
                for (content_id, term_frequency) in list {
                    postings.push(PostingWire {
                        term: term.clone(),
                        content_id: *content_id,
                        title: index.titles.get(content_id).cloned().unwrap_or_default(),
                        term_frequency: *term_frequency,
                    });
                }
            }
            (postings, index.snippets.clone())
        };
        aggregate(postings, &snippets)
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

/// Merge postings gathered from all term shards into a ranked result list.
///
/// Ranking is IDF-weighted, sublinear in term frequency, and boosted when the
/// document title contains the query term:
///
/// `score = Σ (1 + ln tf) · ln(1 + N/df) · title_boost`
///
/// where `N` is the number of distinct candidate documents and `df` is the
/// number containing that term. A term present in every candidate therefore
/// contributes little, while a rare term that ties otherwise-identical
/// documents together dominates — the property raw term frequency lacked, and
/// the reason a single keyword-stuffed page used to outrank everything.
///
/// Postings replicated across shards are collapsed to one contribution per
/// `(term, document)`, and exact title+snippet clones are folded into their
/// best-ranked copy so a spam run cannot occupy several result slots with the
/// same page content. Results are capped at [`MAX_RESULTS`].
///
/// `snippets` maps content ids to excerpts learned from local indexing or
/// trusted shard responses; a missing entry yields an empty snippet and the
/// UI falls back to the title.
pub fn aggregate(
    postings: impl IntoIterator<Item = PostingWire>,
    snippets: &HashMap<ContentId, String>,
) -> Vec<SearchResult> {
    // One contribution per (term, document), keeping the highest frequency
    // reported by replicas and the first non-empty title.
    let mut unique: HashMap<(String, ContentId), (String, u32)> = HashMap::new();
    for posting in postings {
        let term = posting.term.trim().to_lowercase();
        if term.is_empty() {
            continue;
        }
        let tf = posting.term_frequency.clamp(1, MAX_TERM_FREQUENCY);
        unique
            .entry((term, posting.content_id))
            .and_modify(|(title, existing)| {
                if title.is_empty() && !posting.title.is_empty() {
                    *title = posting.title.clone();
                }
                *existing = (*existing).max(tf);
            })
            .or_insert((posting.title, tf));
    }

    let candidates: HashSet<ContentId> = unique.keys().map(|(_, cid)| *cid).collect();
    let candidate_count = candidates.len().max(1) as f32;

    // Document frequency per term across the candidate set.
    let mut term_docs: HashMap<&str, HashSet<ContentId>> = HashMap::new();
    for (term, content_id) in unique.keys() {
        term_docs
            .entry(term.as_str())
            .or_default()
            .insert(*content_id);
    }

    let mut scored: Vec<SearchResult> = Vec::new();
    let mut by_doc: HashMap<ContentId, (&str, f32)> = HashMap::new();
    for ((term, content_id), (title, tf)) in &unique {
        let df = term_docs.get(term.as_str()).map_or(1, HashSet::len) as f32;
        let idf = (1.0 + candidate_count / df).ln();
        let tf_weight = 1.0 + (*tf as f32).ln();
        let title_boost = if title_contains_term(title, term) {
            TITLE_MATCH_BOOST
        } else if title.trim().is_empty() {
            UNTITLED_PENALTY
        } else {
            1.0
        };
        let contribution = tf_weight * idf * title_boost;
        by_doc
            .entry(*content_id)
            .and_modify(|(_, score)| *score += contribution)
            .or_insert((title.as_str(), contribution));
    }

    for (content_id, (title, rank_score)) in by_doc {
        scored.push(SearchResult {
            content_id,
            title: title.to_string(),
            rank_score,
            snippet: snippets.get(&content_id).cloned().unwrap_or_default(),
        });
    }

    scored.sort_by(|a, b| {
        b.rank_score
            .partial_cmp(&a.rank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.content_id.to_hex().cmp(&b.content_id.to_hex()))
    });

    // Fold exact clones (same normalized title *and* excerpt) into the first,
    // best-ranked copy. A document without an excerpt cannot be identified as
    // a clone, so it keeps its own slot.
    let mut seen: HashSet<(String, String)> = HashSet::new();
    scored.retain(|result| {
        let snippet = result.snippet.trim().to_lowercase();
        if snippet.is_empty() {
            return true;
        }
        seen.insert((result.title.trim().to_lowercase(), snippet))
    });
    scored.truncate(MAX_RESULTS);
    scored
}

/// Whether `title` contains `term` as a whole, case-insensitive word.
fn title_contains_term(title: &str, term: &str) -> bool {
    if term.is_empty() {
        return false;
    }
    title
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case(term) || word.to_lowercase() == term)
}

/// Extract the indexed term multiset from a node tree — shared with
/// `credit`'s canonical term hashing (TODO §4.1).
///
/// Terms are bounded here rather than at the call sites: [`MAX_TERM_LEN`]
/// caps one term, [`MAX_TERMS_PER_BUNDLE`] caps how many distinct terms a
/// document may introduce, and counts saturate at [`MAX_TERM_FREQUENCY`] —
/// including the 5× title boost, which is otherwise a cheap ranking squat.
pub(crate) fn extract_terms(node: &pwb_core::spec::PlainNode, counts: &mut HashMap<String, u32>) {
    if let Some(txt) = &node.text_content {
        for word in txt.split_whitespace() {
            count_term(counts, word.trim_matches(|c: char| !c.is_alphanumeric()), 1);
        }
    }

    if node.tag == Tag::Title {
        if let Some(txt) = &node.text_content {
            for word in txt.split_whitespace() {
                count_term(counts, word.trim_matches(|c: char| !c.is_alphanumeric()), 5);
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
    fn test_aggregate_multi_term_and_replica_dedupe() {
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
            // A replicated copy from another shard must not double-count.
            PostingWire {
                term: "rust".into(),
                content_id: doc,
                title: "Rust".into(),
                term_frequency: 2,
            },
            PostingWire {
                term: "rust".into(),
                content_id: other,
                title: "Other".into(),
                term_frequency: 1,
            },
        ];
        let results = aggregate(postings, &HashMap::new());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content_id, doc, "multi-term match ranks first");
        assert!(results[0].rank_score > results[1].rank_score);
    }

    #[test]
    fn test_aggregate_weights_rare_terms_more_than_common_ones() {
        let rare = ContentId([0x31; 32]);
        let common = ContentId([0x32; 32]);
        let also_common = ContentId([0x33; 32]);
        let postings = vec![
            PostingWire {
                term: "quantum".into(),
                content_id: rare,
                title: "A page".into(),
                term_frequency: 1,
            },
            PostingWire {
                term: "web".into(),
                content_id: common,
                title: "B page".into(),
                term_frequency: 1,
            },
            PostingWire {
                term: "web".into(),
                content_id: also_common,
                title: "C page".into(),
                term_frequency: 1,
            },
        ];
        let results = aggregate(postings, &HashMap::new());
        assert_eq!(
            results[0].content_id, rare,
            "the rare term carries more IDF"
        );
    }

    #[test]
    fn test_aggregate_collapses_exact_title_and_snippet_clones() {
        let original = ContentId([0x41; 32]);
        let clone = ContentId([0x42; 32]);
        let postings = vec![
            PostingWire {
                term: "spam".into(),
                content_id: original,
                title: "Buy now".into(),
                term_frequency: 1,
            },
            PostingWire {
                term: "spam".into(),
                content_id: clone,
                title: "buy NOW".into(),
                term_frequency: 1,
            },
        ];
        let mut snippets = HashMap::new();
        snippets.insert(original, "Same copied body text".to_string());
        snippets.insert(clone, "same copied body text".to_string());
        let results = aggregate(postings, &snippets);
        assert_eq!(results.len(), 1, "one content clone owns one result slot");
    }

    #[test]
    fn test_snippet_comes_from_body_and_skips_the_title() {
        let engine = SearchEngine::new();
        let cid = ContentId([0x51; 32]);
        let mut root = PlainNode::new(0, Tag::Document);
        root.children.push(title_node(1, "Hidden Title"));
        root.children
            .push(text_node(2, "  A   real\nbody   excerpt  "));
        engine.index_bundle(cid, &bundle_with(root));

        let results = engine.search("real");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "A real body excerpt");
        assert!(!results[0].snippet.contains("Hidden Title"));
    }

    #[test]
    fn test_snippet_falls_back_to_title_and_is_bounded() {
        let engine = SearchEngine::new();
        let empty = ContentId([0x52; 32]);
        let mut bundle = bundle_with(PlainNode::new(0, Tag::Document));
        bundle.title = "Only A Title".into();
        engine.index_bundle(empty, &bundle);
        assert_eq!(
            engine.snippet(&empty).as_deref(),
            Some("Only A Title"),
            "a body-less page shows its title rather than a content id"
        );

        let long = ContentId([0x53; 32]);
        let long_text = "word ".repeat(MAX_SNIPPET_LEN);
        engine.index_bundle(long, &bundle_with(text_node(0, &long_text)));
        let snippet = engine.snippet(&long).expect("bounded snippet");
        assert!(snippet.chars().count() <= MAX_SNIPPET_LEN);
        assert!(snippet.ends_with('…'));
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
        assert_eq!(
            reopened.snippet(&cid).as_deref(),
            Some("durable plainweb index"),
            "snippets survive a restart with the postings"
        );
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

    #[test]
    fn test_remote_postings_are_sanitized() {
        let engine = SearchEngine::new();
        let cid = ContentId([0x11; 32]);

        // An empty and an oversized term are never indexed.
        engine.record_posting(&PostingWire {
            term: "".into(),
            content_id: cid,
            title: String::new(),
            term_frequency: 1,
        });
        engine.record_posting(&PostingWire {
            term: "x".repeat(MAX_TERM_LEN + 1),
            content_id: cid,
            title: String::new(),
            term_frequency: 1,
        });
        assert_eq!(engine.term_count(), 0, "bad terms never reach the index");

        // A claimed frequency is capped, so rank squatting cannot outrank
        // everything with one forged posting.
        engine.record_posting(&PostingWire {
            term: "squat".into(),
            content_id: cid,
            title: "t".into(),
            term_frequency: u32::MAX,
        });
        assert_eq!(
            engine.postings_wire("squat")[0].term_frequency,
            MAX_TERM_FREQUENCY
        );

        // A remote title is truncated to the bound (a fresh document, since a
        // title already learned for a content id is kept as-is).
        let titled = ContentId([0x12; 32]);
        let long = "a".repeat(MAX_TITLE_LEN * 2);
        engine.record_posting(&PostingWire {
            term: "titled".into(),
            content_id: titled,
            title: long,
            term_frequency: 1,
        });
        assert_eq!(
            engine.title(&titled).map(|title| title.len()),
            Some(MAX_TITLE_LEN)
        );
    }

    #[test]
    fn test_bundle_terms_and_title_are_bounded() {
        let engine = SearchEngine::new();

        // One enormous word is not a term.
        let mut counts = HashMap::new();
        extract_terms(&text_node(0, &"z".repeat(MAX_TERM_LEN * 4)), &mut counts);
        assert!(counts.is_empty());

        // A document cannot introduce more than the per-bundle distinct terms.
        let mut counts = HashMap::new();
        let words: Vec<String> = (0..MAX_TERMS_PER_BUNDLE + 100)
            .map(|n| format!("w{n}"))
            .collect();
        extract_terms(&text_node(1, &words.join(" ")), &mut counts);
        assert_eq!(counts.len(), MAX_TERMS_PER_BUNDLE);

        // Frequency (including the title boost) saturates at the cap.
        let mut counts = HashMap::new();
        let repeated = "spam ".repeat(MAX_TERM_FREQUENCY as usize);
        extract_terms(&text_node(2, &repeated), &mut counts);
        assert_eq!(counts.get("spam"), Some(&MAX_TERM_FREQUENCY));

        // Titles are truncated before they are stored.
        let cid = ContentId([0x22; 32]);
        let mut bundle = bundle_with(text_node(3, "body"));
        bundle.title = "t".repeat(MAX_TITLE_LEN * 3);
        engine.index_bundle(cid, &bundle);
        assert_eq!(
            engine.title(&cid).map(|title| title.len()),
            Some(MAX_TITLE_LEN)
        );
    }
}
