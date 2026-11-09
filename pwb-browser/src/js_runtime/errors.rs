//! Turning QuickJS exceptions into readable devtools console entries.
//!
//! Errors arrive from several places (the page script, DOM event handlers,
//! timers, fetch/WebSocket callbacks). All of them funnel through
//! [`report_error`] so the devtools console always shows the same shape:
//!
//! ```text
//! Uncaught TypeError: x is not a function
//!     at handler (event-click-12.pjs:3:9)
//!     at <eval> (page.pjs:18:5)
//! ```
//!
//! The scripts are evaluated with a filename (`EvalOptions::filename`), so the
//! frames point at the page/handler that actually failed instead of the opaque
//! `eval_script` QuickJS uses by default.

use crate::js_runtime::core::DomHandle;
use crate::js_runtime::devtools::append_console_lines;
use rquickjs::{CaughtError, Ctx, Exception, Runtime, Value};
use std::cell::RefCell;

thread_local! {
    /// Rejections reported by QuickJS's promise tracker while jobs run. Drained
    /// into the devtools console once the microtask queue is empty, so an
    /// unhandled rejection is reported (and not lost with a dropped promise).
    static UNHANDLED_REJECTIONS: RefCell<Vec<JsDiagnostic>> = const { RefCell::new(Vec::new()) };
}

/// Install the host promise-rejection tracker on a fresh runtime.
///
/// QuickJS does not print a warning for a rejected promise nobody awaits, so
/// without this an `async` failure is silent. The tracker only records the
/// diagnostic; [`drain_microtasks`] flushes it to devtools after the microtask
/// checkpoint, which is when browsers consider a rejection unhandled.
pub(crate) fn install_rejection_tracker(runtime: &Runtime) {
    runtime.set_host_promise_rejection_tracker(Some(Box::new(
        |_ctx, _promise, reason, handled| {
            if handled {
                return;
            }
            let diagnostic = describe_value(&reason);
            UNHANDLED_REJECTIONS.with(|queue| queue.borrow_mut().push(diagnostic));
        },
    )));
}

/// Drop rejections recorded against a page that is being torn down, so a new
/// page never inherits the previous one's errors.
pub(crate) fn clear_rejections() {
    UNHANDLED_REJECTIONS.with(|queue| queue.borrow_mut().clear());
}

/// Flush recorded unhandled rejections to the devtools console.
fn report_rejections(dom_handle: &DomHandle) {
    let pending: Vec<JsDiagnostic> =
        UNHANDLED_REJECTIONS.with(|queue| queue.borrow_mut().drain(..).collect());
    for diagnostic in pending {
        eprintln!("[JS] {}", diagnostic.summary);
        for frame in &diagnostic.frames {
            eprintln!("[JS] {frame}");
        }
        let mut lines = vec![(diagnostic.summary.clone(), "error")];
        lines.extend(
            diagnostic
                .frames
                .iter()
                .map(|frame| (frame.clone(), "trace")),
        );
        append_console_lines(dom_handle, &lines);
    }
}

/// A normalized JS error: a one-line summary plus stack frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsDiagnostic {
    pub summary: String,
    pub frames: Vec<String>,
}

impl JsDiagnostic {
    /// The full text (summary first, then indented frames).
    pub(crate) fn text(&self) -> String {
        let mut out = self.summary.clone();
        for frame in &self.frames {
            out.push('\n');
            out.push_str(frame);
        }
        out
    }
}

/// Build a [`JsDiagnostic`] from a caught QuickJS error. Non-`Error` throws
/// (`throw "boom"`) and internal engine failures are described too, instead of
/// degrading to a Rust debug string.
pub(crate) fn describe(caught: &CaughtError<'_>) -> JsDiagnostic {
    match caught {
        CaughtError::Exception(ex) => from_exception(ex, "Uncaught "),
        CaughtError::Value(value) => JsDiagnostic {
            summary: format!("Uncaught {}", value_debug(value)),
            frames: Vec::new(),
        },
        CaughtError::Error(error) => JsDiagnostic {
            summary: format!("JavaScript engine error: {error}"),
            frames: Vec::new(),
        },
    }
}

