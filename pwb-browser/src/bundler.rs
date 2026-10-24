//! PlainWeb Bundler Utility (`pwb-browser::bundler`)
//!
//! Creates `.pwb` bundles from source directories containing `.pml`, `.pss`, and
//! `.pjs` files. `bundle_from_dir` delegates to `pwb-web`, so the browser's
//! built-in `devtools` page and every other page share one PML/PSS/JS compiler.
//! `apply_pss_to_dom` remains for the runtime `pwb.loadStyle` binding.

mod bundle;
mod error;
mod pss;

#[cfg(test)]
mod tests;

pub use bundle::bundle_from_dir;
pub use error::BundlerError;
pub use pss::apply_pss_to_dom;
