//! Native OS right-click context menu (`pwb-browser::context_menu`).
//!
//! Flow: the browser dispatches a `contextmenu` DOM event to the element under
//! the cursor first. If a handler calls `event.preventDefault()`, the native
//! menu is suppressed. Otherwise we show the OS menu when the platform provides
//! one — macOS via AppKit today; other platforms are no-ops (the DOM event still
//! fires everywhere).
//!
//! The menu items ("Copy", "Inspect Element") are wired to app state by the
//! pipeline through [`set_actions`], which is called once at startup.

use std::sync::OnceLock;

/// Callbacks invoked by the native menu items, wired by the pipeline to shared
/// app state (clipboard/selection, devtools).
pub struct ContextMenuActions {
    pub on_copy: Box<dyn Fn() + Send + Sync>,
    pub on_inspect: Box<dyn Fn() + Send + Sync>,
}

static ACTIONS: OnceLock<ContextMenuActions> = OnceLock::new();

/// Install the menu actions (called once before the event loop runs).
pub fn set_actions(actions: ContextMenuActions) {
    let _ = ACTIONS.set(actions);
}

fn invoke_copy() {
    if let Some(a) = ACTIONS.get() {
        (a.on_copy)();
    }
}

fn invoke_inspect() {
    if let Some(a) = ACTIONS.get() {
        (a.on_inspect)();
    }
}

/// Show the OS context menu at `(mouse_x, mouse_y)` (window-local device
/// pixels). A no-op on platforms without a native menu.
pub fn show_context_menu(window: &winit::window::Window, mouse_x: f32, mouse_y: f32) {
    #[cfg(target_os = "macos")]
    macos::show(window, mouse_x, mouse_y);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, mouse_x, mouse_y);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{invoke_copy, invoke_inspect};
    use objc2::declare_class;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, Sel};
    use objc2::{msg_send_id, ClassType, DeclaredClass};
    use objc2_app_kit::{NSMenu, NSView};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Target object for the menu items; each method routes back into Rust.
    declare_class!(
        struct CtxTarget;

        unsafe impl ClassType for CtxTarget {
            type Super = NSObject;
            type Mutability = objc2::mutability::Mutable;
            const NAME: &'static str = "PWBContextMenuTarget";
        }

        impl DeclaredClass for CtxTarget {}

        unsafe impl CtxTarget {
            #[method(copyAction:)]
            fn copy_action(&self, _sender: &AnyObject) {
                invoke_copy();
            }

            #[method(inspectAction:)]
            fn inspect_action(&self, _sender: &AnyObject) {
                invoke_inspect();
            }
        }
    );

    pub fn show(window: &winit::window::Window, mx: f32, my: f32) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(ah) = handle.as_raw() else {
            return;
        };
        // SAFETY: the handle's view is the winit content view, alive for the
        // window's lifetime; we only use it while the popup is running.
        let view: &NSView = unsafe { &*(ah.ns_view.as_ptr() as *const NSView) };

        // winit's content view is flipped (`isFlipped` returns YES — top-left
        // origin, y-down), which matches winit's device-pixel coordinates. So
        // the point maps straight through: x and y are just scaled to points.
        let scale = window.scale_factor() as f32;
        let location = NSPoint {
            x: (mx / scale) as f64,
            y: (my / scale) as f64,
        };

        unsafe {
            let mtm =
                MainThreadMarker::new().expect("context menu must be shown on the main thread");
            let menu = NSMenu::new(mtm);
            // The menu holds its target weakly, so keep the target alive for the
            // duration of the (blocking) popup.
            let target: Retained<CtxTarget> = msg_send_id![CtxTarget::alloc(), init];

            let copy_item = menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str("Copy"),
                Some(Sel::register("copyAction:")),
                &NSString::from_str(""),
            );
            copy_item.setTarget(Some(&*target));

            let inspect_item = menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str("Inspect Element"),
                Some(Sel::register("inspectAction:")),
                &NSString::from_str(""),
            );
            inspect_item.setTarget(Some(&*target));

            menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(view));
        }
    }
}
