//! Desktop GUI Window execution pipeline (`pwb-browser::pipeline`).

mod app;
mod devtools;
mod dom;
mod resize;
mod run;

#[cfg(test)]
mod tests;

pub use run::{build_browser_app, run_event_loop, PipelineError};

pub(crate) use devtools::apply_scroll_to_bottom;
pub(crate) use dom::tree_row_target_node;
