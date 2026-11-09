//! The `execute_script` entry point: installs bindings and runs the user script.

use rquickjs::{CatchResultExt, Object, Value};

use super::super::core::{get_or_create_context, DomHandle, ExecutionDeadline, JsError};
use super::super::errors::{drain_microtasks, eval_named, report_error};
use super::{bindings, webapis, wrapper};

/// Execute a JS script payload against a DOM handle with custom `document` bindings.
///
/// Errors are reported as `page.pjs` in the devtools console. Callers that run
/// page-authored code from another origin (an event handler, a timer) should
/// use [`execute_script_named`] so the stack names that script instead.
pub fn execute_script(dom_handle: &DomHandle, script: &str) -> Result<(), JsError> {
    execute_script_named(dom_handle, script, "page.pjs")
}

/// Like [`execute_script`], with the filename QuickJS reports in stack frames.
///
/// `filename` is not written to disk; it only labels the evaluated source
/// (`event-click-12.pjs`, `page.pjs`, `devtools.pjs`), which is what makes an
/// uncaught error's stack readable in devtools.
pub fn execute_script_named(
    dom_handle: &DomHandle,
    script: &str,
    filename: &str,
) -> Result<(), JsError> {
    let _deadline = ExecutionDeadline::arm();
    let context = get_or_create_context()?;

    context.with(|ctx| {
        let global = ctx.globals();

        bindings::install(dom_handle, &ctx, &global);
        webapis::install(dom_handle, &ctx, &global);

        // Set document object
        global
            .set("document", Object::new(ctx.clone()).unwrap())
            .unwrap();

        let initial_title = dom_handle.dom.borrow().title.clone();

        // 3. Inject JS `document` object wrapper
        let doc_wrapper = wrapper::document_wrapper(&initial_title);

        let _: Value = eval_named(&ctx, &doc_wrapper, "browser.js")
            .map_err(|e| JsError::ExecutionFailed(format!("document wrapper failed: {e}")))?;

        // 4. Run the script. The IIFE keeps top-level declarations
        //    function-scoped (as before) while starting the author's source on
        //    line 1, so reported line numbers match the page's own file.
        //    Uncaught exceptions are reported from Rust, where the full
        //    name/message/stack is available.
        let wrapped = format!("(function(){{{script}\n}})();");
        match eval_named::<Value>(&ctx, &wrapped, filename).catch(&ctx) {
            Ok(_) => {
                // Run the microtasks the script queued (promise reactions)
                // before returning, so a failing `.then()` is reported here
                // instead of surfacing minutes later from an unrelated tick.
                drain_microtasks(&ctx, dom_handle);
                Ok(())
            }
            Err(caught) => {
                let diagnostic = super::super::errors::describe(&caught);
                report_error(dom_handle, &caught);
                Err(JsError::ExecutionFailed(diagnostic.text()))
            }
        }
    })
}
