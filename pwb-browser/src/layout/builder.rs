//! Layout tree builder: computes the LayoutBox tree from a DOM.

mod boxes;
mod entry;
mod positioning;

#[cfg(test)]
mod tests;

pub use entry::build_layout_tree;
