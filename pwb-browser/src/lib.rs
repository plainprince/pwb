//! `pwb-browser` — PlainWeb layer 2: runtime + renderer.
//!
//! Opens a *plain* artifact from layer 1 or resolves native system domains (`search.plain`),
//! decompiles it back to a tree, builds DOM, computes a unified Layout Box tree, applies PSS style rules,
//! executes PlainJS QuickJS script payloads with `document` and `EventHandler` bindings, and renders directly in a native window.

pub mod builtin_sites;
pub mod bundler;
pub mod context_menu;
pub mod dom;
pub mod event_handler;
pub mod feather;
pub mod js_runtime;
pub mod layout;
pub mod pipeline;
pub mod render;
pub mod resolver;
pub mod style;
pub mod webapi;
