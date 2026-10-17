//! QuickJS-NG JavaScript Runtime Integration & Custom `document` Bindings (`pwb-browser::js_runtime`).

mod async_ops;
mod core;
mod devtools;
mod dom_utils;
mod script;

pub use async_ops::{
    execute_event_handler, execute_event_handler_typed, log_console, poll_async_ops, pump_timers,
};
pub use core::{DomHandle, JsError};
pub use script::execute_script;

#[cfg(test)]
mod tests;
