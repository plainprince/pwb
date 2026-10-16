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
