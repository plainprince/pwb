//! The `execute_script` entry point and all QuickJS `document`/`pwb` bindings.

mod bindings;
mod execute;
mod webapis;
mod wrapper;

pub use execute::execute_script;
