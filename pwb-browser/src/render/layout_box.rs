//! Per-tag box rendering (the recursive `render_layout_box` dispatcher).

mod containers;
mod render;
mod text;
mod widgets;

pub(crate) use render::render_layout_box;
