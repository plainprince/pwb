//! Decentralized Indexing & Ranking engine (`pwb-node::search`).

use pwb_core::crypto::ContentId;
use pwb_core::spec::Tag;
use pwb_core::wire::Bundle;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
}

pub struct SearchEngine {
    /// Inverted index: term -> list of (ContentId, frequency)
    inverted_index: HashMap<String, Vec<(ContentId, u32)>>,
    /// Document titles mapping
    titles: HashMap<ContentId, String>,
}

impl SearchEngine {
    pub fn new() -> Self {
        Self {
            inverted_index: HashMap::new(),
            titles: HashMap::new(),
        }
    }

    pub fn index_bundle(&mut self, content_id: ContentId, bundle: &Bundle) {
        self.titles.insert(content_id, bundle.title.clone());

        let mut term_counts: HashMap<String, u32> = HashMap::new();
        extract_terms(&bundle.root_node, &mut term_counts);

        for (term, count) in term_counts {
            self.inverted_index
                .entry(term.to_lowercase())
                .or_default()
                .push((content_id, count));
        }
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let query_term = query.trim().to_lowercase();
        let mut results = Vec::new();

        if let Some(matches) = self.inverted_index.get(&query_term) {
            for (cid, count) in matches {
                let title = self.titles.get(cid).cloned().unwrap_or_default();
                let rank_score = *count as f32 * 1.5; // Simple term frequency score formula
                results.push(SearchResult {
                    content_id: *cid,
                    title,
                    rank_score,
                });
            }
        }

        results.sort_by(|a, b| b.rank_score.partial_cmp(&a.rank_score).unwrap());
        results
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
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
        let mut engine = SearchEngine::new();
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
        let mut engine = SearchEngine::new();
        engine.index_bundle(ContentId([3; 32]), &bundle_with(text_node(0, "Alpha beta")));
        assert_eq!(engine.search("  ALPHA ").len(), 1);
        assert!(engine.search("gamma").is_empty());
    }

    #[test]
    fn test_default_engine_is_empty() {
        let engine = SearchEngine::default();
        assert!(engine.search("anything").is_empty());
    }
}
