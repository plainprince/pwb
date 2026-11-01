//! Browser shell: tabs, session history, bookmarks, and the persistent
//! chrome drawn above pages.
//!
//! PlainWeb's renderer is a page renderer, not a window manager. The shell is
//! therefore represented as a fixed-position chrome tree injected into every
//! loaded DOM: a tab strip, an address bar, back/forward/reload, and a bookmark
//! toggle. The page itself keeps its normal layout underneath.
//!
//! State is deliberately independent of the renderer so it can be tested
//! without a window: [`Shell`] is a plain model with navigation rules, and
//! [`chrome_tree`] converts it to DOM nodes. [`Shell::save`] persists tabs and
//! bookmarks to `$PWB_DATA/session.json` (or `$HOME/.pwb/session.json`), so a
//! browser restart reopens the same session.

use std::collections::HashMap;
use std::path::PathBuf;

use pwb_core::spec::{PlainNode, Tag};

use crate::dom::{Dom, DomNode};
use crate::layout::measure_text_width;
use crate::theme::{self, CloseButton, Metrics, Theme, ThemeInfo};

/// Chrome element id prefix. Page elements cannot claim these names.
pub const CHROME_PREFIX: &str = "__pwb_";

/// Where a new tab (or a tab that replaces the last one) lands: the system
/// search page, so an empty tab is one keystroke away from a query.
pub const NEW_TAB_URL: &str = "search.plain";

/// One open tab. `history`/`index` implement per-tab back/forward; navigating
/// truncates the forward entries (standard browser behavior).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub url: String,
    pub title: String,
    pub history: Vec<String>,
    pub index: usize,
}

impl Tab {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            title: url.to_string(),
            history: vec![url.to_string()],
            index: 0,
        }
    }

    fn visit(&mut self, url: &str) {
        if self.history.get(self.index) == Some(&url.to_string()) {
            return;
        }
        self.history.truncate(self.index + 1);
        self.history.push(url.to_string());
        self.index = self.history.len() - 1;
        self.url = url.to_string();
    }

    /// Move one entry back, returning the URL to load.
    pub fn back(&mut self) -> Option<String> {
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        self.url = self.history[self.index].clone();
        Some(self.url.clone())
    }

    /// Move one entry forward, returning the URL to load.
    pub fn forward(&mut self) -> Option<String> {
        if self.index + 1 >= self.history.len() {
            return None;
        }
        self.index += 1;
        self.url = self.history[self.index].clone();
        Some(self.url.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

/// Tabs + bookmarks + persistence path.
#[derive(Debug, Clone)]
pub struct Shell {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub bookmarks: Vec<Bookmark>,
    /// Live peer count, mirrored from the node engine so the chrome can say
    /// "offline" instead of silently rendering pages nobody will answer for.
    /// Not persisted: it describes this process, not the session.
    pub peers: usize,
    /// Id of the active `.ptheme` (persisted with the session).
    pub theme: String,
    /// First tab index shown in the tab strip; wheel/`<`/`>` scrolling moves
    /// it when the tabs overflow the window.
    pub tab_scroll: usize,
    /// Chrome control currently under the pointer (`tab_2`, `back`, …); the
    /// tab strip shows its close button for hovered tabs.
    pub chrome_hover: Option<String>,
    path: Option<PathBuf>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            tabs: vec![Tab::new("welcome.plain")],
            active: 0,
            bookmarks: Vec::new(),
            peers: 0,
            theme: "default-dark".to_string(),
            tab_scroll: 0,
            chrome_hover: None,
            path: None,
        }
    }
}

impl Shell {
    /// A shell with one tab for `url`, loading persisted bookmarks/session
    /// when a path is found.
    pub fn new(initial_url: &str) -> Self {
        let mut shell = Self {
            tabs: vec![Tab::new(initial_url)],
            active: 0,
            ..Self::default()
        };
        if let Some(path) = default_session_path() {
            shell.path = Some(path.clone());
            shell.load(&path);
            if shell.tabs.is_empty() {
                shell.tabs.push(Tab::new(initial_url));
            }
            if initial_url != "welcome.plain" {
                // The command line target wins over the restored tab.
                shell.tabs[shell.active].url = initial_url.to_string();
                shell.tabs[shell.active].title = initial_url.to_string();
            }
        } else {
            shell.tabs[0] = Tab::new(initial_url);
        }
        shell
    }

    /// Short human label for the connection badge.
    pub fn connection_label(&self) -> String {
        match self.peers {
            0 => "● offline".to_string(),
            1 => "● 1 peer".to_string(),
            peers => format!("● {peers} peers"),
        }
    }

    pub fn current(&self) -> &Tab {
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))]
    }

    pub fn current_url(&self) -> &str {
        &self.current().url
    }

    /// Record a committed navigation in the active tab.
    pub fn navigate(&mut self, url: &str) {
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(url));
            self.active = 0;
        }
        self.tabs[self.active].visit(url);
        self.tabs[self.active].title = url.to_string();
        self.save();
    }

    /// Update a tab title once a page has rendered.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.url == url) {
            tab.title = format!("{title} — {url}");
            self.save();
        }
    }

    pub fn back(&mut self) -> Option<String> {
        let url = self.tabs.get_mut(self.active)?.back()?;
        self.save();
        Some(url)
    }

    pub fn forward(&mut self) -> Option<String> {
        let url = self.tabs.get_mut(self.active)?.forward()?;
        self.save();
        Some(url)
    }

    /// Whether the active tab has an earlier history entry (the back button is
    /// enabled).
    pub fn can_go_back(&self) -> bool {
        self.current().index > 0
    }

    /// Whether the active tab has a later history entry (the forward button is
    /// enabled).
    pub fn can_go_forward(&self) -> bool {
        let tab = self.current();
        tab.index + 1 < tab.history.len()
    }

    pub fn new_tab(&mut self, url: &str) {
        self.tabs.push(Tab::new(url));
        self.active = self.tabs.len() - 1;
        self.save();
    }

    /// Close the active tab, returning the URL to load next. Closing the last
    /// tab replaces it with a fresh new tab (a browser always has a page).
    pub fn close_active(&mut self) -> Option<String> {
        if self.tabs.len() <= 1 {
            let url = NEW_TAB_URL.to_string();
            self.tabs[0] = Tab::new(&url);
            self.active = 0;
            self.save();
            return Some(url);
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.save();
        Some(self.current_url().to_string())
    }

    pub fn select(&mut self, index: usize) -> Option<String> {
        if index >= self.tabs.len() || index == self.active {
            return None;
        }
        self.active = index;
        self.tab_scroll = self.tab_scroll.min(index);
        self.save();
        Some(self.current_url().to_string())
    }

    /// Close a specific tab (its × button). Closing the active tab selects a
    /// neighbor; closing the last tab opens a fresh new tab.
    pub fn close_tab(&mut self, index: usize) -> Option<String> {
        if index >= self.tabs.len() {
            return None;
        }
        self.active = index;
        let url = self.close_active();
        self.tab_scroll = self.tab_scroll.min(self.active);
        url
    }

    /// Scroll the tab strip by `delta` tabs (wheel or `<`/`>` buttons).
    pub fn scroll_tabs(&mut self, delta: i32) {
        let last = self.tabs.len().saturating_sub(1);
        let next = self.tab_scroll as i64 + delta as i64;
        self.tab_scroll = next.clamp(0, last as i64) as usize;
    }

    /// Keep the active tab inside the visible window after it changes.
    pub fn reveal_active(&mut self, visible_count: usize) {
        let count = visible_count.max(1);
        if self.active < self.tab_scroll {
            self.tab_scroll = self.active;
        } else if self.active >= self.tab_scroll + count {
            self.tab_scroll = self.active + 1 - count;
        }
        self.tab_scroll = self.tab_scroll.min(self.tabs.len().saturating_sub(1));
    }

    /// The shortened address shown while the URL bar is unfocused: the host
    /// (or content id / file name) without the protocol, path, or query.
    pub fn address_display(&self) -> String {
        shorten_url(self.current_url())
    }
}

