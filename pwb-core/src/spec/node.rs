//! The PlainWeb AST element node.

use std::collections::HashMap;

use super::style::StyleProperty;
use super::tag::Tag;

/// PlainWeb Node (AST element tree node).
#[derive(Debug, Clone, PartialEq)]
pub struct PlainNode {
    pub id: usize,
    pub tag: Tag,
    pub attributes: HashMap<String, String>,
    pub styles: Vec<StyleProperty>,
    pub text_content: Option<String>,
    pub children: Vec<PlainNode>,
}

impl PlainNode {
    pub fn new(id: usize, tag: Tag) -> Self {
        Self {
            id,
            tag,
            attributes: HashMap::new(),
            styles: Vec::new(),
            text_content: None,
            children: Vec::new(),
        }
    }
}
