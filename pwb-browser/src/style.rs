//! CSS layout and style matching (`pwb-browser::style`).

mod apply;
mod computed;
mod values;

#[cfg(test)]
mod tests;

pub use apply::compute_style;
pub use computed::ComputedStyle;