/// Theme id from a `settings.plain?theme=<id>` link.
pub fn settings_theme_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("settings.plain?")?;
    for pair in rest.split('&') {
        if let Some(id) = pair.strip_prefix("theme=") {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

impl Shell {
    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|bookmark| bookmark.url == url)
    }

    /// Add or remove the active page from bookmarks. Returns `true` when the
    /// page is bookmarked afterwards.
    pub fn toggle_bookmark(&mut self) -> bool {
        let url = self.current_url().to_string();
        let title = self.current().title.clone();
        if let Some(pos) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(pos);
            self.save();
            false
        } else {
            self.bookmarks.push(Bookmark { url, title });
            self.save();
            true
        }
    }

    /// The path used for persistence, when one is configured.
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// Persist tabs/bookmarks. Best-effort: a read-only or missing data
    /// directory must not interrupt browsing.
    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            .map(|tab| {
                serde_json::json!({
                    "url": tab.url,
                    "title": tab.title,
                    "history": tab.history,
                    "index": tab.index,
                })
            })
            .collect();
        let bookmarks: Vec<serde_json::Value> = self
            .bookmarks
            .iter()
            .map(|bookmark| serde_json::json!({"url": bookmark.url, "title": bookmark.title}))
            .collect();
        let value = serde_json::json!({
            "version": 2,
            "active": self.active,
            "theme": self.theme,
            "tabs": tabs,
            "bookmarks": bookmarks,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Temp-file + rename so a crash cannot leave a truncated session.
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, bytes).is_ok() {
                let _ = std::fs::rename(&temp, path);
            }
        }
    }

    /// Load a persisted session. Invalid entries are skipped rather than
    /// aborting the whole restore.
    pub fn load(&mut self, path: &std::path::Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let mut tabs = Vec::new();
        if let Some(entries) = value.get("tabs").and_then(|v| v.as_array()) {
            for entry in entries {
                let Some(url) = entry.get("url").and_then(|v| v.as_str()) else {
                    continue;
                };
                let title = entry
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(url)
                    .to_string();
                let history: Vec<String> = entry
                    .get("history")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .filter(|items: &Vec<String>| !items.is_empty())
                    .unwrap_or_else(|| vec![url.to_string()]);
                let index = entry
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .map(|index| (index as usize).min(history.len() - 1))
                    .unwrap_or(history.len() - 1);
                tabs.push(Tab {
                    url: url.to_string(),
                    title,
                    history,
                    index,
                });
            }
        }
        if !tabs.is_empty() {
            self.tabs = tabs;
            self.active = value
                .get("active")
                .and_then(|v| v.as_u64())
                .map(|index| (index as usize).min(self.tabs.len() - 1))
                .unwrap_or(0);
        }
        if let Some(theme) = value.get("theme").and_then(|v| v.as_str()) {
            if !theme.trim().is_empty() {
                self.theme = theme.to_string();
            }
        }
        if let Some(entries) = value.get("bookmarks").and_then(|v| v.as_array()) {
            self.bookmarks = entries
                .iter()
                .filter_map(|entry| {
                    let url = entry.get("url")?.as_str()?.to_string();
                    let title = entry
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&url)
                        .to_string();
                    Some(Bookmark { url, title })
                })
                .collect();
        }
    }
}

/// Default persistence path: `$PWB_DATA/session.json`, else
/// `<home>/.pwb/session.json`, with the home directory resolved portably
/// (`$HOME`, `$USERPROFILE`, or `%HOMEDRIVE%%HOMEPATH%`) so tabs and
/// bookmarks survive on Windows too.
pub fn default_session_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PWB_DATA") {
        return Some(PathBuf::from(dir).join("session.json"));
    }
    pwb_core::paths::home_dir().map(|home| home.join(".pwb").join("session.json"))
}

/// A specific chrome action derived from the clicked node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChromeAction {
    Back,
    Forward,
    Reload,
    NewTab,
    CloseTab(usize),
    SelectTab(usize),
    /// Move the visible tab window left (-1) or right (+1).
    ScrollTabs(i32),
    ToggleBookmark,
    OpenBookmarks,
    OpenHistory,
    OpenNetwork,
    OpenSettings,
}

/// Resolve a clicked node to a chrome action by walking up to the nearest
/// element whose `id` starts with [`CHROME_PREFIX`]. Returns `None` for page
/// content, so normal link/JS handling is unchanged.
pub fn chrome_action(root: &DomNode, node_id: usize) -> Option<ChromeAction> {
    let suffix = chrome_hover_suffix(root, node_id)?;
    Some(match suffix.as_str() {
        "back" => ChromeAction::Back,
        "forward" => ChromeAction::Forward,
        "reload" => ChromeAction::Reload,
        "new_tab" => ChromeAction::NewTab,
        "bookmark" => ChromeAction::ToggleBookmark,
        "bookmarks" => ChromeAction::OpenBookmarks,
        "history" => ChromeAction::OpenHistory,
        "status" => ChromeAction::OpenNetwork,
        "settings" => ChromeAction::OpenSettings,
        "tabscroll_left" => ChromeAction::ScrollTabs(-1),
        "tabscroll_right" => ChromeAction::ScrollTabs(1),
        other => match other.strip_prefix("close_") {
            Some(index) => ChromeAction::CloseTab(index.parse::<usize>().ok()?),
            None => ChromeAction::SelectTab(other.strip_prefix("tab_")?.parse().ok()?),
        },
    })
}

/// The chrome element suffix (`back`, `tab_2`, `close_0`, …) of the nearest
/// ancestor carrying a `__pwb_` id, if any.
pub fn chrome_hover_suffix(root: &DomNode, node_id: usize) -> Option<String> {
    for id in std::iter::once(node_id).chain(ancestor_ids(root, node_id)) {
        let Some(node) = crate::dom::find_node_by_id_ref(root, &id.to_string()) else {
            continue;
        };
        let Some(element_id) = node.attributes.get("id") else {
            continue;
        };
        if let Some(rest) = element_id.strip_prefix(CHROME_PREFIX) {
            return Some(rest.to_string());
        }
    }
    None
}

/// The DOM id of the address input (also used by the Ctrl/Cmd+L shortcut).
pub fn address_node_id(root: &DomNode) -> Option<usize> {
    find_chrome(root, "address")
}

/// Find a chrome element by its suffix (`address`, `bookmark`, ...).
pub fn find_chrome(root: &DomNode, suffix: &str) -> Option<usize> {
    find_by_element_id(root, &format!("{CHROME_PREFIX}{suffix}")).map(|node| node.id)
}

fn find_by_element_id<'a>(node: &'a DomNode, element_id: &str) -> Option<&'a DomNode> {
    if node.attributes.get("id").map(String::as_str) == Some(element_id) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_by_element_id(child, element_id) {
            return Some(found);
        }
    }
    None
}

