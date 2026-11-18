//! Right-click context menu (`pwb-browser::context_menu`).
//!
//! Flow: the browser dispatches a `contextmenu` DOM event to the element under
//! the cursor first. If a handler calls `event.preventDefault()`, the menu is
//! suppressed. Otherwise the menu is shown natively on macOS (AppKit) and
//! Windows (Win32 `TrackPopupMenu`). Linux window systems expose no popup-menu
//! API to winit applications without linking a desktop toolkit, so there the
//! browser paints the same menu in-window and resolves the next click itself.
//!
//! The item list is built from a [`MenuContext`] by [`entries`], so all three
//! renderers show exactly the same actions: Copy (only with a selection), Cut,
//! Paste (inside an input), Select All, and Inspect Element. Actions are
//! reported through [`set_actions`]; the event loop performs them because it
//! owns the long-lived clipboard handle.

use std::sync::OnceLock;

/// Something the menu can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Cut,
    Paste,
    SelectAll,
    Inspect,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Copy => "Copy",
            Action::Cut => "Cut",
            Action::Paste => "Paste",
            Action::SelectAll => "Select All",
            Action::Inspect => "Inspect Element",
        }
    }
}

/// One actionable row. `enabled` rows are clickable; disabled rows still show
/// (Paste with an empty clipboard), matching native menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItem {
    pub action: Action,
    pub enabled: bool,
}

/// A menu row: an action or a horizontal separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Item(MenuItem),
    Separator,
}

/// What the user right-clicked on, which decides the available actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MenuContext {
    /// The click landed in an `<input>` (including the URL bar).
    pub editable: bool,
    /// Page text or input text is selected, so Copy/Cut make sense.
    pub has_selection: bool,
    /// Devtools are mounted, so Inspect Element can open them.
    pub can_inspect: bool,
}

/// Build the menu for `ctx`. Copy/Cut are omitted (not merely disabled) when
/// there is nothing selected; Paste appears only for editable targets.
pub fn entries(ctx: &MenuContext) -> Vec<Entry> {
    let item = |action: Action, enabled: bool| Entry::Item(MenuItem { action, enabled });
    let mut rows = Vec::new();
    if ctx.editable {
        if ctx.has_selection {
            rows.push(item(Action::Cut, true));
            rows.push(item(Action::Copy, true));
        }
        rows.push(item(Action::Paste, true));
        rows.push(Entry::Separator);
    } else if ctx.has_selection {
        rows.push(item(Action::Copy, true));
    }
    rows.push(item(Action::SelectAll, true));
    rows.push(Entry::Separator);
    rows.push(item(Action::Inspect, ctx.can_inspect));
    rows
}

/// Callback invoked by the native menu rows, wired by the pipeline to shared
/// app state (clipboard/selection, devtools).
pub struct ContextMenuActions {
    pub on_action: Box<dyn Fn(Action) + Send + Sync>,
}

static ACTIONS: OnceLock<ContextMenuActions> = OnceLock::new();

/// Install the menu actions (called once before the event loop runs).
pub fn set_actions(actions: ContextMenuActions) {
    let _ = ACTIONS.set(actions);
}

/// Record the chosen action for the event loop to perform. Native menus run a
/// nested event loop, so this only stores the choice.
pub fn perform(action: Action) {
    if let Some(actions) = ACTIONS.get() {
        (actions.on_action)(action);
    }
}

/// Show the OS context menu at `(mouse_x, mouse_y)` (window-local device
/// pixels). Linux uses [`inline`] instead; this is a no-op there.
pub fn show_context_menu(
    window: &winit::window::Window,
    mouse_x: f32,
    mouse_y: f32,
    entries: &[Entry],
) {
    #[cfg(target_os = "macos")]
    macos::show(window, mouse_x, mouse_y, entries);
    #[cfg(target_os = "windows")]
    windows::show(window, mouse_x, mouse_y, entries);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (window, mouse_x, mouse_y, entries);
    }
}

/// In-window context menu: geometry, hit-testing, and painting.
///
/// Used on platforms where winit has no native popup menu. The menu is painted
/// by the browser itself (last, above page content), and a left click inside it
/// invokes the matching action; a click anywhere else closes it without
/// activating the page.
pub(crate) mod inline {
    use super::{Action, Entry};
    use crate::render::{draw_text_ab_glyph, RenderState, TextSelection};
    use pwb_core::spec::Color;
    use tiny_skia::{Paint, Pixmap, Rect, Transform};

