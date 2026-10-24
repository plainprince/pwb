//! Native OS right-click context menu (`pwb-browser::context_menu`).
//!
//! Flow: the browser dispatches a `contextmenu` DOM event to the element under
//! the cursor first. If a handler calls `event.preventDefault()`, the menu is
//! suppressed. Otherwise macOS shows the native AppKit menu, while every other
//! platform paints the in-window menu from [`inline`] at the cursor (winit has
//! no portable native popup menu).
//!
//! The menu items ("Copy", "Inspect Element") are wired to app state by the
//! pipeline through [`set_actions`], which is called once at startup. The
//! actions record the choice only; the event loop performs the clipboard write
//! using its long-lived clipboard handle.

use std::sync::OnceLock;

/// Items shown by the in-window menu, in order.
pub const ITEMS: [&str; 2] = ["Copy", "Inspect Element"];

/// Run the wired "Copy" action (records the choice for the event loop).
pub fn perform_copy() {
    invoke_copy();
}

/// Run the wired "Inspect Element" action (opens devtools pick mode).
pub fn perform_inspect() {
    invoke_inspect();
}

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
/// pixels). Other platforms use [`inline`] instead; this is a no-op there.
pub fn show_context_menu(window: &winit::window::Window, mouse_x: f32, mouse_y: f32) {
    #[cfg(target_os = "macos")]
    macos::show(window, mouse_x, mouse_y);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, mouse_x, mouse_y);
    }
}

/// In-window context menu: geometry, hit-testing, and painting.
///
/// Used on platforms where winit has no portable native popup menu. The menu
/// is painted by the browser itself (last, above page content), and a left
/// click inside it invokes the matching action; a click anywhere else closes
/// it without activating the page.
pub(crate) mod inline {
    use super::ITEMS;
    use crate::render::{draw_text_ab_glyph, RenderState, TextSelection};
    use pwb_core::spec::Color;
    use tiny_skia::{Paint, Pixmap, Rect, Transform};

    /// Menu width in device pixels.
    pub(crate) const WIDTH: f32 = 190.0;
    /// Height of one menu row.
    pub(crate) const ITEM_HEIGHT: f32 = 30.0;
    /// Vertical padding above the first and below the last row.
    pub(crate) const PADDING: f32 = 6.0;

