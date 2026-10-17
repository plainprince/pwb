//! The `execute_script` entry point: installs bindings and runs the user script.

use rquickjs::{CatchResultExt, Object, Value};

use super::super::core::{get_or_create_context, DomHandle, JsError};
use super::super::devtools::append_console_line;
use super::{bindings, webapis, wrapper};

/// Execute a JS script payload against a DOM handle with custom `document` bindings.
pub fn execute_script(dom_handle: &DomHandle, script: &str) -> Result<(), JsError> {
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

        let _: Value = ctx
            .eval(doc_wrapper)
            .map_err(|e| JsError::ExecutionFailed(format!("document wrapper failed: {}", e)))?;

        // 4. Execute bundled user script. Wrap it so uncaught exceptions are routed to
        //    console.error (message + stack) and re-thrown with full detail.
        let wrapped = format!(
            "(function() {{\n  try {{\n{script}\n  }} catch (e) {{\n    console.error('Uncaught: ' + (e && e.stack ? e.stack : String(e)));\n    throw e;\n  }}\n}})();"
        );
        match ctx.eval::<Value, _>(wrapped).catch(&ctx) {
            Ok(_) => Ok(()),
            Err(caught) => {
                let detail = caught.to_string();
                append_console_line(dom_handle, "error", &detail);
                Err(JsError::ExecutionFailed(detail))
            }
        }
    })
}