/// Ancestor ids from the clicked node up to the root (nearest first). The
/// pipeline's equivalent helper is private to the event layer; the shell only
/// needs the id chain, not layout-specific DOM utilities.
fn ancestor_ids(root: &DomNode, target: usize) -> Vec<usize> {
    fn walk(node: &DomNode, target: usize, path: &mut Vec<usize>) -> bool {
        path.push(node.id);
        if node.id == target {
            return true;
        }
        for child in &node.children {
            if walk(child, target, path) {
                return true;
            }
        }
        path.pop();
        false
    }
    let mut path = Vec::new();
    if !walk(root, target, &mut path) {
        return Vec::new();
    }
    path.pop(); // the clicked node itself; the caller adds it only once
    path.reverse();
    path
}

fn max_node_id(node: &DomNode) -> usize {
    node.children
        .iter()
        .map(max_node_id)
        .fold(node.id, usize::max)
}

// ---------------------------------------------------------------------------
// Chrome tree
// ---------------------------------------------------------------------------

/// Shortened address for display: the host, content id, or file name without
/// the protocol, path, or query. Clicking the field reveals the full URL.
pub fn shorten_url(url: &str) -> String {
    let trimmed = url.trim();
    let no_scheme = trimmed.strip_prefix("plain://").unwrap_or(trimmed);
    if no_scheme.is_empty() {
        return String::new();
    }
    if let Some(path) = no_scheme.strip_prefix("file.pwb/") {
        return path.rsplit('/').next().unwrap_or(path).to_string();
    }
    if let Some(cid) = no_scheme
        .strip_prefix("content.pwb/")
        .or_else(|| no_scheme.strip_prefix("content/"))
    {
        let cid = cid.split(['?', '#']).next().unwrap_or(cid);
        let chars: Vec<char> = cid.chars().collect();
        return if chars.len() > 16 {
            format!("{}…", chars[..12].iter().collect::<String>())
        } else {
            cid.to_string()
        };
    }
    let base = no_scheme.split(['?', '#']).next().unwrap_or(no_scheme);
    let host = base.split('/').next().unwrap_or(base);
    if host.is_empty() {
        base.to_string()
    } else {
        host.to_string()
    }
}

/// The label shown on a tab: the page title, or the shortened address when the
/// title is just the URL.
fn tab_label(tab: &Tab) -> String {
    let title = tab.title.trim();
    let label = title.split(" — ").next().unwrap_or(title).trim();
    if label.is_empty() || label == tab.url {
        shorten_url(&tab.url)
    } else {
        label.to_string()
    }
}

fn truncate_to_width(text: &str, font_size: f32, max_width: f32) -> String {
    if measure_text_width(text, font_size, None) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let mut out = String::new();
    let mut width = measure_text_width(ellipsis, font_size, None);
    for ch in text.chars() {
        let advance = measure_text_width(&ch.to_string(), font_size, None);
        if width + advance > max_width {
            break;
        }
        out.push(ch);
        width += advance;
    }
    out.push('…');
    out
}

/// Which tabs fit in the strip and how wide each one is.
#[derive(Debug, Clone, PartialEq)]
pub struct TabLayout {
    /// `(tab index, width in px)` for the visible tabs.
    pub widths: Vec<(usize, f32)>,
    /// Width of the tabs container itself (so the right scroll button sits at
    /// the strip's right edge).
    pub width: f32,
    pub left_hidden: bool,
    pub right_hidden: bool,
}

impl TabLayout {
    pub fn count(&self) -> usize {
        self.widths.len()
    }
}

/// Compute the visible tab window for `shell` at the current window width.
///
/// Tabs have a pixel min/max width from the theme; when they do not all fit the
/// strip shows a window starting at `shell.tab_scroll`, and the `<`/`>` buttons
/// appear only for a side that has hidden tabs.
pub fn tab_layout(
    shell: &Shell,
    theme: &Theme,
    metrics: &Metrics,
    viewport_width: f32,
) -> TabLayout {
    let gap = metrics.gap;
    let strip_content = (viewport_width - 8.0).max(120.0);
    let new_tab = metrics.control + gap;
    let scroll_reserve = if theme.layout.scroll_buttons {
        2.0 * (metrics.control + gap)
    } else {
        0.0
    };
    let min_w = (theme.layout.tab_min_width * metrics.scale).max(52.0);
    let max_w = (theme.layout.tab_max_width * metrics.scale).max(min_w);
    // How many tabs fit at the minimum width.
    let capacity = |available: f32| -> usize {
        let for_tabs = (available - new_tab).max(72.0);
        (((for_tabs + gap) / (min_w + gap)).floor() as usize).max(1)
    };

    let start = shell.tab_scroll.min(shell.tabs.len().saturating_sub(1));
    if shell.tabs.is_empty() {
        return TabLayout {
            widths: Vec::new(),
            width: strip_content,
            left_hidden: false,
            right_hidden: false,
        };
    }
    let need_scroll = shell.tabs.len() > capacity(strip_content);
    let available = if need_scroll {
        (strip_content - scroll_reserve).max(120.0)
    } else {
        strip_content
    };
    let count = capacity(available)
        .min(shell.tabs.len().saturating_sub(start))
        .max(1);
    // Tabs share the available width; with few tabs they grow up to the
    // theme's max width instead of staying at the overflow-minimum size.
    let for_tabs = (available - new_tab).max(72.0);
    let width = ((for_tabs - gap * (count as f32 - 1.0)) / count as f32)
        .clamp(min_w, max_w)
        .min(for_tabs.max(52.0));
    let left_hidden = start > 0;
    let right_hidden = start + count < shell.tabs.len();
    let widths = (start..start + count).map(|index| (index, width)).collect();
    TabLayout {
        widths,
        width: available,
        left_hidden,
        right_hidden,
    }
}

/// How many tabs the strip shows at the current window size (used to keep the
/// active tab visible when it changes).
pub fn visible_tab_count(
    shell: &Shell,
    theme: &Theme,
    viewport_width: f32,
    viewport_height: f32,
) -> usize {
    let metrics = Metrics::for_window(
        theme,
        viewport_width.round() as u32,
        viewport_height.round() as u32,
    );
    tab_layout(shell, theme, &metrics, viewport_width).count()
}

fn plain_node(id: usize, tag: Tag, element_id: Option<&str>, class: &str) -> PlainNode {
    let mut node = PlainNode::new(id, tag);
    if let Some(element_id) = element_id {
        node.attributes
            .insert("id".to_string(), element_id.to_string());
    }
    if !class.is_empty() {
        node.attributes
            .insert("class".to_string(), class.to_string());
    }
    node
}

fn text_node(id: usize, text: &str, class: &str) -> PlainNode {
    let mut node = plain_node(id, Tag::Text, None, class);
    node.text_content = Some(text.to_string());
    node
}

fn icon_node(id: usize, icon: &str, class: &str) -> PlainNode {
    let mut node = plain_node(id, Tag::Icon, None, class);
    node.attributes.insert("icon".to_string(), icon.to_string());
    node
}

