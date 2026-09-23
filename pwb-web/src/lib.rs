//! `pwb-web` — PlainWeb layer 1: authoring + custom compression.
//!
//! Turns an authored site into a compact *plain* bundle that a runtime can
//! decompile perfectly. The bundle is **one file**: markup, style, and an
//! (opaque) script, with only used element/selector symbols shrunk — never
//! semantics.
//!
//! Modules:
//!   parse    — parse authored markup/style into an intermediate tree.
//!   bundle   — assemble the parsed tree + script into one plain artifact.
//!   compress — apply the lossless, reversible compression (zip + symbol map).

pub mod bundle;
pub mod bundler;
pub mod compress;
pub mod parse;
