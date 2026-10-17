//! PlainWeb Bundler Utility (`pwb-browser::bundler`)
//!
//! Creates `.pwb` bundles from source directories containing `.pml`, `.pss`, and `.pjs` files.
//! Used to load the built-in `devtools` directory into the running page as a mounted DOM.

mod bundle;
mod error;
mod parser;
mod pss;

#[cfg(test)]
mod tests;

pub use bundle::bundle_from_dir;
pub use error::BundlerError;
pub use pss::apply_pss_to_dom;
