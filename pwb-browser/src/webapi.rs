//! Web API support (`pwb-browser::webapi`).
//!
//! Backing machinery for the browser JS APIs:
//! - `fetch` — HTTP on a background thread (ureq); the result is queued and
//!   resolved into the JS promise on the main thread by
//!   [`crate::js_runtime::poll_async_ops`]. CORS enforcement is compiled in
//!   via the `strict-cors` cargo feature (see [`cors_validate`]).
//! - `WebSocket` — one background thread per connection (tungstenite); inbound
//!   events are queued like fetch, outbound messages are pushed through a
//!   shared per-connection queue the thread drains.
//! - `setTimeout`/`setInterval` — a main-thread timer registry pumped by
//!   [`crate::js_runtime::pump_timers`] (the callbacks are `Persistent`
//!   JS functions, so they survive across `execute_script` calls).
//! - `localStorage`/`sessionStorage` — JSON maps; `localStorage` persists to a
//!   per-bundle file.
//! - `alert`/`confirm`/`prompt` — native dialogs on macOS (NSAlert), console
//!   fallbacks elsewhere.
//! - `crypto`/`btoa`/`atob`/`performance.now` — random bytes, UUIDs, base64,
//!   and a monotonic millisecond clock.

mod network;
mod platform;
mod state;
mod storage;
mod timers;

#[cfg(test)]
mod tests;

pub use network::{spawn_fetch, ws_close, ws_open, ws_send};
pub use platform::{
    atob, btoa, performance_now, random_bytes, random_uuid, show_alert, show_confirm, show_prompt,
};
pub use state::{
    add_detached, drain_async, next_free_id, next_op_id, put_detached, take_detached, AsyncEvent,
    FetchResponse, WebApiState, WsEvent,
};
pub use storage::storage_op;
pub use timers::{
    clear_timer, next_timer_deadline, pop_due_timers, register_timer, reschedule_timer,
};