/// A rejection reason: an `Error` object gets its name/message/stack, while a
/// plain value (`throw 'nope'`, `Promise.reject(42)`) is described inline.
fn describe_value(value: &Value<'_>) -> JsDiagnostic {
    if let Some(object) = value.as_object().cloned() {
        if let Some(exception) = Exception::from_object(object) {
            return from_exception(&exception, "Uncaught (in promise) ");
        }
    }
    JsDiagnostic {
        summary: format!("Uncaught (in promise) {}", value_debug(value)),
        frames: Vec::new(),
    }
}

fn from_exception(ex: &Exception<'_>, prefix: &str) -> JsDiagnostic {
    let name = ex
        .get::<_, Option<String>>("name")
        .ok()
        .flatten()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Error".to_string());
    let message = ex
        .message()
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| "(no message)".to_string());
    let summary = if message.starts_with(&name) {
        format!("{prefix}{message}")
    } else {
        format!("{prefix}{name}: {message}")
    };
    let frames = ex
        .stack()
        .map(|stack| stack_frames(&stack))
        .unwrap_or_default();
    JsDiagnostic { summary, frames }
}

/// `String(value)`-ish rendering for thrown non-Error values.
fn value_debug(value: &rquickjs::Value<'_>) -> String {
    if let Some(string) = value.as_string() {
        return string
            .to_string()
            .unwrap_or_else(|_| "<string>".to_string());
    }
    if let Some(number) = value.as_int() {
        return number.to_string();
    }
    if let Some(float) = value.as_float() {
        return float.to_string();
    }
    if value.is_null() {
        return "null".to_string();
    }
    if value.is_undefined() {
        return "undefined".to_string();
    }
    if value.is_bool() {
        return value.as_bool().unwrap_or(false).to_string();
    }
    "<value>".to_string()
}

/// Split a QuickJS stack into indented frames, dropping the leading
/// `Name: message` line and blank lines.
fn stack_frames(stack: &str) -> Vec<String> {
    stack
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter(|line| line.trim_start().starts_with("at "))
        .map(|line| format!("    {}", line.trim_start()))
        .collect()
}

/// Report a caught error: one `[JS]` line on stderr and a console entry in the
/// devtools panel (a hidden panel still records it, so opening devtools later
/// shows what failed).
pub(crate) fn report_error(dom_handle: &DomHandle, caught: &CaughtError<'_>) {
    let diagnostic = describe(caught);
    eprintln!("[JS] {}", diagnostic.summary);
    for frame in &diagnostic.frames {
        eprintln!("[JS] {frame}");
    }
    let mut lines = vec![(diagnostic.summary.clone(), "error")];
    lines.extend(
        diagnostic
            .frames
            .iter()
            .map(|frame| (frame.clone(), "trace")),
    );
    append_console_lines(dom_handle, &lines);
}

/// Run queued promise reactions / jobs, reporting any exception they throw.
///
/// QuickJS keeps a microtask queue per runtime; an exception inside a
/// `.then()` callback would otherwise be invisible. The caller decides when a
/// checkpoint happens (right after a script, after a timer, after an async
/// event), matching the JavaScript event loop.
///
/// Returns whether at least one job ran.
pub(crate) fn drain_microtasks(ctx: &Ctx<'_>, dom_handle: &DomHandle) -> bool {
    let mut ran = false;
    while ctx.execute_pending_job() {
        ran = true;
        if ctx.has_exception() {
            let caught = CaughtError::from_error(ctx, rquickjs::Error::Exception);
            report_error(dom_handle, &caught);
        }
    }
    report_rejections(dom_handle);
    ran
}

/// Evaluate `source` with a filename so stack frames name the script. Used by
/// every entry point that runs page-authored code.
pub(crate) fn eval_named<'js, V: rquickjs::FromJs<'js>>(
    ctx: &Ctx<'js>,
    source: &str,
    filename: &str,
) -> rquickjs::Result<V> {
    let mut options = rquickjs::context::EvalOptions::default();
    options.filename = Some(filename.to_string());
    ctx.eval_with_options(source, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_frames_keep_only_call_sites() {
        let stack = "TypeError: nope\n    at foo (page.pjs:3:9)\n\n    at <eval> (page.pjs:9:1)";
        assert_eq!(
            stack_frames(stack),
            vec![
                "    at foo (page.pjs:3:9)".to_string(),
                "    at <eval> (page.pjs:9:1)".to_string(),
            ]
        );
    }
}
