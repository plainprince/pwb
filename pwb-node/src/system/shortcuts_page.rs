//! `shortcuts.plain` — the browser's keyboard and mouse shortcuts.
//!
//! The bindings are resolved by `pwb-browser/src/shortcuts.rs`; this page is
//! the user-facing reference linked from `help.plain`.

use super::network_page::{compile, HELP_STYLES};
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

/// Render `shortcuts.plain`.
pub fn render_shortcuts_page_bundle() -> Bundle {
    let title = format!("{} — Keyboard Shortcuts", SystemDomain::Shortcuts.as_str());
    let pml_markup = r##"<plain>
        <head><title>shortcuts.plain</title></head>
        <view class="help-container">
            <text class="help-title">shortcuts.plain — Keyboard and Mouse Shortcuts</text>
            <text class="help-intro">Ctrl/Cmd means Cmd on macOS and Ctrl on Windows and Linux; both modifiers work everywhere. AltGr (Ctrl+Alt) never triggers a browser shortcut, so characters that need it still type normally.</text>

            <view class="help-card">
                <text class="help-heading">Navigation</text>
                <text class="help-row">Alt+Left / Alt+Right, or Ctrl/Cmd+[ / Ctrl/Cmd+] — back / forward</text>
                <text class="help-row">Ctrl/Cmd+R or F5 — reload the page</text>
                <text class="help-row">Ctrl/Cmd+L, Alt+D, or F6 — focus the address bar (the URL is selected)</text>
                <text class="help-row">Alt+Home — open the home page in a new tab</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Tabs</text>
                <text class="help-row">Ctrl/Cmd+T — new tab</text>
                <text class="help-row">Ctrl/Cmd+W — close the current tab</text>
                <text class="help-row">Ctrl/Cmd+Shift+T — reopen the last closed tab (up to 10)</text>
                <text class="help-row">Ctrl+Tab / Ctrl+Shift+Tab, or Ctrl/Cmd+PageDown / PageUp — next / previous tab</text>
                <text class="help-row">Ctrl/Cmd+1 … Ctrl/Cmd+8 — select a tab; Ctrl/Cmd+9 — last tab</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Editing and selection</text>
                <text class="help-row">Ctrl/Cmd+A — select the focused input's text, otherwise all page text</text>
                <text class="help-row">Ctrl/Cmd+C / X / V — copy, cut, paste (paste works in inputs and the URL bar)</text>
                <text class="help-row">Shift+Left / Right / Home / End — extend an input selection</text>
                <text class="help-row">Backspace / Delete — delete the selection or the character next to the caret</text>
                <text class="help-row">Double-click — select a word in an input; triple-click — select the whole value; drag — select a range</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Developer tools and page tools</text>
                <text class="help-row">F12, Ctrl/Cmd+Shift+I, or Ctrl/Cmd+Shift+J — toggle devtools (Cmd+Option+I / J on macOS)</text>
                <text class="help-row">Ctrl/Cmd+Shift+C — inspect an element (Cmd+Option+C on macOS)</text>
                <text class="help-row">Ctrl/Cmd+S — save the current page as a .pwb bundle</text>
                <text class="help-row">Ctrl/Cmd+D — bookmark the current page</text>
            </view>

            <view class="help-card">
                <text class="help-heading">Mouse</text>
                <text class="help-row">Right-click — context menu; it shows Cut/Copy only with a selection, Paste inside inputs and the URL bar, Select All, Inspect Element, and link actions on links</text>
                <text class="help-row">Arrow keys move the in-window menu's highlight on Linux; Enter runs the selected row and Escape closes the menu</text>
                <text class="help-row">Click the row under the pointer, or click outside the menu, to choose or dismiss it</text>
            </view>

            <link class="help-link" href="help.plain">← help.plain</link>
        </view>
    </plain>"##;

    compile(title, pml_markup, HELP_STYLES, None)
}