    /// Menu width in CSS pixels.
    pub(crate) const WIDTH: f32 = 190.0;
    /// Height of one menu row in CSS pixels.
    pub(crate) const ITEM_HEIGHT: f32 = 30.0;
    /// Height of a separator row in CSS pixels.
    pub(crate) const SEPARATOR_HEIGHT: f32 = 9.0;
    /// Vertical padding above the first and below the last row.
    pub(crate) const PADDING: f32 = 6.0;

    /// Menu metrics in device pixels. Geometry and hit-testing share this, so
    /// the painted rows and the clickable rows never disagree on a HiDPI
    /// display.
    fn metrics() -> (f32, f32, f32, f32) {
        (
            crate::dpi::px(WIDTH),
            crate::dpi::px(ITEM_HEIGHT),
            crate::dpi::px(SEPARATOR_HEIGHT),
            crate::dpi::px(PADDING),
        )
    }

    /// Content height of `entries` in device pixels.
    fn content_height(entries: &[Entry]) -> f32 {
        let (_, item_h, separator_h, padding) = metrics();
        let rows: f32 = entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(_) => item_h,
                Entry::Separator => separator_h,
            })
            .sum();
        rows + padding * 2.0
    }

    /// The menu rectangle for a click at `(x, y)`, clamped to the viewport so
    /// a menu opened near an edge stays fully visible.
    pub(crate) fn menu_rect(
        x: f32,
        y: f32,
        viewport_w: f32,
        viewport_h: f32,
        entries: &[Entry],
    ) -> Rect {
        let (width, _, _, _) = metrics();
        let width = width.min(viewport_w.max(1.0));
        let height = content_height(entries).min(viewport_h.max(1.0));
        let left = x.min(viewport_w - width).max(0.0);
        let top = y.min(viewport_h - height).max(0.0);
        Rect::from_xywh(left, top, width, height)
            .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap())
    }

    /// The top edge of every row, in order (including separators).
    fn row_tops(rect: Rect, entries: &[Entry]) -> Vec<f32> {
        let (_, item_h, separator_h, padding) = metrics();
        let mut top = rect.top() + padding;
        entries
            .iter()
            .map(|entry| {
                let this = top;
                top += match entry {
                    Entry::Item(_) => item_h,
                    Entry::Separator => separator_h,
                };
                this
            })
            .collect()
    }

    /// Which enabled action (if any) the point hits.
    pub(crate) fn hit(rect: Rect, x: f32, y: f32, entries: &[Entry]) -> Option<Action> {
        if x < rect.left() || x >= rect.right() || y < rect.top() || y >= rect.bottom() {
            return None;
        }
        let (_, item_h, _, _) = metrics();
        let tops = row_tops(rect, entries);
        for (index, entry) in entries.iter().enumerate() {
            let Entry::Item(item) = entry else {
                continue;
            };
            let top = tops[index];
            if item.enabled && y >= top && y < top + item_h {
                return Some(item.action);
            }
        }
        None
    }

    /// Paint the menu, highlighting the row under the cursor.
    pub(crate) fn draw(
        pixmap: &mut Pixmap,
        rect: Rect,
        mouse_x: f32,
        mouse_y: f32,
        entries: &[Entry],
    ) {
        let (_, item_h, _, padding) = metrics();
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

        let hovered = hit(rect, mouse_x, mouse_y, entries);
        let tops = row_tops(rect, entries);
        for (index, entry) in entries.iter().enumerate() {
            let top = tops[index];
            match entry {
                Entry::Separator => {
                    let y = (top + 1.0).round();
                    if let Some(line) = Rect::from_xywh(
                        rect.left() + padding,
                        y,
                        (rect.width() - padding * 2.0).max(1.0),
                        1.0,
                    ) {
                        paint.set_color_rgba8(78, 80, 96, 255);
                        pixmap.fill_rect(line, &paint, Transform::identity(), None);
                    }
                }
                Entry::Item(item) => {
                    let active = item.enabled && hovered == Some(item.action);
                    if active {
                        if let Some(row) = Rect::from_xywh(
                            rect.left() + 1.0,
                            top,
                            (rect.width() - 2.0).max(1.0),
                            item_h,
                        ) {
                            paint.set_color_rgba8(69, 71, 90, 255);
                            pixmap.fill_rect(row, &paint, Transform::identity(), None);
                        }
                    }
                    let font_size = crate::dpi::px(15.0);
                    draw_text_ab_glyph(
                        pixmap,
                        item.action.label(),
                        rect.left() + crate::dpi::px(12.0),
                        top + (item_h - font_size * 1.1) / 2.0,
                        font_size,
                        if item.enabled {
                            Color {
                                r: 205,
                                g: 214,
                                b: 244,
                                a: 255,
                            }
                        } else {
                            Color {
                                r: 128,
                                g: 132,
                                b: 150,
                                a: 255,
                            }
                        },
                        0,
                        &TextSelection::default(),
                        RenderState::identity(),
                    );
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{perform, Action, Entry};
    use objc2::declare_class;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, Sel};
    use objc2::{msg_send_id, ClassType, DeclaredClass};
    use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
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
                perform(Action::Copy);
            }

            #[method(cutAction:)]
            fn cut_action(&self, _sender: &AnyObject) {
                perform(Action::Cut);
            }

            #[method(pasteAction:)]
            fn paste_action(&self, _sender: &AnyObject) {
                perform(Action::Paste);
            }

            #[method(selectAllAction:)]
            fn select_all_action(&self, _sender: &AnyObject) {
                perform(Action::SelectAll);
            }

            #[method(inspectAction:)]
            fn inspect_action(&self, _sender: &AnyObject) {
                perform(Action::Inspect);
            }
        }
    );

    fn selector(action: Action) -> &'static str {
        match action {
            Action::Copy => "copyAction:",
            Action::Cut => "cutAction:",
            Action::Paste => "pasteAction:",
            Action::SelectAll => "selectAllAction:",
            Action::Inspect => "inspectAction:",
        }
    }

    pub fn show(window: &winit::window::Window, mx: f32, my: f32, entries: &[Entry]) {
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
            // Otherwise AppKit recomputes enabled state from the target and
            // re-enables a disabled Inspect Element row.
            menu.setAutoenablesItems(false);
            // The menu holds its target weakly, so keep the target alive for the
            // duration of the (blocking) popup.
            let target: Retained<CtxTarget> = msg_send_id![CtxTarget::alloc(), init];

            for entry in entries {
                match entry {
                    Entry::Separator => {
                        menu.addItem(&NSMenuItem::separatorItem(mtm));
                    }
                    Entry::Item(item) => {
                        let title = NSString::from_str(item.action.label());
                        let action_selector = Sel::register(selector(item.action));
                        let menu_item = menu.addItemWithTitle_action_keyEquivalent(
                            &title,
                            Some(action_selector),
                            &NSString::from_str(""),
                        );
                        menu_item.setTarget(Some(&*target));
                        menu_item.setEnabled(item.enabled);
                    }
                }
            }

            menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(view));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{perform, Entry};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{HWND, POINT};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow,
        TrackPopupMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_NONOTIFY,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_NULL,
    };

    pub fn show(window: &winit::window::Window, mx: f32, my: f32, entries: &[Entry]) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get() as HWND;

        // Row index → command id (1-based; 0 means "no command").
        let mut commands: Vec<super::Action> = Vec::new();
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let mut command_id = 0usize;
            for entry in entries {
                match entry {
                    Entry::Separator => {
                        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                    }
                    Entry::Item(item) => {
                        command_id += 1;
                        let mut flags = MF_STRING;
                        if !item.enabled {
                            flags |= MF_GRAYED;
                        }
                        let mut title: Vec<u16> = item.action.label().encode_utf16().collect();
                        title.push(0);
                        AppendMenuW(menu, flags, command_id, title.as_ptr());
                        commands.push(item.action);
                    }
                }
            }

            // Screen coordinates: winit's cursor position is client-relative
            // physical pixels, TrackPopupMenu wants screen pixels.
            let mut point = POINT {
                x: mx.round() as i32,
                y: my.round() as i32,
            };
            ClientToScreen(hwnd, &mut point);
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_TOPALIGN | TPM_LEFTALIGN,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            // The documented workaround for the first click after the menu
            // closes being swallowed by the hidden popup window.
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);

            if chosen > 0 {
                if let Some(action) = commands.get(chosen as usize - 1) {
                    perform(*action);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{entries, inline, Action, Entry, MenuContext, MenuItem};

    fn actions(rows: &[Entry]) -> Vec<Action> {
        rows.iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(item.action),
                Entry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn page_menu_hides_copy_without_a_selection() {
        let idle = entries(&MenuContext {
            editable: false,
            has_selection: false,
            can_inspect: true,
        });
        assert_eq!(actions(&idle), vec![Action::SelectAll, Action::Inspect]);
        assert!(!actions(&idle).contains(&Action::Paste));

        let selected = entries(&MenuContext {
            editable: false,
            has_selection: true,
            can_inspect: true,
        });
        assert_eq!(
            actions(&selected),
            vec![Action::Copy, Action::SelectAll, Action::Inspect]
        );
    }

    #[test]
    fn input_menu_offers_paste_and_select_all() {
        let idle = entries(&MenuContext {
            editable: true,
            has_selection: false,
            can_inspect: true,
        });
        assert_eq!(
            actions(&idle),
            vec![Action::Paste, Action::SelectAll, Action::Inspect]
        );

        let selected = entries(&MenuContext {
            editable: true,
            has_selection: true,
            can_inspect: true,
        });
        assert_eq!(
            actions(&selected),
            vec![
                Action::Cut,
                Action::Copy,
                Action::Paste,
                Action::SelectAll,
                Action::Inspect
            ]
        );
    }

    #[test]
    fn inspect_is_disabled_when_devtools_are_missing() {
        let rows = entries(&MenuContext {
            editable: false,
            has_selection: false,
            can_inspect: false,
        });
        assert!(matches!(
            rows.last(),
            Some(Entry::Item(MenuItem {
                action: Action::Inspect,
                enabled: false
            }))
        ));
        // A disabled row is not clickable.
        let rect = inline::menu_rect(10.0, 10.0, 800.0, 600.0, &rows);
        let last_top = rect.bottom() - inline::PADDING - inline::ITEM_HEIGHT;
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, last_top + 5.0, &rows),
            None
        );
    }

    #[test]
    fn inline_menu_clamps_to_the_viewport() {
        let rows = entries(&MenuContext {
            editable: true,
            has_selection: true,
            can_inspect: true,
        });
        // Opening at the bottom-right keeps the whole menu on screen.
        let rect = inline::menu_rect(790.0, 590.0, 800.0, 600.0, &rows);
        assert_eq!(rect.right(), 800.0);
        assert_eq!(rect.bottom(), 600.0);
        // Opening at the top-left sits exactly at the cursor.
        let rect = inline::menu_rect(10.0, 20.0, 800.0, 600.0, &rows);
        assert_eq!((rect.left(), rect.top()), (10.0, 20.0));
    }

    #[test]
    fn inline_menu_paints_the_hovered_row() {
        // The pointer highlight must follow the cursor: row 0 highlighted at
        // one position, row 1 highlighted after moving down.
        let rows = entries(&MenuContext {
            editable: true,
            has_selection: true,
            can_inspect: true,
        });
        let mut pixmap = tiny_skia::Pixmap::new(400, 300).unwrap();
        let rect = inline::menu_rect(20.0, 20.0, 400.0, 300.0, &rows);
        let row_top =
            |index: usize| rect.top() + inline::PADDING + inline::ITEM_HEIGHT * index as f32;
        let sample = |pixmap: &tiny_skia::Pixmap, index: usize| {
            let pixel = pixmap
                .pixel((rect.left() + 5.0) as u32, (row_top(index) + 5.0) as u32)
                .unwrap();
            (pixel.red(), pixel.green(), pixel.blue())
        };

        inline::draw(
            &mut pixmap,
            rect,
            rect.left() + 5.0,
            row_top(0) + 5.0,
            &rows,
        );
        let hover = (69, 71, 90);
        let body = (49, 50, 68);
        assert_eq!(sample(&pixmap, 0), hover, "first row is highlighted");
        assert_eq!(sample(&pixmap, 1), body, "second row is not highlighted");

        inline::draw(
            &mut pixmap,
            rect,
            rect.left() + 5.0,
            row_top(1) + 5.0,
            &rows,
        );
        assert_eq!(sample(&pixmap, 0), body, "highlight moved away from row 0");
        assert_eq!(sample(&pixmap, 1), hover, "row 1 is now highlighted");
    }

    #[test]
    fn inline_menu_hit_tests_actions_and_skips_separators() {
        let rows = entries(&MenuContext {
            editable: true,
            has_selection: false,
            can_inspect: true,
        });
        let rect = inline::menu_rect(100.0, 100.0, 800.0, 600.0, &rows);
        // Rows are Paste, separator, Select All, separator, Inspect.
        let item_row = |index: usize| {
            rect.top()
                + inline::PADDING
                + inline::ITEM_HEIGHT * index as f32
                + inline::SEPARATOR_HEIGHT * index as f32
                + inline::ITEM_HEIGHT / 2.0
        };
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, item_row(0), &rows),
            Some(Action::Paste)
        );
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, item_row(1), &rows),
            Some(Action::SelectAll)
        );
        // The separator after Paste is not clickable.
        let separator_y = rect.top() + inline::PADDING + inline::ITEM_HEIGHT + 2.0;
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, separator_y, &rows),
            None
        );
        // Padding above the first row and points outside the menu miss.
        assert_eq!(
            inline::hit(rect, rect.left() + 20.0, rect.top() + 2.0, &rows),
            None
        );
        assert_eq!(
            inline::hit(rect, rect.left() - 5.0, item_row(0), &rows),
            None
        );
    }
}
