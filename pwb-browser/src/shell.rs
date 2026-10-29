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

use pwb_core::spec::Tag;

use crate::dom::{Dom, DomNode};

/// Chrome element id prefix. Page elements cannot claim these names.
pub const CHROME_PREFIX: &str = "__pwb_";

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
    path: Option<PathBuf>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            tabs: vec![Tab::new("welcome.plain")],
            active: 0,
            bookmarks: Vec::new(),
            peers: 0,
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

    pub fn new_tab(&mut self, url: &str) {
        self.tabs.push(Tab::new(url));
        self.active = self.tabs.len() - 1;
        self.save();
    }

    /// Close the active tab, returning the URL to load next. Closing the last
    /// tab replaces it with `welcome.plain` (a browser always has a page).
    pub fn close_active(&mut self) -> Option<String> {
        if self.tabs.len() <= 1 {
            let url = "welcome.plain".to_string();
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
        self.save();
        Some(self.current_url().to_string())
    }

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
            "version": 1,
            "active": self.active,
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
    SelectTab(usize),
    ToggleBookmark,
    OpenBookmarks,
    OpenHistory,
    OpenNetwork,
}

/// Resolve a clicked node to a chrome action by walking up to the nearest
/// element whose `id` starts with [`CHROME_PREFIX`]. Returns `None` for page
/// content, so normal link/JS handling is unchanged.
pub fn chrome_action(root: &DomNode, node_id: usize) -> Option<ChromeAction> {
    let chain = ancestor_ids(root, node_id);
    for id in std::iter::once(node_id).chain(chain) {
        let Some(node) = crate::dom::find_node_by_id_ref(root, &id.to_string()) else {
            continue;
        };
        let Some(element_id) = node.attributes.get("id") else {
            continue;
        };
        let Some(rest) = element_id.strip_prefix(CHROME_PREFIX) else {
            continue;
        };
        return match rest {
            "back" => Some(ChromeAction::Back),
            "forward" => Some(ChromeAction::Forward),
            "reload" => Some(ChromeAction::Reload),
            "new_tab" => Some(ChromeAction::NewTab),
            "bookmark" => Some(ChromeAction::ToggleBookmark),
            "bookmarks" => Some(ChromeAction::OpenBookmarks),
            "history" => Some(ChromeAction::OpenHistory),
            "status" => Some(ChromeAction::OpenNetwork),
            _ => rest
                .strip_prefix("tab_")
                .and_then(|index| index.parse::<usize>().ok())
                .map(ChromeAction::SelectTab),
        };
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

fn element(id: usize, tag: Tag, element_id: &str, style: &str) -> DomNode {
    let mut node = DomNode {
        id,
        tag,
        attributes: HashMap::new(),
        text: None,
        children: Vec::new(),
    };
    node.attributes
        .insert("id".to_string(), element_id.to_string());
    node.attributes
        .insert("style".to_string(), style.to_string());
    node
}

/// Height of the fixed chrome bar. Layout reserves this much space above the
/// page so content starts below the bar instead of underneath it.
pub const CHROME_HEIGHT: f32 = 92.0;

/// A chrome button whose label is drawn by the button's own text, which is
/// what the layout and renderer measure. A child `<text>` node would be
/// ignored for leaf tags (buttons lay out no children) and the renderer would
/// fall back to its placeholder label.
fn chrome_button(id: usize, suffix: &str, style: &str, label: &str, title: &str) -> DomNode {
    let mut button = element(id, Tag::Button, &format!("{CHROME_PREFIX}{suffix}"), style);
    button.text = Some(label.to_string());
    button
        .attributes
        .insert("title".to_string(), title.to_string());
    button
}

/// Build the fixed chrome tree for `shell`. Node ids start at `first_id` so
/// repeated injections never collide with the page's own nodes.
pub fn chrome_tree(shell: &Shell, first_id: usize) -> DomNode {
    let mut next = first_id;
    let mut take = || {
        let id = next;
        next += 1;
        id
    };

    let bar_style = format!(
        "position: fixed; top: 0; left: 0; width: 100%; height: {CHROME_HEIGHT}px; \
         background-color: #17171c; color: #e6e6ef; padding: 6px; gap: 4px; \
         flex-direction: column; border-bottom: 1px solid #2d2d3a;"
    );
    // Rows set their own padding: 0 — a View otherwise gets the 3.33vh default
    // (≈22px), which is larger than the row itself and pushes every control
    // below it.
    let row_style = "flex-direction: row; gap: 4px; height: 32px; align-items: center; padding: 0;";
    // Chrome controls use fixed pixel type and heights: the bar does not grow
    // with the viewport, so a 2vh label would outgrow its 32px row on a tall
    // window and overlap the row below it.
    let button_style = "background-color: #262631; color: #e6e6ef; padding: 4px; \
         border-radius: 4px; font-size: 13px;";
    let active_tab_style = "background-color: #3b3b4d; color: #ffffff; padding: 4px; \
         border-radius: 4px; font-size: 13px;";
    // 45% leaves room for every text-labelled control at the minimum window
    // width (760px); the address bar grows with the window from there.
    let address_style = "background-color: #0f0f14; color: #ffffff; padding: 4px; \
         width: 45%; height: 24px; border-radius: 4px; font-size: 13px;";

    let mut chrome = element(
        take(),
        Tag::View,
        &format!("{CHROME_PREFIX}chrome"),
        &bar_style,
    );
    chrome
        .attributes
        .insert("title".to_string(), "PlainWeb browser shell".to_string());

    let mut tabs_row = element(
        take(),
        Tag::View,
        &format!("{CHROME_PREFIX}tabs_row"),
        row_style,
    );
    for (index, tab) in shell.tabs.iter().enumerate() {
        let label = if tab.title.is_empty() {
            &tab.url
        } else {
            &tab.title
        };
        let label = if label.chars().count() > 24 {
            let mut short: String = label.chars().take(21).collect();
            short.push('…');
            short
        } else {
            label.clone()
        };
        let style = if index == shell.active {
            active_tab_style
        } else {
            button_style
        };
        tabs_row.children.push(chrome_button(
            take(),
            &format!("tab_{index}"),
            style,
            &label,
            &tab.url,
        ));
    }
    tabs_row.children.push(chrome_button(
        take(),
        "new_tab",
        button_style,
        "+",
        "New tab (Ctrl/Cmd+T)",
    ));
    chrome.children.push(tabs_row);

    let mut address_row = element(
        take(),
        Tag::View,
        &format!("{CHROME_PREFIX}address_row"),
        row_style,
    );
    // ASCII labels: the bundled font has no arrow glyphs, so "←"/"→"/"⟳"
    // painted as a dash and missing-glyph boxes.
    for (suffix, label, title) in [
        ("back", "Back", "Back (Alt+Left)"),
        ("forward", "Fwd", "Forward (Alt+Right)"),
        ("reload", "Reload", "Reload (Ctrl+R)"),
    ] {
        address_row
            .children
            .push(chrome_button(take(), suffix, button_style, label, title));
    }

    let mut address = element(
        take(),
        Tag::Input,
        &format!("{CHROME_PREFIX}address"),
        address_style,
    );
    address
        .attributes
        .insert("value".to_string(), shell.current_url().to_string());
    address
        .attributes
        .insert("placeholder".to_string(), "address or search".to_string());
    address_row.children.push(address);

    let bookmarked = shell.is_bookmarked(shell.current_url());
    address_row.children.push(chrome_button(
        take(),
        "bookmark",
        button_style,
        if bookmarked { "Saved" } else { "Save" },
        "Bookmark this page (Ctrl+D)",
    ));

    for (suffix, label, title) in [
        ("bookmarks", "Bookmarks", "Open bookmarks"),
        ("history", "History", "Open history"),
    ] {
        address_row
            .children
            .push(chrome_button(take(), suffix, button_style, label, title));
    }

    // Connection badge: the one piece of state a new user needs at a glance.
    // It is a button so clicking it opens `network.plain` with the details.
    let (status_style, status_title) = if shell.peers == 0 {
        (
            "background-color: #2a1f1f; color: #d18f8f; padding: 4px; \
             border-radius: 4px; font-size: 12px;",
            "No peers: this browser is not connected to a swarm. Click for network.plain",
        )
    } else {
        (
            "background-color: #1f2a1f; color: #8fd18f; padding: 4px; \
             border-radius: 4px; font-size: 12px;",
            "Connected peers. Click for network.plain",
        )
    };
    address_row.children.push(chrome_button(
        take(),
        "status",
        status_style,
        &shell.connection_label(),
        status_title,
    ));

    chrome.children.push(address_row);
    chrome
}

/// Replace the chrome subtree in `dom` with a fresh one for `shell`.
pub fn install_chrome(dom: &mut Dom, shell: &Shell) {
    dom.root
        .children
        .retain(|child| child.attributes.get("id").map(String::as_str) != Some("__pwb_chrome"));
    let next_id = max_node_id(&dom.root) + 1;
    let chrome = chrome_tree(shell, next_id);
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

/// A generated `.pwb` page listing bookmarks or session history. The links are
/// native `<link>` elements, so clicking one navigates without JavaScript.
pub fn list_page(shell: &Shell, kind: &str) -> Vec<u8> {
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
    let mut root = pwb_web::parse::parse_pml_markup(&markup)
        .unwrap_or_else(|_| pwb_core::spec::PlainNode::new(0, Tag::Document));
    let styles = pwb_web::parse::parse_pss_stylesheet(
        ".page { padding: 110px; gap: 6px; background-color: #101014; } \
         .title { font-size: 3vh; color: #ffffff; padding: 6px; } \
         .url { font-size: 1.8vh; color: #8a8a9a; padding: 2px; } \
         link { color: #7ab7ff; font-size: 2.2vh; padding: 3px; } \
         .empty { color: #8a8a9a; font-size: 2.2vh; padding: 8px; }",
    );
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

    fn shell() -> Shell {
        // Construct directly so a developer's real session file cannot make
        // the tests depend on their machine state.
        Shell {
            tabs: vec![Tab::new("welcome.plain")],
            active: 0,
            bookmarks: Vec::new(),
            peers: 0,
            path: None,
        }
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
    fn chrome_tree_exposes_actions() {
        let mut shell = shell();
        shell.new_tab("two.plain");
        let chrome = chrome_tree(&shell, 1000);
        assert_eq!(
            chrome.attributes.get("id").map(String::as_str),
            Some("__pwb_chrome")
        );
        let dom = Dom {
            title: "x".into(),
            root: DomNode {
                id: 0,
                tag: Tag::Document,
                attributes: HashMap::new(),
                text: None,
                children: vec![chrome],
            },
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        assert!(find_chrome(&dom.root, "address").is_some());
        let tab_id = find_chrome(&dom.root, "tab_1").expect("second tab");
        assert_eq!(
            chrome_action(&dom.root, tab_id),
            Some(ChromeAction::SelectTab(1))
        );
        let back_id = find_chrome(&dom.root, "back").expect("back");
        assert_eq!(chrome_action(&dom.root, back_id), Some(ChromeAction::Back));

        // The connection badge is clickable and names the current state.
        let status_id = find_chrome(&dom.root, "status").expect("connection badge");
        assert_eq!(
            chrome_action(&dom.root, status_id),
            Some(ChromeAction::OpenNetwork)
        );
        assert_eq!(shell.connection_label(), "● offline");
        // Every chrome button carries its label as text (a `<text>` child is
        // ignored for leaf tags, which is what produced literal "Button"
        // labels and 8px-wide boxes).
        for suffix in [
            "back",
            "forward",
            "reload",
            "bookmark",
            "bookmarks",
            "history",
        ] {
            let id = find_chrome(&dom.root, suffix).expect("chrome button");
            let node = crate::dom::find_node_by_id_ref(&dom.root, &id.to_string()).unwrap();
            assert!(node.text.as_deref().is_some_and(|t| !t.is_empty()));
            assert!(node.children.is_empty());
        }
    }

    #[test]
    fn connection_badge_tracks_peers_and_stays_inside_the_bar() {
        let mut shell = shell();
        shell.peers = 3;
        assert_eq!(shell.connection_label(), "● 3 peers");
        shell.peers = 1;
        assert_eq!(shell.connection_label(), "● 1 peer");

        shell.peers = 2;
        let mut dom = Dom {
            title: "page".into(),
            root: DomNode {
                id: 0,
                tag: Tag::Document,
                attributes: HashMap::new(),
                text: None,
                children: Vec::new(),
            },
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        install_chrome(&mut dom, &shell);
        let status = find_chrome(&dom.root, "status").expect("badge");
        assert_eq!(
            crate::dom::find_node_by_id_ref(&dom.root, &status.to_string())
                .and_then(|node| node.text.clone()),
            Some("● 2 peers".to_string())
        );

        // The badge must not be clipped off the right edge of the chrome bar.
        let layout = crate::layout::build_layout_tree(&dom, 900, 650, 0.0);
        let boxed = crate::layout::find_box_by_id(&layout, status).expect("laid out");
        assert!(
            boxed.rect.right() <= 900.0,
            "badge runs past the window edge: {}",
            boxed.rect.right()
        );
        assert!(
            boxed.rect.top() < 100.0,
            "badge belongs in the fixed top bar, got {}",
            boxed.rect.top()
        );
    }

    #[test]
    fn session_round_trips_through_disk() {
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
        shell.save();
        let mut restored = Shell::default();
        restored.load(&path);
        assert_eq!(restored.current_url(), "saved.plain");
        assert!(restored.is_bookmarked("saved.plain"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chrome_lays_out_as_a_fixed_top_bar() {
        let shell = shell();
        let page_text = DomNode {
            id: 1,
            tag: Tag::Text,
            attributes: HashMap::new(),
            text: Some("page content".into()),
            children: Vec::new(),
        };
        let mut dom = Dom {
            title: "page".into(),
            root: DomNode {
                id: 0,
                tag: Tag::Document,
                attributes: HashMap::new(),
                text: None,
                children: vec![page_text],
            },
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        install_chrome(&mut dom, &shell);
        install_chrome(&mut dom, &shell);
        assert_eq!(
            dom.root
                .children
                .iter()
                .filter(
                    |child| child.attributes.get("id").map(String::as_str) == Some("__pwb_chrome")
                )
                .count(),
            1,
            "rebuilding the chrome must not stack copies"
        );
        let layout = crate::layout::build_layout_tree(&dom, 900, 650, 0.0);
        let address = find_chrome(&dom.root, "address").expect("address bar");
        let boxed = crate::layout::find_box_by_id(&layout, address).expect("laid out");
        assert!(
            boxed.rect.top() < 100.0,
            "address bar sits in the fixed top bar, got {}",
            boxed.rect.top()
        );
    }

    #[test]
    fn list_page_serializes_with_links() {
        let mut shell = shell();
        shell.toggle_bookmark();
        let bytes = list_page(&shell, "bookmarks");
        let bundle = pwb_core::wire::Bundle::deserialize(&bytes).expect("valid bundle");
        fn has_link(node: &pwb_core::spec::PlainNode) -> bool {
            (node.tag == Tag::Link && node.attributes.contains_key("href"))
                || node.children.iter().any(has_link)
        }
        assert!(
            has_link(&bundle.root_node),
            "bookmark entries must be native links"
        );
    }

    #[test]
    fn chrome_reserves_space_and_controls_stay_inside_their_rows() {
        let shell = shell();
        let page_text = DomNode {
            id: 1,
            tag: Tag::Text,
            attributes: HashMap::new(),
            text: Some("page content".into()),
            children: Vec::new(),
        };
        let mut dom = Dom {
            title: "page".into(),
            root: DomNode {
                id: 0,
                tag: Tag::Document,
                attributes: HashMap::new(),
                text: None,
                children: vec![page_text],
            },
            components: HashMap::new(),
            modules: HashMap::new(),
        };
        install_chrome(&mut dom, &shell);

        let layout = crate::layout::build_layout_tree(&dom, 900, 650, 0.0);
        let chrome_id = find_chrome(&dom.root, "chrome").expect("chrome");
        let chrome = crate::layout::find_box_by_id(&layout, chrome_id).expect("laid out");
        let page = crate::layout::find_box_by_id(&layout, 1).expect("page text");
        assert_eq!(chrome.rect.height(), CHROME_HEIGHT);
        assert!(
            page.rect.top() >= chrome.rect.bottom(),
            "page content must start below the chrome: page top {}, chrome bottom {}",
            page.rect.top(),
            chrome.rect.bottom()
        );

        // Every control stays within its row's box (the row used to advance by
        // its tallest child while drawing 32px, so the address row overlapped
        // the tab row and the controls spilled below the bar).
        for (row_suffix, controls) in [
            ("tabs_row", vec!["tab_0", "new_tab"]),
            (
                "address_row",
                vec![
                    "back",
                    "forward",
                    "reload",
                    "bookmark",
                    "bookmarks",
                    "history",
                    "status",
                ],
            ),
        ] {
            let row_id = find_chrome(&dom.root, row_suffix).expect("row");
            let row = crate::layout::find_box_by_id(&layout, row_id).expect("laid out row");
            for suffix in controls {
                let id = find_chrome(&dom.root, suffix).expect("control");
                let b = crate::layout::find_box_by_id(&layout, id).expect("laid out control");
                assert!(
                    b.rect.top() >= row.rect.top() - 0.5
                        && b.rect.bottom() <= row.rect.bottom() + 0.5,
                    "{suffix} escapes {row_suffix}: control {}..{}, row {}..{}",
                    b.rect.top(),
                    b.rect.bottom(),
                    row.rect.top(),
                    row.rect.bottom()
                );
                assert!(b.rect.right() <= 900.0, "{suffix} runs past the window");
            }
        }
    }
}