    /// The menu rectangle for a click at `(x, y)`, clamped to the viewport so
    /// a menu opened near an edge stays fully visible.
    pub(crate) fn menu_rect(x: f32, y: f32, viewport_w: f32, viewport_h: f32) -> Rect {
        let width = WIDTH.min(viewport_w.max(1.0));
        let height = (ITEM_HEIGHT * ITEMS.len() as f32 + PADDING * 2.0).min(viewport_h.max(1.0));
        let left = x.min(viewport_w - width).max(0.0);
        let top = y.min(viewport_h - height).max(0.0);
        Rect::from_xywh(left, top, width, height)
            .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap())
    }

    /// Which item (if any) the point hits.
    pub(crate) fn hit(rect: Rect, x: f32, y: f32) -> Option<usize> {
        if x < rect.left() || x >= rect.right() || y < rect.top() || y >= rect.bottom() {
            return None;
        }
        let local = y - rect.top() - PADDING;
        if local < 0.0 {
            return None;
        }
        let index = (local / ITEM_HEIGHT) as usize;
        (index < ITEMS.len()).then_some(index)
    }

    /// Paint the menu, highlighting the row under the cursor.
    pub(crate) fn draw(pixmap: &mut Pixmap, rect: Rect, mouse_x: f32, mouse_y: f32) {
        let mut paint = Paint::default();
        // 1px border + body.
        paint.set_color_rgba8(88, 91, 112, 255);
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        let body = Rect::from_xywh(
            rect.left() + 1.0,
            rect.top() + 1.0,
            (rect.width() - 2.0).max(1.0),
            (rect.height() - 2.0).max(1.0),
        )
        .unwrap();
        paint.set_color_rgba8(49, 50, 68, 255);
        pixmap.fill_rect(body, &paint, Transform::identity(), None);

        let hovered = hit(rect, mouse_x, mouse_y);
        for (index, label) in ITEMS.iter().enumerate() {
            let top = rect.top() + PADDING + ITEM_HEIGHT * index as f32;
            if hovered == Some(index) {
                if let Some(row) = Rect::from_xywh(
                    rect.left() + 1.0,
                    top,
                    (rect.width() - 2.0).max(1.0),
                    ITEM_HEIGHT,
                ) {
                    paint.set_color_rgba8(69, 71, 90, 255);
                    pixmap.fill_rect(row, &paint, Transform::identity(), None);
                }
            }
            draw_text_ab_glyph(
                pixmap,
                label,
                rect.left() + 12.0,
                top + (ITEM_HEIGHT - 15.0) / 2.0,
                15.0,
                Color {
                    r: 205,
                    g: 214,
                    b: 244,
                    a: 255,
                },
                0,
                &TextSelection::default(),
                RenderState::identity(),
            );
        }
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

#[cfg(test)]
mod tests {
    use super::inline;

    #[test]
    fn inline_menu_clamps_to_the_viewport() {
        // Opening at the bottom-right keeps the whole menu on screen.
        let rect = inline::menu_rect(790.0, 590.0, 800.0, 600.0);
        assert_eq!(rect.right(), 800.0);
        assert_eq!(rect.bottom(), 600.0);
        // Opening at the top-left sits exactly at the cursor.
        let rect = inline::menu_rect(10.0, 20.0, 800.0, 600.0);
        assert_eq!((rect.left(), rect.top()), (10.0, 20.0));
    }

    #[test]
    fn inline_menu_paints_the_hovered_row() {
        // The pointer highlight must follow the cursor: row 0 highlighted at
        // one position, row 1 highlighted after moving down.
        let mut pixmap = tiny_skia::Pixmap::new(400, 300).unwrap();
        let rect = inline::menu_rect(20.0, 20.0, 400.0, 300.0);
        let row_top =
            |index: usize| rect.top() + inline::PADDING + inline::ITEM_HEIGHT * index as f32;
        let sample = |pixmap: &tiny_skia::Pixmap, index: usize| {
            let pixel = pixmap
                .pixel((rect.left() + 5.0) as u32, (row_top(index) + 5.0) as u32)
                .unwrap();
            (pixel.red(), pixel.green(), pixel.blue())
        };

        inline::draw(&mut pixmap, rect, rect.left() + 5.0, row_top(0) + 5.0);
        let hover = (69, 71, 90);
        let body = (49, 50, 68);
        assert_eq!(sample(&pixmap, 0), hover, "first row is highlighted");
        assert_eq!(sample(&pixmap, 1), body, "second row is not highlighted");

        inline::draw(&mut pixmap, rect, rect.left() + 5.0, row_top(1) + 5.0);
        assert_eq!(sample(&pixmap, 0), body, "highlight moved away from row 0");
        assert_eq!(sample(&pixmap, 1), hover, "row 1 is now highlighted");
    }

    #[test]
    fn inline_menu_hit_tests_each_item() {
        let rect = inline::menu_rect(100.0, 100.0, 800.0, 600.0);
        let row = |index: usize| {
            rect.top()
                + inline::PADDING
                + inline::ITEM_HEIGHT * index as f32
                + inline::ITEM_HEIGHT / 2.0
        };
        assert_eq!(inline::hit(rect, rect.left() + 20.0, row(0)), Some(0));
        assert_eq!(inline::hit(rect, rect.left() + 20.0, row(1)), Some(1));
        // Padding above the first row and points outside the menu miss.
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, rect.top() + 2.0),
            None
        );
        assert_eq!(inline::hit(rect, rect.left() - 5.0, row(0)), None);
    }
}