fn classes(base: &str, extra: &[(&str, bool)]) -> String {
    let mut out = base.to_string();
    for (class, enabled) in extra {
        if *enabled {
            out.push(' ');
            out.push_str(class);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn icon_button(
    id: usize,
    suffix: &str,
    icon: &str,
    icon_class: &str,
    title: &str,
    hovered: bool,
    disabled: bool,
    metrics: &Metrics,
) -> PlainNode {
    let mut node = plain_node(
        id,
        Tag::View,
        Some(&format!("{CHROME_PREFIX}{suffix}")),
        &classes(
            "pwb-iconbtn",
            &[
                ("hovered", hovered && !disabled),
                ("pwb-disabled", disabled),
            ],
        ),
    );
    node.attributes
        .insert("title".to_string(), title.to_string());
    // A fixed glyph size keeps every control on the same pixel grid even
    // before the PSS class is applied. Disabled controls get the dimmed glyph
    // class, and never a hover highlight.
    let glyph_class = if disabled {
        "pwb-icon-disabled"
    } else {
        icon_class
    };
    let mut glyph = icon_node(id + 500, icon, glyph_class);
    glyph
        .attributes
        .insert("width".to_string(), format!("{}px", metrics.icon));
    glyph
        .attributes
        .insert("height".to_string(), format!("{}px", metrics.icon));
    node.children.push(glyph);
    node
}

/// Build the fixed chrome tree for `shell`: a tab strip above a centered
/// address toolbar, Chrome-style.
///
/// Node ids are allocated from `first_id` at fixed offsets, so rebuilding the
/// chrome (hover changes, tab scrolling) keeps every control's id stable —
/// important while the address field is focused.
pub fn chrome_tree(
    shell: &Shell,
    theme: &Theme,
    metrics: &Metrics,
    viewport_width: f32,
    first_id: usize,
) -> DomNode {
    let base = first_id;
    let hovered = |suffix: &str| shell.chrome_hover.as_deref() == Some(suffix);
    let layout = tab_layout(shell, theme, metrics, viewport_width);

    let mut chrome = plain_node(base, Tag::View, Some("__pwb_chrome"), "pwb-chrome");

    // ---- Tab strip -------------------------------------------------------
    let mut strip = plain_node(base + 1, Tag::View, Some("__pwb_tabs_row"), "pwb-tabstrip");
    if layout.left_hidden && theme.layout.scroll_buttons {
        strip.children.push(icon_button(
            base + 2,
            "tabscroll_left",
            &theme.icons.tab_left,
            "pwb-icon-muted",
            "Scroll tabs left",
            hovered("tabscroll_left"),
            false,
            metrics,
        ));
    }

    let mut tabs = plain_node(base + 4, Tag::View, Some("__pwb_tabs"), "pwb-tabs");
    tabs.attributes
        .insert("width".to_string(), format!("{}px", layout.width));
    let close_size = (metrics.control * 0.55).max(14.0);
    for (index, width) in &layout.widths {
        let tab = &shell.tabs[*index];
        let active = *index == shell.active;
        let hovered_tab = shell.chrome_hover.as_deref() == Some(format!("tab_{index}").as_str())
            || shell.chrome_hover.as_deref() == Some(format!("close_{index}").as_str());
        let show_close = match theme.layout.close_button {
            CloseButton::Always => true,
            CloseButton::Never => false,
            CloseButton::Hover => active || hovered_tab,
        };
        let mut tab_node = plain_node(
            base + 100 + index,
            Tag::View,
            Some(&format!("{CHROME_PREFIX}tab_{index}")),
            &classes("pwb-tab", &[("active", active), ("hovered", hovered_tab)]),
        );
        tab_node
            .attributes
            .insert("width".to_string(), format!("{width}px"));
        tab_node
            .attributes
            .insert("title".to_string(), tab.url.clone());

        // Reserve the label's inset and, when shown, the close button so the
        // text is truncated before it collides with the ×.
        let label_width = (*width
            - 12.0
            - metrics.gap.min(6.0)
            - if show_close {
                close_size + metrics.gap.min(6.0)
            } else {
                0.0
            })
        .max(12.0);
        let label = truncate_to_width(&tab_label(tab), metrics.ui_font, label_width);
        let label_class = if active {
            "pwb-tab-label pwb-tab-label-active"
        } else {
            "pwb-tab-label"
        };
        // The label owns everything left of the ×, so `space-between` pins the
        // close button to the tab's right edge and the truncated text cannot
        // run underneath it.
        let mut label_node = text_node(base + 200 + index, &label, label_class);
        label_node
            .attributes
            .insert("width".to_string(), format!("{label_width}px"));
        tab_node.children.push(label_node);
        if show_close {
            let mut close = plain_node(
                base + 400 + index,
                Tag::View,
                Some(&format!("{CHROME_PREFIX}close_{index}")),
                "pwb-tab-close",
            );
            close
                .attributes
                .insert("title".to_string(), format!("Close {}", tab_label(tab)));
            close.children.push(icon_node(
                base + 600 + index,
                &theme.icons.close,
                "pwb-icon-muted",
            ));
            tab_node.children.push(close);
        }
        tabs.children.push(tab_node);
    }
    // "+" sits right after the last visible tab.
    let mut new_tab = plain_node(
        base + 5,
        Tag::View,
        Some("__pwb_new_tab"),
        &classes("pwb-newtab", &[("hovered", hovered("new_tab"))]),
    );
    new_tab
        .attributes
        .insert("title".to_string(), "New tab (Ctrl/Cmd+T)".to_string());
    new_tab
        .children
        .push(icon_node(base + 650, &theme.icons.new_tab, "pwb-icon-nav"));
    tabs.children.push(new_tab);
    strip.children.push(tabs);

    if layout.right_hidden && theme.layout.scroll_buttons {
        strip.children.push(icon_button(
            base + 3,
            "tabscroll_right",
            &theme.icons.tab_right,
            "pwb-icon-muted",
            "Scroll tabs right",
            hovered("tabscroll_right"),
            false,
            metrics,
        ));
    }
    chrome.children.push(strip);

    // ---- Toolbar ---------------------------------------------------------
    let mut toolbar = plain_node(
        base + 6,
        Tag::View,
        Some("__pwb_address_row"),
        "pwb-toolbar",
    );

    let mut nav = plain_node(base + 7, Tag::View, Some("__pwb_nav"), "pwb-nav");
    // Back/forward are drawn disabled (and lose their hover highlight) when
    // the tab has no history in that direction.
    let can_back = shell.can_go_back();
    let can_forward = shell.can_go_forward();
    for (suffix, offset, icon, title, enabled) in [
        (
            "back",
            10_usize,
            &theme.icons.back,
            "Back (Alt+Left)",
            can_back,
        ),
        (
            "forward",
            11,
            &theme.icons.forward,
            "Forward (Alt+Right)",
            can_forward,
        ),
        ("reload", 12, &theme.icons.reload, "Reload (Ctrl+R)", true),
    ] {
        nav.children.push(icon_button(
            base + offset,
            suffix,
            icon,
            "pwb-icon-nav",
            title,
            hovered(suffix),
            !enabled,
            metrics,
        ));
    }
    toolbar.children.push(nav);

    let mut address_group = plain_node(
        base + 13,
        Tag::View,
        Some("__pwb_address_group"),
        "pwb-address-group",
    );
    let mut address = PlainNode::new(base + 14, Tag::Input);
    address
        .attributes
        .insert("id".to_string(), format!("{CHROME_PREFIX}address"));
    address
        .attributes
        .insert("class".to_string(), "pwb-address".to_string());
    address
        .attributes
        .insert("value".to_string(), shell.current_url().to_string());
    address
        .attributes
        .insert("data-short-url".to_string(), shell.address_display());
    // PSS percentages resolve against the viewport, not the parent row, so the
    // field's width is computed in pixels from the theme's address-width knob
    // (minus the group's horizontal padding).
    let address_width = (viewport_width * theme.layout.address_width / 100.0 - 14.0).max(80.0);
    address
        .attributes
        .insert("width".to_string(), format!("{address_width}px"));
    address.attributes.insert(
        "placeholder".to_string(),
        "Search or enter address".to_string(),
    );
    address.attributes.insert(
        "title".to_string(),
        "Address bar (Ctrl/Cmd+L) — click to see the full URL".to_string(),
    );
    address_group.children.push(address);
    toolbar.children.push(address_group);

    let mut actions = plain_node(base + 15, Tag::View, Some("__pwb_actions"), "pwb-actions");
    let bookmarked = shell.is_bookmarked(shell.current_url());
    actions.children.push(icon_button(
        base + 20,
        "bookmark",
        &theme.icons.bookmark,
        if bookmarked {
            "pwb-icon-accent"
        } else {
            "pwb-icon-nav"
        },
        if bookmarked {
            "Bookmarked — click to remove (Ctrl+D)"
        } else {
            "Bookmark this page (Ctrl+D)"
        },
        hovered("bookmark"),
        false,
        metrics,
    ));
    for (suffix, offset, icon, title) in [
        (
            "bookmarks",
            21_usize,
            &theme.icons.bookmarks,
            "Open bookmarks",
        ),
        ("history", 22, &theme.icons.history, "Open history"),
    ] {
        actions.children.push(icon_button(
            base + offset,
            suffix,
            icon,
            "pwb-icon-nav",
            title,
            hovered(suffix),
            false,
            metrics,
        ));
    }

    // Connection badge: a red/green dot plus the live peer count.
    let mut status = plain_node(
        base + 23,
        Tag::View,
        Some("__pwb_status"),
        &classes("pwb-status", &[("hovered", hovered("status"))]),
    );
    status.attributes.insert(
        "title".to_string(),
        format!("{}. Click for network.plain", shell.connection_label()),
    );
    status.children.push(text_node(
        base + 23 + 800,
        "●",
        if shell.peers == 0 {
            "pwb-status-dot-off"
        } else {
            "pwb-status-dot"
        },
    ));
    status.children.push(text_node(
        base + 23 + 900,
        &shell.peers.to_string(),
        "pwb-status-label",
    ));
    actions.children.push(status);

    actions.children.push(icon_button(
        base + 24,
        "settings",
        &theme.icons.settings,
        "pwb-icon-nav",
        "Theme settings",
        hovered("settings"),
        false,
        metrics,
    ));
    toolbar.children.push(actions);
    chrome.children.push(toolbar);

    // Generated chrome styles, then the theme's raw `ui` PSS on top.
    let rules = pwb_web::parse::parse_pss_stylesheet(&theme.chrome_css(metrics));
    pwb_web::parse::apply_stylesheet(&mut chrome, &rules);
    crate::dom::dom_node_from_plain(&chrome)
}

/// Replace the chrome subtree in `dom` with a fresh one for `shell`.
pub fn install_chrome(
    dom: &mut Dom,
    shell: &Shell,
    theme: &Theme,
    viewport_width: u32,
    viewport_height: u32,
) {
    dom.root
        .children
        .retain(|child| child.attributes.get("id").map(String::as_str) != Some("__pwb_chrome"));
    let next_id = max_node_id(&dom.root) + 1;
    let metrics = Metrics::for_window(theme, viewport_width, viewport_height);
    let chrome = chrome_tree(shell, theme, &metrics, viewport_width as f32, next_id);
    // Paint above the page but below the devtools overlay, which is mounted as
    // a later root child. Appending unconditionally would put the bar on top of
    // the devtools sidebar's header.
    let devtools = dom.root.children.iter().position(|child| {
        child.attributes.get("data-devtools").map(String::as_str) == Some("root")
    });
    match devtools {
        Some(index) => dom.root.children.insert(index, chrome),
        None => dom.root.children.push(chrome),
    }
}
/// native `<link>` elements, so clicking one navigates without JavaScript.
pub fn list_page(shell: &Shell, theme: &Theme, kind: &str) -> Vec<u8> {
    let (title, entries): (&str, Vec<(&str, String)>) = match kind {
        "history" => (
            "history.plain",
            shell
                .tabs
                .iter()
                .flat_map(|tab| tab.history.iter().map(|url| ("History", url.clone())))
                .collect(),
        ),
        _ => (
            "bookmarks.plain",
            shell
                .bookmarks
                .iter()
                .map(|bookmark| (bookmark.title.as_str(), bookmark.url.clone()))
                .collect(),
        ),
    };
    let mut body = String::new();
    if entries.is_empty() {
        body.push_str("<text class=\"empty\">Nothing here yet.</text>");
    } else {
        for (label, url) in entries.iter().rev().take(200) {
            let label = if label.is_empty() {
                url.as_str()
            } else {
                label
            };
            body.push_str(&format!(
                "<link href=\"{}\">{}</link>",
                escape(url),
                escape(label)
            ));
            body.push_str(&format!("<text class=\"url\">{}</text>", escape(url)));
        }
    }
    let markup = format!(
        "<plain><head><title>{title}</title></head><view class=\"page\">\
         <text class=\"title\">{title}</text>{body}</view></plain>"
    );
    build_system_page(markup, title, theme)
}

/// The built-in `settings.plain` theme picker. Each theme is a native link to
/// `settings.plain?theme=<id>`, which the browser applies and re-renders.
pub fn settings_page(
    shell: &Shell,
    theme: &Theme,
    themes: &[ThemeInfo],
    themes_dir: &std::path::Path,
) -> Vec<u8> {
    let mut body = String::new();
    body.push_str(&format!(
        "<text class=\"hint\">Themes live in {}. Editing the active .ptheme file reloads it automatically. Missing or broken fields fall back to the hardcoded dark theme.</text>",
        escape(&themes_dir.display().to_string())
    ));
    body.push_str("<view class=\"themes\">");
    if themes.is_empty() {
        body.push_str("<text class=\"empty\">No themes found.</text>");
    }
    for info in themes {
        body.push_str(&format!(
            "<link href=\"settings.plain?theme={}\">{}</link>",
            escape(&info.id),
            escape(&info.name)
        ));
        if info.id == shell.theme {
            body.push_str("<text class=\"active\">active</text>");
        }
    }
    body.push_str("</view>");
    body.push_str(
        "<text class=\"section\">Theme file format</text>\
         <text class=\"hint\">JSON-like: comments, bare keys, trailing commas, and triple-quoted raw PSS blocks are accepted. A theme can set the UI colors, the default colors and sizes sites inherit, the chrome layout, the icon set, and arbitrary PSS for the chrome.</text>",
    );
    body.push_str(
        "<code>{ name: \"Mine\", dark: true, colors: { accent: \"#ff00aa\" }, sizes: { \"ui-font\": 13 }, ui: \"\"\" .pwb-tab { border-radius: 3px; } \"\"\" }</code>",
    );
    let markup = format!(
        "<plain><head><title>settings.plain</title></head><view class=\"page\">\
         <text class=\"title\">Settings</text>\
         <text class=\"section\">Theme</text>{body}</view></plain>"
    );
    build_system_page(markup, "settings.plain", theme)
}

/// Compile system-page markup with a PSS stylesheet derived from the theme, so
/// generated pages (bookmarks, history, settings) match the active theme.
fn build_system_page(markup: String, title: &str, theme: &Theme) -> Vec<u8> {
    let page = &theme.page;
    let font = theme.sizes.page_font.max(10.0);
    let small = (font * 0.82).max(9.0);
    let styles = pwb_web::parse::parse_pss_stylesheet(&format!(
        ".page {{ background-color: {}; padding: 28px; gap: 10px; }} \
         .title {{ color: {}; font-size: {}px; padding: 0px; }} \
         .section {{ color: {}; font-size: {}px; padding: 0px; }} \
         .url {{ color: {}; font-size: {}px; padding: 0px; }} \
         .hint {{ color: {}; font-size: {}px; padding: 0px; }} \
         .active {{ color: {}; font-size: {}px; padding: 0px; }} \
         .themes {{ flex-direction: row; gap: 16px; padding: 4px; }} \
         link {{ color: {}; font-size: {}px; padding: 2px; }} \
         code {{ color: {}; background-color: {}; font-size: {}px; padding: 10px; }} \
         .empty {{ color: {}; font-size: {}px; padding: 4px; }}",
        theme::color_hex(&page.background),
        theme::color_hex(&page.text),
        (font * 1.8).round(),
        theme::color_hex(&page.text),
        (font * 1.2).round(),
        theme::color_hex(&page.muted),
        small.round(),
        theme::color_hex(&page.muted),
        small.round(),
        theme::color_hex(&page.link),
        small.round(),
        theme::color_hex(&page.link),
        font.round(),
        theme::color_hex(&page.code_text),
        theme::color_hex(&page.code_bg),
        small.round(),
        theme::color_hex(&page.muted),
        font.round(),
    ));
    let mut root = pwb_web::parse::parse_pml_markup(&markup)
        .unwrap_or_else(|_| PlainNode::new(0, Tag::Document));
    pwb_web::parse::apply_stylesheet(&mut root, &styles);
    pwb_web::bundle::create_bundle(
        root,
        title.to_string(),
        None,
        None,
        HashMap::new(),
        HashMap::new(),
    )
    .serialize()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default_dark()
    }

    fn shell() -> Shell {
        // Construct directly so a developer's real session file cannot make
        // the tests depend on their machine state.
        Shell {
            tabs: vec![Tab::new("welcome.plain")],
            active: 0,
            bookmarks: Vec::new(),
            peers: 0,
            theme: "default-dark".to_string(),
            tab_scroll: 0,
            chrome_hover: None,
            path: None,
        }
    }

    /// A DOM holding just the chrome for `shell`.
    fn chrome_dom(shell: &Shell, width: u32, height: u32) -> Dom {
        let mut dom = Dom {
            title: "page".into(),
            root: DomNode {
                id: 0,
                tag: Tag::Document,
                attributes: HashMap::new(),
                text: None,
                children: vec![DomNode {
                    id: 1,
                    tag: Tag::Text,
                    attributes: HashMap::new(),
                    text: Some("page content".into()),
                    children: Vec::new(),
                }],
            },
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        install_chrome(&mut dom, shell, &theme(), width, height);
        dom
    }

    fn node_by_id<'a>(dom: &'a Dom, suffix: &str) -> &'a DomNode {
        let id = find_chrome(&dom.root, suffix).unwrap_or_else(|| panic!("chrome {suffix}"));
        crate::dom::find_node_by_id_ref(&dom.root, &id.to_string()).expect("node")
    }

    #[test]
    fn navigating_truncates_forward_history() {
        let mut shell = shell();
        shell.navigate("a.plain");
        shell.navigate("b.plain");
        assert_eq!(shell.back().as_deref(), Some("a.plain"));
        assert_eq!(shell.forward().as_deref(), Some("b.plain"));
        assert_eq!(shell.back().as_deref(), Some("a.plain"));
        shell.navigate("c.plain");
        assert_eq!(
            shell.forward(),
            None,
            "new navigation drops the forward tail"
        );
        assert_eq!(shell.current_url(), "c.plain");
    }

    #[test]
    fn tabs_close_and_select() {
        let mut shell = shell();
        shell.new_tab("second.plain");
        assert_eq!(shell.tabs.len(), 2);
        assert_eq!(shell.select(0).as_deref(), Some("welcome.plain"));
        assert_eq!(shell.close_active().as_deref(), Some("second.plain"));
        assert_eq!(shell.tabs.len(), 1);
        assert_eq!(shell.current_url(), "second.plain");

        // Closing a specific tab (its × button) selects a neighbor.
        shell.new_tab("third.plain");
        assert_eq!(shell.close_tab(0).as_deref(), Some("third.plain"));
        assert_eq!(shell.tabs.len(), 1);
        assert_eq!(shell.current_url(), "third.plain");
        // The last tab is replaced by a fresh new tab rather than closing the
        // window.
        assert_eq!(shell.close_tab(0).as_deref(), Some(NEW_TAB_URL));
        assert_eq!(shell.current_url(), NEW_TAB_URL);
    }

    #[test]
    fn back_and_forward_buttons_are_disabled_at_the_history_ends() {
        let mut tab = shell();
        assert!(!tab.can_go_back(), "a fresh tab has no history");
        assert!(!tab.can_go_forward());

        tab.navigate("a.plain");
        tab.navigate("b.plain");
        assert!(tab.can_go_back());
        assert!(!tab.can_go_forward());

        tab.back();
        assert!(tab.can_go_back());
        assert!(tab.can_go_forward());

        tab.back();
        assert!(!tab.can_go_back());
        assert!(tab.can_go_forward());
    }

    #[test]
    fn chrome_marks_back_and_forward_disabled_at_the_history_ends() {
        fn is_disabled(dom: &Dom, suffix: &str) -> bool {
            node_by_id(dom, suffix)
                .attributes
                .get("class")
                .is_some_and(|class| class.contains("pwb-disabled"))
        }

        // A fresh tab can go nowhere in either direction.
        let fresh = chrome_dom(&shell(), 1000, 700);
        assert!(is_disabled(&fresh, "back"));
        assert!(is_disabled(&fresh, "forward"));
        assert!(!is_disabled(&fresh, "reload"));
        assert_eq!(
            node_by_id(&fresh, "forward").children[0]
                .attributes
                .get("class")
                .map(String::as_str),
            Some("pwb-icon-disabled")
        );

        // After going forward and back, both arrows are live.
        let mut live = shell();
        live.navigate("a.plain");
        live.navigate("b.plain");
        live.back();
        let dom = chrome_dom(&live, 1000, 700);
        assert!(!is_disabled(&dom, "back"));
        assert!(!is_disabled(&dom, "forward"));
    }

    #[test]
    fn bookmarks_toggle_without_duplicates() {
        let mut shell = shell();
        shell.navigate("bookmark.pwb");
        assert!(!shell.is_bookmarked("bookmark.pwb"));
        assert!(shell.toggle_bookmark());
        assert!(shell.is_bookmarked("bookmark.pwb"));
        assert_eq!(shell.bookmarks.len(), 1);
        assert!(!shell.toggle_bookmark());
        assert!(shell.bookmarks.is_empty());
    }

    #[test]
    fn address_shortens_to_host_and_expands_on_focus() {
        assert_eq!(
            shorten_url("plain://mysite.pwb/deep/path?q=1"),
            "mysite.pwb"
        );
        assert_eq!(shorten_url("welcome.plain"), "welcome.plain");
        assert_eq!(shorten_url("file.pwb//tmp/some-site.pwb"), "some-site.pwb");
        assert!(shorten_url("content.pwb/abcdef0123456789abcdef").starts_with("abcdef012345"));

        let mut shell = shell();
        shell.navigate("plain://mysite.pwb/deep/path");
        assert_eq!(shell.address_display(), "mysite.pwb");

        // The field carries the full URL as its value (shown when focused) and
        // the shortened form as `data-short-url` (shown while unfocused).
        let dom = chrome_dom(&shell, 900, 650);
        let address = node_by_id(&dom, "address");
        assert_eq!(
            address.attributes.get("value").map(String::as_str),
            Some("plain://mysite.pwb/deep/path")
        );
        assert_eq!(
            address.attributes.get("data-short-url").map(String::as_str),
            Some("mysite.pwb")
        );
    }

    #[test]
    fn tab_layout_windows_overflowing_tabs_and_scroll_buttons() {
        let mut shell = shell();
        for index in 0..12 {
            shell.new_tab(&format!("site{index}.pwb"));
        }
        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let layout = tab_layout(&shell, &theme, &metrics, 900.0);
        assert!(layout.count() < shell.tabs.len(), "tabs overflow the strip");
        assert!(!layout.left_hidden, "first window starts at tab 0");
        assert!(layout.right_hidden, "more tabs are hidden to the right");
        assert!(layout.widths.iter().all(|(_, w)| *w > 40.0));

        // Scrolling right hides tabs on the left and eventually reaches the end.
        shell.tab_scroll = 4;
        let layout = tab_layout(&shell, &theme, &metrics, 900.0);
        assert!(layout.left_hidden);
        assert_eq!(layout.widths.first().map(|(index, _)| *index), Some(4));
        shell.tab_scroll = shell.tabs.len() - 1;
        let layout = tab_layout(&shell, &theme, &metrics, 900.0);
        assert!(!layout.right_hidden);
        assert_eq!(
            layout.widths.last().map(|(index, _)| *index),
            Some(shell.tabs.len() - 1)
        );
        shell.tab_scroll = 0;
        shell.scroll_tabs(-1);
        assert_eq!(shell.tab_scroll, 0, "scroll clamps at the first tab");
    }

    #[test]
    fn chrome_exposes_actions_and_keeps_ids_stable() {
        let mut shell = shell();
        shell.new_tab("two.plain");
        shell.peers = 3;
        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let first = chrome_tree(&shell, &theme, &metrics, 900.0, 1000);
        let dom = chrome_dom(&shell, 900, 650);
        assert_eq!(
            node_by_id(&dom, "chrome")
                .attributes
                .get("id")
                .map(String::as_str),
            Some("__pwb_chrome")
        );
        for suffix in [
            "tabs_row",
            "tabs",
            "new_tab",
            "address_row",
            "back",
            "forward",
            "reload",
            "address",
            "bookmark",
            "bookmarks",
            "history",
            "status",
            "settings",
            "tab_0",
            "tab_1",
        ] {
            assert!(find_chrome(&dom.root, suffix).is_some(), "missing {suffix}");
        }

        // Clicking an icon inside a control still resolves the control: the
        // buttons are views wrapping an <icon> child.
        let back = find_chrome(&dom.root, "back").expect("back");
        let back_node = node_by_id(&dom, "back");
        let glyph_id = back_node.children[0].id;
        assert_eq!(chrome_action(&dom.root, glyph_id), Some(ChromeAction::Back));
        assert_eq!(chrome_action(&dom.root, back), Some(ChromeAction::Back));
        let tab = find_chrome(&dom.root, "tab_1").expect("tab");
        assert_eq!(
            chrome_action(&dom.root, tab),
            Some(ChromeAction::SelectTab(1))
        );
        let settings = find_chrome(&dom.root, "settings").expect("settings");
        assert_eq!(
            chrome_action(&dom.root, settings),
            Some(ChromeAction::OpenSettings)
        );
        let status = find_chrome(&dom.root, "status").expect("status");
        assert_eq!(
            chrome_action(&dom.root, status),
            Some(ChromeAction::OpenNetwork)
        );

        // Rebuilding (hover changes, peer count) keeps control ids stable, so a
        // focused address field survives a chrome refresh.
        let mut again = shell.clone();
        again.chrome_hover = Some("tab_0".to_string());
        let rebuilt = chrome_dom(&again, 900, 650);
        assert_eq!(
            find_chrome(&rebuilt.root, "address"),
            find_chrome(&dom.root, "address")
        );
        assert_eq!(
            first.attributes.get("id").map(String::as_str),
            Some("__pwb_chrome")
        );
    }

    #[test]
    fn hovering_a_tab_reveals_its_close_button() {
        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);

        // Active tab always shows its ×; an inactive one only on hover.
        let mut plain = shell();
        plain.new_tab("two.plain");
        plain.new_tab("three.plain");
        let dom = chrome_dom(&plain, 900, 650);
        assert_eq!(plain.active, 2, "new_tab selects the new tab");
        assert!(find_chrome(&dom.root, "close_2").is_some(), "active tab ×");
        assert!(find_chrome(&dom.root, "close_0").is_none());
        assert!(find_chrome(&dom.root, "close_1").is_none());

        let mut hovered = plain.clone();
        hovered.chrome_hover = Some("tab_1".to_string());
        let hovered_tree = chrome_tree(&hovered, &theme, &metrics, 900.0, 1000);
        let hovered_dom = Dom {
            title: "x".into(),
            root: hovered_tree,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        assert!(
            find_chrome(&hovered_dom.root, "close_1").is_some(),
            "hovering a tab shows its × button"
        );
        let close = find_chrome(&hovered_dom.root, "close_1").expect("close");
        assert_eq!(
            chrome_action(&hovered_dom.root, close),
            Some(ChromeAction::CloseTab(1))
        );

        // A theme can pin the close button on every tab or remove it entirely.
        let mut always = theme.clone();
        always.layout.close_button = CloseButton::Always;
        let tree = chrome_tree(&plain, &always, &metrics, 900.0, 1000);
        let dom = Dom {
            title: "x".into(),
            root: tree,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        assert!(find_chrome(&dom.root, "close_2").is_some());
        let mut never = theme.clone();
        never.layout.close_button = CloseButton::Never;
        let tree = chrome_tree(&plain, &never, &metrics, 900.0, 1000);
        let dom = Dom {
            title: "x".into(),
            root: tree,
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        assert!(find_chrome(&dom.root, "close_0").is_none());
    }

    #[test]
    fn connection_badge_tracks_peers_and_stays_inside_the_bar() {
        let mut shell = shell();
        shell.peers = 3;
        assert_eq!(shell.connection_label(), "● 3 peers");
        shell.peers = 1;
        assert_eq!(shell.connection_label(), "● 1 peer");
        shell.peers = 0;
        assert_eq!(shell.connection_label(), "● offline");

        shell.peers = 2;
        let dom = chrome_dom(&shell, 900, 650);
        let status = node_by_id(&dom, "status");
        let texts: Vec<&str> = status
            .children
            .iter()
            .filter_map(|child| child.text.as_deref())
            .collect();
        assert!(texts.contains(&"●"), "green/red dot: {texts:?}");
        assert!(texts.contains(&"2"), "peer count: {texts:?}");
        assert!(status.children.iter().any(|child| child
            .attributes
            .get("class")
            .map(String::as_str)
            == Some("pwb-status-dot")));

        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let status_id = find_chrome(&dom.root, "status").expect("badge");
        let layout = crate::layout::build_layout_tree(&dom, 900, 650, 0.0);
        let boxed = crate::layout::find_box_by_id(&layout, status_id).expect("laid out");
        assert!(
            boxed.rect.right() <= 900.0,
            "badge runs past the window edge: {}",
            boxed.rect.right()
        );
        assert!(
            boxed.rect.top() < metrics.chrome_height,
            "badge belongs in the fixed top bar, got {}",
            boxed.rect.top()
        );
    }

    #[test]
    fn session_round_trips_theme_tabs_and_bookmarks_through_disk() {
        let path = std::env::temp_dir().join(format!(
            "pwb-shell-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut shell = Shell {
            path: Some(path.clone()),
            ..shell()
        };
        shell.navigate("saved.plain");
        shell.toggle_bookmark();
        shell.theme = "default-light".to_string();
        shell.save();
        let mut restored = Shell::default();
        restored.load(&path);
        assert_eq!(restored.current_url(), "saved.plain");
        assert!(restored.is_bookmarked("saved.plain"));
        assert_eq!(restored.theme, "default-light");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chrome_lays_out_as_a_fixed_two_row_bar() {
        let shell = shell();
        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let dom = chrome_dom(&shell, 900, 650);
        // Rebuilding must not stack copies of the bar.
        assert_eq!(
            dom.root
                .children
                .iter()
                .filter(
                    |child| child.attributes.get("id").map(String::as_str) == Some("__pwb_chrome")
                )
                .count(),
            1
        );

        let layout = crate::layout::build_layout_tree(&dom, 900, 650, 0.0);
        let chrome_id = find_chrome(&dom.root, "chrome").expect("chrome");
        let chrome = crate::layout::find_box_by_id(&layout, chrome_id).expect("laid out");
        assert!(
            (chrome.rect.height() - metrics.chrome_height).abs() < 1.0,
            "chrome bar is the theme's tab + toolbar height, got {}",
            chrome.rect.height()
        );
        let page = crate::layout::find_box_by_id(&layout, 1).expect("page text");
        assert!(
            page.rect.top() >= chrome.rect.bottom(),
            "page content must start below the chrome: page top {}, chrome bottom {}",
            page.rect.top(),
            chrome.rect.bottom()
        );

        // The tab strip sits above the address toolbar, and every control stays
        // inside its row and on screen.
        let strip_id = find_chrome(&dom.root, "tabs_row").expect("strip");
        let toolbar_id = find_chrome(&dom.root, "address_row").expect("toolbar");
        let strip = crate::layout::find_box_by_id(&layout, strip_id).expect("strip box");
        let toolbar = crate::layout::find_box_by_id(&layout, toolbar_id).expect("toolbar box");
        assert!(strip.rect.top() < toolbar.rect.top(), "tab strip on top");
        for (row_suffix, controls) in [
            ("tabs_row", vec!["tabs", "new_tab"]),
            (
                "address_row",
                vec![
                    "back", "forward", "reload", "address", "bookmark", "history", "settings",
                ],
            ),
        ] {
            let row_id = find_chrome(&dom.root, row_suffix).expect("row");
            let row = crate::layout::find_box_by_id(&layout, row_id).expect("row box");
            for suffix in controls {
                let id = find_chrome(&dom.root, suffix).expect("control");
                let b = crate::layout::find_box_by_id(&layout, id).expect("control box");
                assert!(
                    b.rect.top() >= row.rect.top() - 0.5
                        && b.rect.bottom() <= row.rect.bottom() + 0.5,
                    "{suffix} escapes {row_suffix}: {}..{}, row {}..{}",
                    b.rect.top(),
                    b.rect.bottom(),
                    row.rect.top(),
                    row.rect.bottom()
                );
                assert!(b.rect.right() <= 900.0, "{suffix} runs past the window");
                assert!(b.rect.width() > 0.0 && b.rect.height() > 0.0);
            }
        }

        // The address field occupies the middle of the toolbar, so it is
        // centered under the tab strip.
        let address_id = find_chrome(&dom.root, "address").expect("address");
        let address = crate::layout::find_box_by_id(&layout, address_id).expect("address box");
        let centre = address.rect.left() + address.rect.width() / 2.0;
        assert!(
            (centre - 450.0).abs() < 40.0,
            "address bar is roughly centered, centre={centre}"
        );
    }

    #[test]
    fn chrome_tree_exposes_icon_controls_from_the_theme() {
        let theme = theme();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let shell = shell();
        let tree = chrome_tree(&shell, &theme, &metrics, 900.0, 1000);
        fn find_icon(node: &DomNode) -> Option<String> {
            if node.tag == Tag::Icon {
                return node.attributes.get("icon").cloned();
            }
            node.children.iter().find_map(find_icon)
        }
        let icons: Vec<String> = collect_icons(&tree);
        assert!(icons.contains(&theme.icons.back), "back icon: {icons:?}");
        assert!(icons.contains(&theme.icons.reload));
        assert!(icons.contains(&theme.icons.new_tab));
        assert!(icons.contains(&theme.icons.settings));
        assert!(find_icon(&tree).is_some());
    }

    fn collect_icons(node: &DomNode) -> Vec<String> {
        let mut out = Vec::new();
        if node.tag == Tag::Icon {
            if let Some(icon) = node.attributes.get("icon") {
                out.push(icon.clone());
            }
        }
        for child in &node.children {
            out.extend(collect_icons(child));
        }
        out
    }

    #[test]
    fn list_and_settings_pages_are_native_links() {
        let mut shell = shell();
        shell.toggle_bookmark();
        let theme = theme();
        let bytes = list_page(&shell, &theme, "bookmarks");
        let bundle = pwb_core::wire::Bundle::deserialize(&bytes).expect("valid bundle");
        fn has_link(node: &pwb_core::spec::PlainNode) -> bool {
            (node.tag == Tag::Link && node.attributes.contains_key("href"))
                || node.children.iter().any(has_link)
        }
        assert!(
            has_link(&bundle.root_node),
            "bookmark entries must be native links"
        );

        let themes = vec![
            ThemeInfo {
                id: "default-dark".into(),
                name: "Default Dark".into(),
                builtin: true,
            },
            ThemeInfo {
                id: "midnight".into(),
                name: "Midnight".into(),
                builtin: false,
            },
        ];
        let bytes = settings_page(
            &shell,
            &theme,
            &themes,
            std::path::Path::new("/home/u/.pwb/themes"),
        );
        let bundle = pwb_core::wire::Bundle::deserialize(&bytes).expect("valid bundle");
        assert_eq!(bundle.title, "settings.plain");
        fn hrefs(node: &pwb_core::spec::PlainNode) -> Vec<String> {
            let mut out = Vec::new();
            if node.tag == Tag::Link {
                if let Some(href) = node.attributes.get("href") {
                    out.push(href.clone());
                }
            }
            for child in &node.children {
                out.extend(hrefs(child));
            }
            out
        }
        let links = hrefs(&bundle.root_node);
        assert!(
            links
                .iter()
                .any(|href| href == "settings.plain?theme=midnight"),
            "{links:?}"
        );
        assert_eq!(
            settings_theme_of("settings.plain?theme=midnight").as_deref(),
            Some("midnight")
        );
        assert_eq!(settings_theme_of("settings.plain"), None);
    }
}
