//! Browser themes (`pwb-browser::theme`).
//!
//! A theme is one `.ptheme` file in `$PWB_DATA/themes` (default
//! `~/.pwb/themes`). The format is JSON-like — comments, unquoted keys,
//! trailing commas, and triple-quoted raw blocks are all accepted — so a theme
//! can carry real PSS without escaping it:
//!
//! ```text
//! // ~/.pwb/themes/midnight.ptheme
//! {
//!   name: "Midnight",
//!   dark: true,
//!   colors: { chrome: "#141419", accent: "#8ab4ff" },
//!   sizes: { "ui-font": 13, "tab-height": 38, "page-font": 17 },
//!   icons: { back: "arrow-left", reload: "refresh-cw" },
//!   ui: """
//!     .pwb-tab { border-radius: 0px; }
//!   """,
//! }
//! ```
//!
//! Every field is optional and falls back to [`Theme::default_dark`], so a
//! theme only has to state what it changes. Themes are re-read while the
//! browser runs: the app polls the active file and rebuilds the chrome when it
//! changes.
//!
//! A theme controls five things:
//!
//! 1. **UI colors** — [`UiColors`], rendered through the generated chrome PSS.
//! 2. **Site default colors** — [`PageColors`]: the fallback colors pages get
//!    when their own PSS says nothing. Authored PSS always wins.
//! 3. **UI layout** — [`ChromeLayout`] knobs plus the raw `ui` PSS block, which
//!    is applied after the generated rules and can restyle anything.
//! 4. **Icon set** — [`IconSet`] maps each chrome role to a feather glyph, and
//!    `icons-custom` can add new glyphs drawn from raw path data.
//! 5. **Sizes** — [`Sizes`] in pixels, scaled gently by window size instead of
//!    being derived from `vh` (which made controls tiny on short windows and
//!    huge on tall ones).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use pwb_core::spec::Color;

// ---------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------

/// Parse `#rgb`, `#rrggbb`, or a small set of names. Alpha is always opaque.
pub fn parse_color(value: &str) -> Option<Color> {
    let clean = value.trim().trim_start_matches('#');
    let named = |r: u8, g: u8, b: u8| Some(Color { r, g, b, a: 255 });
    match clean.to_ascii_lowercase().as_str() {
        "white" => return Some(Color::WHITE),
        "black" => return Some(Color::BLACK),
        "transparent" | "none" => return Some(Color::TRANSPARENT),
        "red" => return named(255, 0, 0),
        "green" => return named(0, 128, 0),
        "blue" => return named(0, 0, 255),
        "gray" | "grey" => return named(128, 128, 128),
        _ => {}
    }
    if !clean.is_ascii() {
        return None;
    }
    match clean.len() {
        3 => {
            let r = u8::from_str_radix(&clean[0..1], 16).ok()?;
            let g = u8::from_str_radix(&clean[1..2], 16).ok()?;
            let b = u8::from_str_radix(&clean[2..3], 16).ok()?;
            Some(Color {
                r: r * 17,
                g: g * 17,
                b: b * 17,
                a: 255,
            })
        }
        6 => {
            let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
            let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
            let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
            Some(Color { r, g, b, a: 255 })
        }
        _ => None,
    }
}

/// `#rrggbb` for a color (alpha dropped: the renderer's fallback colors are
/// opaque).
pub fn color_hex(color: &Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

// ---------------------------------------------------------------------------
// Theme model
// ---------------------------------------------------------------------------

/// Chrome colors.
#[derive(Debug, Clone, PartialEq)]
pub struct UiColors {
    pub chrome: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub surface_active: Color,
    pub border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub address_bg: Color,
    pub address_text: Color,
    pub tab_active: Color,
    pub tab_inactive: Color,
    pub tab_hover: Color,
    pub ok: Color,
    pub danger: Color,
    pub selection: Color,
}

/// Fallback colors for pages that do not style themselves. These are defaults,
/// not overrides: a site's own PSS always wins.
#[derive(Debug, Clone, PartialEq)]
pub struct PageColors {
    pub background: Color,
    pub text: Color,
    pub muted: Color,
    pub link: Color,
    pub button_bg: Color,
    pub button_text: Color,
    pub input_bg: Color,
    pub input_text: Color,
    pub input_border: Color,
    pub placeholder: Color,
    pub code_bg: Color,
    pub code_text: Color,
    /// Warning text ("nothing here yet", offline notes).
    pub warn: Color,
    /// Error text (failed fetches, invalid input).
    pub danger: Color,
    pub icon: Color,
    pub scrollbar_track: Color,
    pub scrollbar_thumb: Color,
    pub selection: Color,
}

/// Pixel sizes for the chrome and the page defaults. Values are in CSS px and
/// scaled gently by [`Metrics`]/[`PageDefaults`] for the current window.
#[derive(Debug, Clone, PartialEq)]
pub struct Sizes {
    pub ui_font: f32,
    pub ui_font_small: f32,
    pub tab_height: f32,
    pub toolbar_height: f32,
    pub control: f32,
    pub gap: f32,
    pub radius: f32,
    pub icon: f32,
    pub page_font: f32,
    pub page_padding: f32,
    pub page_gap: f32,
}

/// When a tab shows its close button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseButton {
    /// Hovered tabs (and the active one) show it.
    Hover,
    /// Every tab always shows it.
    Always,
    /// Never; Ctrl/Cmd+W still closes.
    Never,
}

impl CloseButton {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hover" | "auto" => Some(Self::Hover),
            "always" | "all" | "true" => Some(Self::Always),
            "never" | "none" | "false" => Some(Self::Never),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabsAlign {
    Start,
    Center,
    End,
}

impl TabsAlign {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "start" | "left" => Some(Self::Start),
            "center" => Some(Self::Center),
            "end" | "right" => Some(Self::End),
            _ => None,
        }
    }

    fn justify(self) -> &'static str {
        match self {
            Self::Start => "flex-start",
            Self::Center => "center",
            Self::End => "flex-end",
        }
    }
}

/// Chrome layout knobs. Anything beyond these can be expressed in the raw `ui`
/// PSS block.
#[derive(Debug, Clone, PartialEq)]
pub struct ChromeLayout {
    pub close_button: CloseButton,
    /// Width of the address area as a percentage of the toolbar.
    pub address_width: f32,
    pub tab_min_width: f32,
    pub tab_max_width: f32,
    pub tabs_align: TabsAlign,
    /// Show `<`/`>` tab-scroll buttons when tabs overflow.
    pub scroll_buttons: bool,
}

/// Chrome icon roles → feather glyph names (or `icons-custom` names).
#[derive(Debug, Clone, PartialEq)]
pub struct IconSet {
    pub back: String,
    pub forward: String,
    pub reload: String,
    pub new_tab: String,
    pub close: String,
    pub bookmark: String,
    pub bookmarks: String,
    pub history: String,
    pub settings: String,
    pub network: String,
    pub tab_left: String,
    pub tab_right: String,
    pub home: String,
}

/// One theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// File stem / builtin id (`default-dark`).
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub colors: UiColors,
    pub page: PageColors,
    pub sizes: Sizes,
    pub layout: ChromeLayout,
    pub icons: IconSet,
    /// Extra glyphs by name, drawn from raw `M`/`L`/`C`/`Z` path data.
    pub custom_icons: Vec<(String, String)>,
    /// PSS applied to the chrome after the generated rules.
    pub ui_pss: String,
    /// PSS applied to pages as *fallback* attributes (only where the page does
    /// not already set the property).
    pub page_pss: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}

impl Theme {
    /// The hardcoded dark theme. Every other theme is layered over this one, so
    /// it is also the fallback for broken or partial `.ptheme` files.
    pub fn default_dark() -> Self {
        Self {
            id: "default-dark".to_string(),
            name: "Default Dark".to_string(),
            dark: true,
            colors: UiColors {
                chrome: hex("#1c1c22"),
                surface: hex("#26262f"),
                surface_hover: hex("#32323d"),
                surface_active: hex("#3a3a47"),
                border: hex("#33333f"),
                text: hex("#e9e9f0"),
                text_muted: hex("#9b9baa"),
                accent: hex("#8ab4ff"),
                accent_text: hex("#0d1017"),
                address_bg: hex("#141419"),
                address_text: hex("#f2f2f8"),
                tab_active: hex("#3a3a47"),
                tab_inactive: hex("#26262f"),
                tab_hover: hex("#2f2f3a"),
                ok: hex("#5fd08a"),
                danger: hex("#ff6b6b"),
                selection: hex("#3d5a8a"),
            },
            page: PageColors {
                background: hex("#141419"),
                text: hex("#e8e8ef"),
                muted: hex("#9a9aa8"),
                link: hex("#7fb3ff"),
                button_bg: hex("#3a3a47"),
                button_text: hex("#f5f5fa"),
                input_bg: hex("#1b1b22"),
                input_text: hex("#f2f2f8"),
                input_border: hex("#3d3d4a"),
                placeholder: hex("#80808e"),
                code_bg: hex("#1e1e26"),
                code_text: hex("#d7d7e2"),
                warn: hex("#e8c07a"),
                danger: hex("#ff8080"),
                icon: hex("#a8d5b5"),
                scrollbar_track: hex("#1b1b20"),
                scrollbar_thumb: hex("#4a4a56"),
                selection: hex("#3d5a8a"),
            },
            sizes: Sizes {
                ui_font: 14.0,
                ui_font_small: 12.0,
                tab_height: 40.0,
                toolbar_height: 46.0,
                control: 30.0,
                gap: 7.0,
                radius: 9.0,
                icon: 17.0,
                page_font: 18.0,
                page_padding: 20.0,
                page_gap: 12.0,
            },
            layout: ChromeLayout {
                close_button: CloseButton::Hover,
                address_width: 50.0,
                tab_min_width: 96.0,
                tab_max_width: 230.0,
                tabs_align: TabsAlign::Start,
                scroll_buttons: true,
            },
            icons: IconSet {
                back: "chevron-left".to_string(),
                forward: "chevron-right".to_string(),
                reload: "rotate-cw".to_string(),
                new_tab: "plus".to_string(),
                close: "x".to_string(),
                bookmark: "bookmark".to_string(),
                bookmarks: "book-open".to_string(),
                history: "clock".to_string(),
                settings: "settings".to_string(),
                network: "wifi".to_string(),
                tab_left: "chevron-left".to_string(),
                tab_right: "chevron-right".to_string(),
                home: "home".to_string(),
            },
            custom_icons: Vec::new(),
            ui_pss: String::new(),
            page_pss: String::new(),
        }
    }

    /// The hardcoded light theme.
    pub fn default_light() -> Self {
        let mut theme = Self::default_dark();
        theme.id = "default-light".to_string();
        theme.name = "Default Light".to_string();
        theme.dark = false;
        theme.colors = UiColors {
            chrome: hex("#e9e9ee"),
            surface: hex("#f7f7fa"),
            surface_hover: hex("#ececf2"),
            surface_active: hex("#ffffff"),
            border: hex("#d5d5de"),
            text: hex("#1c1c22"),
            text_muted: hex("#6b6b78"),
            accent: hex("#2f6fdb"),
            accent_text: hex("#ffffff"),
            address_bg: hex("#ffffff"),
            address_text: hex("#17171c"),
            tab_active: hex("#ffffff"),
            tab_inactive: hex("#e4e4ea"),
            tab_hover: hex("#ededf2"),
            ok: hex("#1f8a4c"),
            danger: hex("#c23a3a"),
            selection: hex("#bcd3f7"),
        };
        theme.page = PageColors {
            background: hex("#fbfbfd"),
            text: hex("#1b1b21"),
            muted: hex("#5f5f6b"),
            link: hex("#1f5fbf"),
            button_bg: hex("#e8eaf0"),
            button_text: hex("#1b1b21"),
            input_bg: hex("#ffffff"),
            input_text: hex("#1b1b21"),
            input_border: hex("#c9c9d4"),
            placeholder: hex("#8b8b98"),
            code_bg: hex("#f2f2f7"),
            code_text: hex("#24242c"),
            warn: hex("#8a5a00"),
            danger: hex("#b03030"),
            icon: hex("#2f6f4f"),
            scrollbar_track: hex("#ededf2"),
            scrollbar_thumb: hex("#b9b9c6"),
            selection: hex("#bcd3f7"),
        };
        theme
    }

    /// Builtin theme by id, if `id` names one.
    pub fn builtin(id: &str) -> Option<Self> {
        match id {
            "default-dark" | "dark" => Some(Self::default_dark()),
            "default-light" | "light" => Some(Self::default_light()),
            _ => None,
        }
    }

    /// Look up a custom icon glyph by name.
    pub fn custom_icon(&self, name: &str) -> Option<&str> {
        self.custom_icons
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, data)| data.as_str())
    }

    /// Parse a `.ptheme` file. Missing fields fall back to the dark theme.
    pub fn parse(source: &str) -> Result<Self, String> {
        let value = Parser::new(source).parse_document()?;
        let Value::Map(map) = value else {
            return Err("theme file must contain a key/value object".to_string());
        };
        Ok(Self::from_value(&map))
    }

    fn from_value(map: &[(String, Value)]) -> Self {
        let mut theme = Self::default_dark();
        if let Some(name) = get_str(map, &["name", "title"]) {
            theme.name = name;
        }
        if let Some(dark) = get_bool(map, &["dark", "is-dark"]) {
            theme.dark = dark;
        }
        if let Some(group) = get_map(map, &["colors", "ui-colors", "ui"]) {
            apply_ui_colors(&mut theme.colors, group);
        }
        if let Some(group) = get_map(map, &["page", "page-colors", "site", "site-colors"]) {
            apply_page_colors(&mut theme.page, group);
        }
        if let Some(group) = get_map(map, &["sizes", "size"]) {
            apply_sizes(&mut theme.sizes, group);
        }
        if let Some(group) = get_map(map, &["layout", "chrome"]) {
            apply_layout(&mut theme.layout, group);
        }
        if let Some(group) = get_map(map, &["icons", "icon-set"]) {
            apply_icons(&mut theme.icons, group);
        }
        if let Some(group) = get_map(map, &["icons-custom", "custom-icons", "glyphs"]) {
            for (name, value) in group {
                if let Value::Str(data) = value {
                    theme.custom_icons.push((normalize_key(name), data.clone()));
                }
            }
        }
        if let Some(pss) = get_str(map, &["ui-pss", "ui-style", "ui-css", "ui", "style"]) {
            theme.ui_pss = pss;
        }
        if let Some(pss) = get_str(map, &["page-pss", "page-style", "page-css", "defaults-pss"]) {
            theme.page_pss = pss;
        }
        theme
    }

    /// Serialize back to `.ptheme` text (used to seed the builtin files).
    pub fn to_ptheme(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "// PlainWeb browser theme. Delete this file to restore the builtin default.\n",
        );
        out.push_str("{\n");
        out.push_str(&format!("  name: {:?},\n", self.name));
        out.push_str(&format!("  dark: {},\n", self.dark));
        out.push_str("\n  // Chrome (browser UI) colors.\n  colors: {\n");
        let c = &self.colors;
        for (key, color) in [
            ("chrome", &c.chrome),
            ("surface", &c.surface),
            ("surface-hover", &c.surface_hover),
            ("surface-active", &c.surface_active),
            ("border", &c.border),
            ("text", &c.text),
            ("text-muted", &c.text_muted),
            ("accent", &c.accent),
            ("accent-text", &c.accent_text),
            ("address-bg", &c.address_bg),
            ("address-text", &c.address_text),
            ("tab-active", &c.tab_active),
            ("tab-inactive", &c.tab_inactive),
            ("tab-hover", &c.tab_hover),
            ("ok", &c.ok),
            ("danger", &c.danger),
            ("selection", &c.selection),
        ] {
            out.push_str(&format!("    \"{key}\": \"{}\",\n", color_hex(color)));
        }
        out.push_str(
            "  },\n\n  // Defaults for pages that do not style themselves (PSS wins).\n  page: {\n",
        );
        let p = &self.page;
        for (key, color) in [
            ("background", &p.background),
            ("text", &p.text),
            ("muted", &p.muted),
            ("link", &p.link),
            ("button-bg", &p.button_bg),
            ("button-text", &p.button_text),
            ("input-bg", &p.input_bg),
            ("input-text", &p.input_text),
            ("input-border", &p.input_border),
            ("placeholder", &p.placeholder),
            ("code-bg", &p.code_bg),
            ("code-text", &p.code_text),
            ("warn", &p.warn),
            ("danger", &p.danger),
            ("icon", &p.icon),
            ("scrollbar-track", &p.scrollbar_track),
            ("scrollbar-thumb", &p.scrollbar_thumb),
            ("selection", &p.selection),
        ] {
            out.push_str(&format!("    \"{key}\": \"{}\",\n", color_hex(color)));
        }
        out.push_str("  },\n\n  // Pixel sizes, scaled gently with the window.\n  sizes: {\n");
        let s = &self.sizes;
        for (key, value) in [
            ("ui-font", s.ui_font),
            ("ui-font-small", s.ui_font_small),
            ("tab-height", s.tab_height),
            ("toolbar-height", s.toolbar_height),
            ("control", s.control),
            ("gap", s.gap),
            ("radius", s.radius),
            ("icon", s.icon),
            ("page-font", s.page_font),
            ("page-padding", s.page_padding),
            ("page-gap", s.page_gap),
        ] {
            out.push_str(&format!("    \"{key}\": {value},\n"));
        }
        out.push_str("  },\n\n  layout: {\n");
        out.push_str(&format!(
            "    \"close-button\": \"{}\",\n",
            match self.layout.close_button {
                CloseButton::Hover => "hover",
                CloseButton::Always => "always",
                CloseButton::Never => "never",
            }
        ));
        out.push_str(&format!(
            "    \"address-width\": {},\n    \"tab-min-width\": {},\n    \"tab-max-width\": {},\n",
            self.layout.address_width, self.layout.tab_min_width, self.layout.tab_max_width
        ));
        out.push_str(&format!(
            "    \"tabs-align\": \"{}\",\n    \"scroll-buttons\": {},\n",
            match self.layout.tabs_align {
                TabsAlign::Start => "start",
                TabsAlign::Center => "center",
                TabsAlign::End => "end",
            },
            self.layout.scroll_buttons
        ));
        out.push_str("  },\n\n  icons: {\n");
        let i = &self.icons;
        for (key, value) in [
            ("back", &i.back),
            ("forward", &i.forward),
            ("reload", &i.reload),
            ("new-tab", &i.new_tab),
            ("close", &i.close),
            ("bookmark", &i.bookmark),
            ("bookmarks", &i.bookmarks),
            ("history", &i.history),
            ("settings", &i.settings),
            ("network", &i.network),
            ("tab-left", &i.tab_left),
            ("tab_right", &i.tab_right),
            ("home", &i.home),
        ] {
            out.push_str(&format!("    \"{key}\": {value:?},\n"));
        }
        out.push_str("  },\n");
        if !self.custom_icons.is_empty() {
            out.push_str("\n  icons-custom: {\n");
            for (name, data) in &self.custom_icons {
                out.push_str(&format!("    \"{name}\": {data:?},\n"));
            }
            out.push_str("  },\n");
        }
        if !self.ui_pss.trim().is_empty() {
            out.push_str("\n  ui: \"\"\"\n");
            out.push_str(&self.ui_pss);
            out.push_str("\n  \"\"\",\n");
        }
        if !self.page_pss.trim().is_empty() {
            out.push_str("\n  page-pss: \"\"\"\n");
            out.push_str(&self.page_pss);
            out.push_str("\n  \"\"\",\n");
        }
        out.push_str("}\n");
        out
    }

    /// The generated chrome stylesheet (before the theme's own `ui` PSS).
    pub fn chrome_css(&self, m: &Metrics) -> String {
        let c = &self.colors;
        let mut css = String::new();
        let px = |v: f32| format!("{}px", v);
        css.push_str(&format!(
            ".pwb-chrome {{ position: fixed; top: 0px; left: 0px; width: 100%; height: {}px; \
             background-color: {}; flex-direction: column; gap: 0px; padding: 0px; \
             border-bottom: 1px solid {}; }}\n",
            m.chrome_height,
            color_hex(&c.chrome),
            color_hex(&c.border)
        ));
        css.push_str(&format!(
            ".pwb-tabstrip {{ flex-direction: row; width: 100%; height: {}px; \
             align-items: center; justify-content: space-between; gap: {}px; padding: 4px; \
             background-color: {}; }}\n",
            m.tab_height,
            m.gap,
            color_hex(&c.chrome)
        ));
        css.push_str(&format!(
            ".pwb-tabs {{ flex-direction: row; height: {}px; align-items: center; \
             justify-content: {}; gap: {}px; padding: 0px; background-color: {}; }}\n",
            m.tab_height - 10.0,
            self.layout.tabs_align.justify(),
            m.gap,
            color_hex(&c.chrome)
        ));
        let tab_h = m.tab_height - 10.0;
        // `space-between` pins the close button to the tab's right edge no
        // matter how short the label is; the label itself is truncated to the
        // width that is left over, so text never runs under the ×.
        css.push_str(&format!(
            ".pwb-tab {{ flex-direction: row; height: {}px; padding: 6px; align-items: center; \
             justify-content: space-between; gap: {}px; border-radius: {}px; \
             background-color: {}; overflow: hidden; cursor: pointer; }}\n",
            tab_h,
            m.gap.min(6.0),
            m.radius,
            color_hex(&c.tab_inactive)
        ));
        css.push_str(&format!(
            ".pwb-tab.active {{ background-color: {}; }}\n",
            color_hex(&c.tab_active)
        ));
        css.push_str(&format!(
            ".pwb-tab.hovered {{ background-color: {}; }}\n",
            color_hex(&c.tab_hover)
        ));
        css.push_str(&format!(
            ".pwb-tab-label {{ font-size: {}px; color: {}; padding: 0px; }}\n",
            m.ui_font,
            color_hex(&c.text_muted)
        ));
        // PSS has no descendant selectors, so the active label color comes from
        // this extra class rather than `.pwb-tab.active text`.
        css.push_str(&format!(
            ".pwb-tab-label-active {{ font-size: {}px; color: {}; padding: 0px; }}\n",
            m.ui_font,
            color_hex(&c.text)
        ));
        css.push_str(&format!(
            ".pwb-tab-close {{ flex-direction: row; width: {}px; height: {}px; padding: 0px; \
             align-items: center; justify-content: center; border-radius: 6px; \
             background-color: transparent; cursor: pointer; }}\n",
            (m.control * 0.55).max(14.0),
            (m.control * 0.55).max(14.0)
        ));
        css.push_str(&format!(
            ".pwb-icon {{ width: {}px; height: {}px; padding: 0px; color: {}; }}\n",
            m.icon,
            m.icon,
            color_hex(&c.text_muted)
        ));
        css.push_str(&format!(
            ".pwb-icon-nav {{ width: {}px; height: {}px; padding: 0px; color: {}; }}\n",
            m.icon,
            m.icon,
            color_hex(&c.text)
        ));
        css.push_str(&format!(
            ".pwb-icon-muted {{ width: {}px; height: {}px; padding: 0px; color: {}; }}\n",
            m.icon,
            m.icon,
            color_hex(&c.text_muted)
        ));
        css.push_str(&format!(
            ".pwb-icon-accent {{ width: {}px; height: {}px; padding: 0px; color: {}; }}\n",
            m.icon,
            m.icon,
            color_hex(&c.accent)
        ));
        // `flex-direction: row` is what makes the glyph sit in the middle: a
        // column container would put it in the top-left corner.
        css.push_str(&format!(
            ".pwb-iconbtn {{ flex-direction: row; width: {}px; height: {}px; padding: 0px; \
             align-items: center; justify-content: center; border-radius: {}px; \
             background-color: transparent; cursor: pointer; }}\n",
            m.control, m.control, m.radius
        ));
        // A control with nothing to do (back/forward at the end of the tab's
        // history) is drawn in a dimmed color and gets no hover highlight.
        css.push_str(&format!(
            ".pwb-icon-disabled {{ width: {}px; height: {}px; padding: 0px; color: {}; }}\n",
            m.icon,
            m.icon,
            color_hex(&dim(&c.text_muted, &c.chrome, 0.45))
        ));
        css.push_str(".pwb-iconbtn.pwb-disabled { cursor: default; }\n");
        css.push_str(&format!(
            ".pwb-toolbar {{ flex-direction: row; width: 100%; height: {}px; \
             align-items: center; gap: 0px; padding: 0px; background-color: {}; }}\n",
            m.toolbar_height,
            color_hex(&c.chrome)
        ));
        css.push_str(&format!(
            ".pwb-nav {{ flex-direction: row; width: {}%; height: {}px; align-items: center; \
             justify-content: flex-start; gap: 2px; padding: 6px; background-color: transparent; }}\n",
            25, m.toolbar_height
        ));
        css.push_str(&format!(
            ".pwb-actions {{ flex-direction: row; width: {}%; height: {}px; align-items: center; \
             justify-content: flex-end; gap: 2px; padding: 6px; background-color: transparent; }}\n",
            25, m.toolbar_height
        ));
        css.push_str(&format!(
            ".pwb-address-group {{ flex-direction: row; width: {}%; height: {}px; \
             align-items: center; justify-content: center; gap: 0px; padding: 6px; \
             background-color: transparent; }}\n",
            self.layout.address_width, m.toolbar_height
        ));
        css.push_str(&format!(
            ".pwb-address {{ height: {}px; padding: 7px; font-size: {}px; \
             border-radius: {}px; background-color: {}; color: {}; border: 1px solid {}; \
             cursor: text; }}\n",
            m.control,
            m.ui_font,
            (m.control / 2.0).round(),
            color_hex(&c.address_bg),
            color_hex(&c.address_text),
            color_hex(&c.border)
        ));
        // The badge shares `.pwb-iconbtn` so it is a real button (hover,
        // pointer cursor); only its width is intrinsic, since it holds a label
        // next to the status dot.
        css.push_str(&format!(
            ".pwb-status {{ flex-direction: row; width: auto; height: {}px; \
             align-items: center; justify-content: center; gap: 5px; padding: 6px; \
             border-radius: {}px; background-color: transparent; cursor: pointer; }}\n",
            m.control, m.radius
        ));
        css.push_str(&format!(
            ".pwb-status-dot {{ font-size: {}px; color: {}; padding: 0px; }}\n",
            m.ui_font_small,
            color_hex(&c.ok)
        ));
        css.push_str(&format!(
            ".pwb-status-dot-off {{ font-size: {}px; color: {}; padding: 0px; }}\n",
            m.ui_font_small,
            color_hex(&c.danger)
        ));
        css.push_str(&format!(
            ".pwb-status-label {{ font-size: {}px; color: {}; padding: 0px; }}\n",
            m.ui_font_small,
            color_hex(&c.text_muted)
        ));
        css.push_str(&format!(
            ".pwb-separator {{ width: 1px; height: {}px; background-color: {}; padding: 0px; }}\n",
            (m.control * 0.6).round(),
            color_hex(&c.border)
        ));
        // Gentle hover feedback for chrome controls is driven by the app adding
        // the `hovered` class, exactly like tabs.
        css.push_str(&format!(
            ".pwb-iconbtn.hovered {{ background-color: {}; }}\n",
            color_hex(&c.surface_hover)
        ));
        css.push_str(&format!(
            ".pwb-newtab {{ flex-direction: row; width: {}px; height: {}px; padding: 0px; \
             align-items: center; justify-content: center; border-radius: {}px; \
             background-color: transparent; cursor: pointer; margin-left: 2px; }}\n",
            m.control, m.control, m.radius
        ));
        css.push_str(&format!(
            ".pwb-newtab.hovered {{ background-color: {}; }}\n",
            color_hex(&c.surface_hover)
        ));
        let _ = px(0.0);
        if !self.ui_pss.trim().is_empty() {
            css.push_str("\n/* theme ui overrides */\n");
            css.push_str(&self.ui_pss);
            css.push('\n');
        }
        css
    }
}

fn hex(value: &str) -> Color {
    parse_color(value).unwrap_or(Color::WHITE)
}

/// Mix `color` toward `towards` by `amount` (0 = unchanged, 1 = fully
/// `towards`). Used for derived colors such as disabled controls.
fn dim(color: &Color, towards: &Color, amount: f32) -> Color {
    let t = amount.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color {
        r: mix(color.r, towards.r),
        g: mix(color.g, towards.g),
        b: mix(color.b, towards.b),
        a: color.a,
    }
}

// ---------------------------------------------------------------------------
// Metrics and page defaults
// ---------------------------------------------------------------------------

/// Chrome metrics for the current window. Sizes are the theme's pixel values
/// scaled by a mild factor, so controls stay a sane size instead of tracking
/// `vh` (tiny on short windows, huge on tall ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub scale: f32,
    pub ui_font: f32,
    pub ui_font_small: f32,
    pub tab_height: f32,
    pub toolbar_height: f32,
    pub control: f32,
    pub gap: f32,
    pub radius: f32,
    pub icon: f32,
    pub chrome_height: f32,
}

impl Metrics {
    pub fn for_window(theme: &Theme, width: u32, height: u32) -> Self {
        let w = width.max(1) as f32;
        let h = height.max(1) as f32;
        // Geometric mean of the window size relative to a 1200x800 baseline.
        // Clamped, because chrome controls should not double on a 4K display.
        let scale = ((w / 1200.0) * (h / 800.0)).sqrt().clamp(0.92, 1.2);
        let s = &theme.sizes;
        let round = |v: f32| (v * scale).round().max(1.0);
        let tab_height = round(s.tab_height);
        let toolbar_height = round(s.toolbar_height);
        Self {
            scale,
            ui_font: (s.ui_font * scale).round().max(8.0),
            ui_font_small: (s.ui_font_small * scale).round().max(8.0),
            tab_height,
            toolbar_height,
            control: round(s.control),
            gap: round(s.gap),
            radius: round(s.radius),
            icon: round(s.icon),
            chrome_height: tab_height + toolbar_height + 1.0,
        }
    }
}

/// Default lengths for page content when the page does not specify them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageDefaults {
    pub scale: f32,
    pub font: f32,
    pub small_font: f32,
    pub view_padding: f32,
    pub gap: f32,
    pub button_font: f32,
    pub button_padding: f32,
    pub input_font: f32,
    pub input_padding: f32,
    pub icon_size: f32,
    pub radius: f32,
    /// Space reserved below inline widgets before the next flow item.
    pub spacing: f32,
    /// Left/right page margin when no chrome bar is present.
    pub margin: f32,
    /// Space between the chrome bar and page content.
    pub top_gap: f32,
}

impl PageDefaults {
    pub fn for_window(theme: &Theme, width: u32, height: u32) -> Self {
        let w = width.max(1) as f32;
        let h = height.max(1) as f32;
        let scale = ((w / 1200.0) * (h / 800.0)).sqrt().clamp(0.9, 1.15);
        let s = &theme.sizes;
        let sized = |v: f32| (v * scale).round().max(1.0);
        Self {
            scale,
            font: (s.page_font * scale).round().max(10.0),
            small_font: (s.page_font * scale * 0.8).round().max(9.0),
            view_padding: sized(s.page_padding),
            gap: sized(s.page_gap),
            button_font: (s.page_font * scale * 0.93).round().max(10.0),
            button_padding: sized(8.0),
            input_font: (s.page_font * scale * 0.93).round().max(10.0),
            input_padding: sized(7.0),
            icon_size: sized(18.0),
            radius: sized(6.0),
            spacing: sized(8.0),
            margin: (w * 0.04).clamp(16.0, 72.0),
            top_gap: sized(10.0),
        }
    }
}

// ---------------------------------------------------------------------------
// Active theme (the layout and renderer read this)
// ---------------------------------------------------------------------------

static ACTIVE: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();

fn active_cell() -> &'static RwLock<Arc<Theme>> {
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(Theme::default_dark())))
}

/// The theme the browser is currently rendering with.
pub fn active_theme() -> Arc<Theme> {
    active_cell()
        .read()
        .map(|guard| Arc::clone(&guard))
        .unwrap_or_else(|_| Arc::new(Theme::default_dark()))
}

/// Run `f` with the active theme without cloning it.
pub fn with_active<R>(f: impl FnOnce(&Theme) -> R) -> R {
    match active_cell().read() {
        Ok(guard) => f(&guard),
        Err(_) => f(&Theme::default_dark()),
    }
}

/// Install `theme` as the one used by layout and rendering.
pub fn set_active(theme: Theme) {
    if let Ok(mut guard) = active_cell().write() {
        *guard = Arc::new(theme);
    }
}

pub fn page_defaults(width: u32, height: u32) -> PageDefaults {
    with_active(|theme| PageDefaults::for_window(theme, width, height))
}

// ---------------------------------------------------------------------------
// Page fallback styling
// ---------------------------------------------------------------------------

/// Apply the theme's `page-pss` to a page tree as fallback attributes: a
/// property is only inserted when the page does not already set it, so
/// authored PSS and markup attributes always win.
pub fn apply_page_defaults(root: &mut crate::dom::DomNode, theme: &Theme) {
    if theme.page_pss.trim().is_empty() {
        return;
    }
    let rules = pwb_web::parse::parse_pss_stylesheet(&theme.page_pss);
    apply_defaults_to_node(root, &rules);
}

fn apply_defaults_to_node(
    node: &mut crate::dom::DomNode,
    rules: &[(String, Vec<pwb_core::spec::StyleProperty>)],
) {
    // Never restyle browser chrome or the devtools panel.
    if node
        .attributes
        .get("id")
        .map(|id| id.starts_with("__pwb_"))
        .unwrap_or(false)
        || node.attributes.contains_key("data-devtools")
    {
        return;
    }
    let tag = node.tag.name();
    // Owned copies: the attribute map is mutated below.
    let id = node.attributes.get("id").cloned();
    let classes: Vec<String> = node
        .attributes
        .get("class")
        .map(|value| value.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    for (selector, props) in rules {
        let selector = selector.trim();
        let matches = if let Some(rest) = selector.strip_prefix('#') {
            id.as_deref() == Some(rest)
        } else if let Some(rest) = selector.strip_prefix('.') {
            let required: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
            !required.is_empty()
                && required
                    .iter()
                    .all(|want| classes.iter().any(|class| class == want))
        } else {
            selector == tag
        };
        if !matches {
            continue;
        }
        for prop in props {
            if let Some((key, value)) = pwb_web::parse::style_property_to_attr(prop) {
                node.attributes.entry(key).or_insert(value);
            }
        }
    }
    for child in &mut node.children {
        apply_defaults_to_node(child, rules);
    }
}

// ---------------------------------------------------------------------------
// Browser-shipped pages
// ---------------------------------------------------------------------------

/// Whether `url` addresses a page the browser itself ships: a `*.plain` system
/// page or a builtin page from `webpages/`. Those pages are authored against
/// the conventional dark palette below and are recolored from the active theme;
/// sites authored by anyone else are never touched.
pub fn is_shipped_page(url: &str) -> bool {
    let clean = url.trim().trim_start_matches("plain://");
    let target = clean.split(['?', '#']).next().unwrap_or(clean);
    if target.is_empty() {
        return false;
    }
    // The devtools panel is chrome, not a page: it keeps its own dark look.
    if target == "devtools" || target == "devtools.plain" {
        return false;
    }
    crate::builtin_sites::is_builtin_site(target) || crate::resolver::is_system_target(target)
}

/// One authored palette literal and the page color it maps onto.
type PaletteSlot = (&'static str, fn(&PageColors) -> Color);

/// Palette literals the browser's own pages are written with. They come from
/// the dark palette the default themes were designed against; a theme maps each
/// onto one of its page colors.
const SHIPPED_PALETTE: &[PaletteSlot] = &[
    ("#1e1e2e", |p| p.background),
    ("#181825", |p| p.background),
    ("#11111b", |p| p.background),
    ("#313244", |p| p.code_bg),
    ("#45475a", |p| p.input_border),
    ("#cdd6f4", |p| p.text),
    ("#bac2de", |p| p.text),
    ("#a6adc8", |p| p.text),
    ("#6c7086", |p| p.muted),
    ("#89b4fa", |p| p.link),
    ("#fab387", |p| p.link),
    ("#a6e3a1", |p| p.icon),
    ("#f9e2af", |p| p.warn),
    ("#f38ba8", |p| p.danger),
];

/// Rewrite the palette literals in a browser-shipped page onto the active
/// theme's page colors.
///
/// A theme owns the default look of the browser's own pages (welcome, search,
/// network, …), so switching to the light theme gives light pages instead of a
/// light chrome around dark content. Everything outside the palette — layouts,
/// sizes, and colors the page chose for itself — is left untouched.
pub fn recolor_shipped_page(root: &mut crate::dom::DomNode, page: &PageColors, url: &str) {
    if !is_shipped_page(url) {
        return;
    }
    recolor_page(root, page);
}

/// Recolor a page the browser generated itself (an error page, a list page).
pub fn recolor_page(root: &mut crate::dom::DomNode, page: &PageColors) {
    recolor_node(root, page);
}

fn recolor_node(node: &mut crate::dom::DomNode, page: &PageColors) {
    for (key, value) in node.attributes.iter_mut() {
        // Only color-bearing properties are rewritten; sizes, classes and
        // layout attributes are left exactly as the page authored them.
        if !(key.contains("color") || key == "border" || key.starts_with("border-")) {
            continue;
        }
        let mut next = value.clone();
        for (literal, pick) in SHIPPED_PALETTE {
            if next.contains(literal) {
                next = next.replace(literal, &color_hex(&pick(page)));
            }
        }
        if next != *value {
            *value = next;
        }
    }
    for child in &mut node.children {
        recolor_node(child, page);
    }
}

// ---------------------------------------------------------------------------
// Theme store (~/.pwb/themes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    pub builtin: bool,
}

/// Themes on disk plus a per-file fingerprint so the app can hot-reload the
/// active file after it is edited.
#[derive(Debug, Default)]
pub struct ThemeStore {
    dir: PathBuf,
    fingerprints: HashMap<String, (u64, Option<std::time::SystemTime>)>,
}

impl ThemeStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            fingerprints: HashMap::new(),
        }
    }

    /// `$PWB_DATA/themes`, else `~/.pwb/themes` — the same data directory the
    /// session and storage already use.
    pub fn discover() -> Self {
        let dir = if let Some(dir) = std::env::var_os("PWB_DATA") {
            PathBuf::from(dir).join("themes")
        } else {
            pwb_core::paths::home_dir()
                .map(|home| home.join(".pwb").join("themes"))
                .unwrap_or_else(|| PathBuf::from(".pwb").join("themes"))
        };
        Self::new(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Create the themes directory and write the two builtin themes there when
    /// they are missing (a fresh install, or a folder the user emptied). The
    /// files are written line by line. An existing file is left untouched —
    /// local edits survive upgrades — unless it is a single-line file written by
    /// an older build, which is rewritten in the readable form.
    pub fn ensure_builtin_files(&self) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            eprintln!(
                "[Theme] Could not create theme directory {}",
                self.dir.display()
            );
            return;
        }
        for theme in [Theme::default_dark(), Theme::default_light()] {
            let path = self.dir.join(format!("{}.ptheme", theme.id));
            if std::fs::read_to_string(&path)
                .map(|source| !Self::needs_pretty_rewrite(&source))
                .unwrap_or(false)
            {
                continue;
            }
            if let Err(error) = std::fs::write(&path, theme.to_ptheme()) {
                eprintln!("[Theme] Could not write {}: {error}", path.display());
            }
        }
    }

    /// A builtin file needs rewriting when it is empty or was written as one
    /// long line. Multi-line files are left alone: they are the user's edits.
    fn needs_pretty_rewrite(source: &str) -> bool {
        let trimmed = source.trim();
        trimmed.is_empty() || !trimmed.contains('\n')
    }

    /// Installed themes: every parseable `.ptheme` in the directory, plus the
    /// builtins when they are not present as files.
    pub fn list(&self) -> Vec<ThemeInfo> {
        let mut list = Vec::new();
        let mut seen = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("ptheme") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                match std::fs::read_to_string(&path) {
                    Ok(source) => match Theme::parse(&source) {
                        Ok(theme) => {
                            list.push(ThemeInfo {
                                id: id.to_string(),
                                name: theme.name,
                                builtin: Theme::builtin(id).is_some(),
                            });
                            seen.push(id.to_string());
                        }
                        Err(error) => eprintln!("[Theme] {}: {error}", path.display()),
                    },
                    Err(error) => eprintln!("[Theme] {}: {error}", path.display()),
                }
            }
        }
        for theme in [Theme::default_dark(), Theme::default_light()] {
            if !seen.contains(&theme.id) {
                list.push(ThemeInfo {
                    id: theme.id.clone(),
                    name: theme.name,
                    builtin: true,
                });
            }
        }
        list.sort_by_key(|theme| theme.name.to_lowercase());
        list
    }

    /// Load a theme by id, falling back to the hardcoded dark theme for unknown
    /// or unreadable files.
    pub fn load(&self, id: &str) -> Theme {
        if let Some(source) = self.read(id) {
            match Theme::parse(&source) {
                Ok(mut theme) => {
                    theme.id = id.to_string();
                    return theme;
                }
                Err(error) => {
                    eprintln!("[Theme] {id}.ptheme: {error} (using default dark)");
                }
            }
        }
        if let Some(mut theme) = Theme::builtin(id) {
            theme.id = id.to_string();
            return theme;
        }
        eprintln!("[Theme] No theme named '{id}'; using default dark");
        let mut theme = Theme::default_dark();
        theme.id = id.to_string();
        theme.name = format!("{id} (unavailable)");
        theme
    }

    fn read(&self, id: &str) -> Option<String> {
        let path = self.dir.join(format!("{id}.ptheme"));
        std::fs::read_to_string(path).ok()
    }

    fn fingerprint(&self, id: &str) -> Option<(u64, Option<std::time::SystemTime>)> {
        let path = self.dir.join(format!("{id}.ptheme"));
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.len(), metadata.modified().ok()))
    }

    /// Reload the active theme if its file changed on disk.
    pub fn reload_if_changed(&mut self, id: &str) -> Option<Theme> {
        let current = self.fingerprint(id);
        match current {
            Some(current) => {
                if self.fingerprints.get(id) == Some(&current) {
                    return None;
                }
                self.fingerprints.insert(id.to_string(), current);
                Some(self.load(id))
            }
            None => {
                // Builtin (no file) or a file that was just deleted. Report a
                // change only when the state differs from the last poll.
                match self.fingerprints.get(id) {
                    Some((_, None)) => None,
                    Some(_) => {
                        // The file disappeared: fall back to the builtin or the
                        // hardcoded default.
                        self.fingerprints.insert(id.to_string(), (0, None));
                        Some(self.load(id))
                    }
                    None => {
                        self.fingerprints.insert(id.to_string(), (0, None));
                        Some(self.load(id))
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-like parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
        }
    }

    fn parse_document(mut self) -> Result<Value, String> {
        self.skip();
        if self.peek() == Some('{') {
            self.pos += 1;
            let map = self.parse_map_body(true)?;
            self.skip();
            if self.pos < self.chars.len() {
                return Err(format!("unexpected trailing input at offset {}", self.pos));
            }
            Ok(Value::Map(map))
        } else {
            let map = self.parse_map_body(false)?;
            Ok(Value::Map(map))
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip(&mut self) {
        loop {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.pos += 1;
            }
            if self.peek() == Some('/') && self.chars.get(self.pos + 1) == Some(&'/') {
                while matches!(self.peek(), Some(c) if c != '\n') {
                    self.pos += 1;
                }
                continue;
            }
            if self.peek() == Some('/') && self.chars.get(self.pos + 1) == Some(&'*') {
                self.pos += 2;
                while self.pos + 1 < self.chars.len() {
                    if self.chars[self.pos] == '*' && self.chars[self.pos + 1] == '/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn parse_map_body(&mut self, terminated: bool) -> Result<Vec<(String, Value)>, String> {
        let mut map = Vec::new();
        loop {
            self.skip();
            match self.peek() {
                None => {
                    if terminated {
                        return Err("unexpected end of file; missing '}'".to_string());
                    }
                    break;
                }
                Some('}') => {
                    self.pos += 1;
                    if !terminated {
                        return Err("unexpected '}'".to_string());
                    }
                    break;
                }
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                Some(_) => {}
            }
            let key = self.parse_key()?;
            self.skip();
            if self.peek() != Some(':') && self.peek() != Some('=') {
                return Err(format!("expected ':' after key '{key}'"));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.push((normalize_key(&key), value));
            self.skip();
            if self.peek() == Some(',') {
                self.pos += 1;
            }
        }
        Ok(map)
    }

    fn parse_key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some('"') | Some('\'') => self.parse_string(),
            Some(_) => {
                let mut key = String::new();
                while let Some(c) = self.peek() {
                    if c.is_whitespace() || matches!(c, ':' | '=' | ',' | '}' | '{') {
                        break;
                    }
                    key.push(c);
                    self.pos += 1;
                }
                if key.is_empty() {
                    Err("expected a key".to_string())
                } else {
                    Ok(key)
                }
            }
            None => Err("unexpected end of file; expected a key".to_string()),
        }
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip();
        match self.peek() {
            None => Err("unexpected end of file; expected a value".to_string()),
            Some('{') => {
                self.pos += 1;
                Ok(Value::Map(self.parse_map_body(true)?))
            }
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip();
                    match self.peek() {
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(',') => {
                            self.pos += 1;
                        }
                        None => return Err("unexpected end of file in list".to_string()),
                        Some(_) => items.push(self.parse_value()?),
                    }
                }
                Ok(Value::List(items))
            }
            Some('"') | Some('\'') => {
                if self.peek() == Some('"')
                    && self.chars.get(self.pos + 1) == Some(&'"')
                    && self.chars.get(self.pos + 2) == Some(&'"')
                {
                    self.parse_raw_string().map(Value::Str)
                } else {
                    self.parse_string().map(Value::Str)
                }
            }
            Some(_) => {
                let mut token = String::new();
                while let Some(c) = self.peek() {
                    if c.is_whitespace() || matches!(c, ',' | '}' | ']' | ':') {
                        break;
                    }
                    token.push(c);
                    self.pos += 1;
                }
                if token.is_empty() {
                    return Err("expected a value".to_string());
                }
                if token.eq_ignore_ascii_case("true") {
                    Ok(Value::Bool(true))
                } else if token.eq_ignore_ascii_case("false") {
                    Ok(Value::Bool(false))
                } else if token.eq_ignore_ascii_case("null") {
                    Ok(Value::Str(String::new()))
                } else if let Ok(number) = token.parse::<f64>() {
                    Ok(Value::Num(number))
                } else {
                    Ok(Value::Str(token))
                }
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("expected a string")?;
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    let escaped = self.peek().unwrap_or('\\');
                    self.pos += 1;
                    out.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// `"""raw text"""`: everything up to the closing triple quote is taken
    /// verbatim, so PSS (`{`, `}`, `#colors`, comments) needs no escaping.
    fn parse_raw_string(&mut self) -> Result<String, String> {
        self.pos += 3;
        let mut out = String::new();
        while self.pos + 2 < self.chars.len() {
            if self.chars[self.pos] == '"'
                && self.chars[self.pos + 1] == '"'
                && self.chars[self.pos + 2] == '"'
            {
                // Trim a single leading newline so `"""\n...` starts at the
                // first content line.
                if out.starts_with('\n') {
                    out.remove(0);
                }
                self.pos += 3;
                return Ok(out);
            }
            out.push(self.chars[self.pos]);
            self.pos += 1;
        }
        Err("unterminated \"\"\" block".to_string())
    }
}

fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('_', "-")
}

fn get_value<'a>(map: &'a [(String, Value)], keys: &[&str]) -> Option<&'a Value> {
    for (key, value) in map {
        if keys.iter().any(|want| *want == key) {
            return Some(value);
        }
    }
    None
}

fn get_str(map: &[(String, Value)], keys: &[&str]) -> Option<String> {
    match get_value(map, keys)? {
        Value::Str(value) => Some(value.clone()),
        Value::Num(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn get_bool(map: &[(String, Value)], keys: &[&str]) -> Option<bool> {
    match get_value(map, keys)? {
        Value::Bool(value) => Some(*value),
        Value::Str(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        Value::Num(value) => Some(*value != 0.0),
        _ => None,
    }
}

fn get_num(map: &[(String, Value)], keys: &[&str]) -> Option<f32> {
    match get_value(map, keys)? {
        Value::Num(value) => Some(*value as f32),
        Value::Str(value) => value.trim().parse::<f32>().ok(),
        _ => None,
    }
}

fn get_map<'a>(map: &'a [(String, Value)], keys: &[&str]) -> Option<&'a [(String, Value)]> {
    match get_value(map, keys)? {
        Value::Map(map) => Some(map),
        _ => None,
    }
}

fn assign_color(map: &[(String, Value)], keys: &[&str], target: &mut Color) {
    if let Some(value) = get_str(map, keys) {
        if let Some(color) = parse_color(&value) {
            *target = color;
        }
    }
}

fn apply_ui_colors(colors: &mut UiColors, map: &[(String, Value)]) {
    assign_color(map, &["chrome", "background"], &mut colors.chrome);
    assign_color(map, &["surface"], &mut colors.surface);
    assign_color(map, &["surface-hover"], &mut colors.surface_hover);
    assign_color(map, &["surface-active"], &mut colors.surface_active);
    assign_color(map, &["border", "separator"], &mut colors.border);
    assign_color(map, &["text", "foreground"], &mut colors.text);
    assign_color(map, &["text-muted", "muted"], &mut colors.text_muted);
    assign_color(map, &["accent"], &mut colors.accent);
    assign_color(map, &["accent-text"], &mut colors.accent_text);
    assign_color(map, &["address-bg"], &mut colors.address_bg);
    assign_color(map, &["address-text"], &mut colors.address_text);
    assign_color(map, &["tab-active"], &mut colors.tab_active);
    assign_color(map, &["tab-inactive", "tab"], &mut colors.tab_inactive);
    assign_color(map, &["tab-hover"], &mut colors.tab_hover);
    assign_color(map, &["ok", "online", "success"], &mut colors.ok);
    assign_color(map, &["danger", "offline", "error"], &mut colors.danger);
    assign_color(map, &["selection"], &mut colors.selection);
}

fn apply_page_colors(page: &mut PageColors, map: &[(String, Value)]) {
    assign_color(map, &["background", "bg"], &mut page.background);
    assign_color(map, &["text", "foreground"], &mut page.text);
    assign_color(map, &["muted"], &mut page.muted);
    assign_color(map, &["link", "accent"], &mut page.link);
    assign_color(map, &["button-bg", "button"], &mut page.button_bg);
    assign_color(map, &["button-text"], &mut page.button_text);
    assign_color(map, &["input-bg"], &mut page.input_bg);
    assign_color(map, &["input-text"], &mut page.input_text);
    assign_color(map, &["input-border"], &mut page.input_border);
    assign_color(map, &["placeholder"], &mut page.placeholder);
    assign_color(map, &["code-bg"], &mut page.code_bg);
    assign_color(map, &["code-text"], &mut page.code_text);
    assign_color(map, &["warn", "warning"], &mut page.warn);
    assign_color(map, &["danger", "error"], &mut page.danger);
    assign_color(map, &["icon"], &mut page.icon);
    assign_color(map, &["scrollbar-track"], &mut page.scrollbar_track);
    assign_color(map, &["scrollbar-thumb"], &mut page.scrollbar_thumb);
    assign_color(map, &["selection"], &mut page.selection);
}

fn apply_sizes(sizes: &mut Sizes, map: &[(String, Value)]) {
    let set = |keys: &[&str], target: &mut f32| {
        if let Some(value) = get_num(map, keys) {
            if value > 0.0 {
                *target = value;
            }
        }
    };
    set(&["ui-font", "font"], &mut sizes.ui_font);
    set(&["ui-font-small", "small-font"], &mut sizes.ui_font_small);
    set(&["tab-height"], &mut sizes.tab_height);
    set(&["toolbar-height", "toolbar"], &mut sizes.toolbar_height);
    set(&["control", "button-size"], &mut sizes.control);
    set(&["gap", "spacing"], &mut sizes.gap);
    set(&["radius", "border-radius"], &mut sizes.radius);
    set(&["icon", "icon-size"], &mut sizes.icon);
    set(&["page-font", "base-font"], &mut sizes.page_font);
    set(&["page-padding"], &mut sizes.page_padding);
    set(
        &["page-gap", "page-spacing", "page-block-gap"],
        &mut sizes.page_gap,
    );
}

fn apply_layout(layout: &mut ChromeLayout, map: &[(String, Value)]) {
    if let Some(value) = get_str(map, &["close-button", "close"]) {
        if let Some(mode) = CloseButton::parse(&value) {
            layout.close_button = mode;
        }
    }
    if let Some(value) = get_num(map, &["address-width"]) {
        layout.address_width = value.clamp(20.0, 90.0);
    }
    if let Some(value) = get_num(map, &["tab-min-width"]) {
        layout.tab_min_width = value.max(40.0);
    }
    if let Some(value) = get_num(map, &["tab-max-width"]) {
        layout.tab_max_width = value.max(layout.tab_min_width);
    }
    if let Some(value) = get_str(map, &["tabs-align"]) {
        if let Some(align) = TabsAlign::parse(&value) {
            layout.tabs_align = align;
        }
    }
    if let Some(value) = get_bool(map, &["scroll-buttons", "scroll"]) {
        layout.scroll_buttons = value;
    }
}

fn apply_icons(icons: &mut IconSet, map: &[(String, Value)]) {
    let set = |keys: &[&str], target: &mut String| {
        if let Some(value) = get_str(map, keys) {
            if !value.trim().is_empty() {
                *target = value.trim().to_string();
            }
        }
    };
    set(&["back"], &mut icons.back);
    set(&["forward"], &mut icons.forward);
    set(&["reload", "refresh"], &mut icons.reload);
    set(&["new-tab", "new"], &mut icons.new_tab);
    set(&["close", "x"], &mut icons.close);
    set(&["bookmark"], &mut icons.bookmark);
    set(&["bookmarks", "book"], &mut icons.bookmarks);
    set(&["history", "clock"], &mut icons.history);
    set(&["settings", "gear"], &mut icons.settings);
    set(&["network", "peers"], &mut icons.network);
    set(&["tab-left", "scroll-left"], &mut icons.tab_left);
    set(&["tab-right", "scroll-right"], &mut icons.tab_right);
    set(&["home"], &mut icons.home);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::spec::PlainNode;

    #[test]
    fn theme_parser_accepts_comments_quotes_and_trailing_commas() {
        let theme = Theme::parse(
            r##"
            // a comment
            {
              name: "Night",
              dark: true,
              colors: {
                chrome: "#101014",
                accent: "#ff00aa",
              },
              sizes: { "ui-font": 12.5, tab_height: 36 },
              icons: { back: "arrow-left" },
            }
            "##,
        )
        .expect("parses");
        assert_eq!(theme.name, "Night");
        assert_eq!(theme.colors.chrome, hex("#101014"));
        assert_eq!(theme.colors.accent, hex("#ff00aa"));
        assert_eq!(theme.sizes.ui_font, 12.5);
        assert_eq!(theme.sizes.tab_height, 36.0);
        assert_eq!(theme.icons.back, "arrow-left");
        // Unspecified fields fall back to the hardcoded dark theme.
        assert_eq!(theme.page.text, Theme::default_dark().page.text);
    }

    #[test]
    fn raw_pss_blocks_keep_braces_and_hashes() {
        let theme = Theme::parse(
            r#"
            {
              ui: """
                .pwb-chrome { background-color: #123456; }
                .pwb-tab { border-radius: 0px; }
              """,
              page-pss: """
                view { padding: 12px; }
              """,
            }
            "#,
        )
        .expect("parses");
        assert!(
            theme
                .ui_pss
                .contains(".pwb-chrome { background-color: #123456; }"),
            "ui_pss = {:?}",
            theme.ui_pss
        );
        assert!(theme.page_pss.contains("padding: 12px"));
    }

    #[test]
    fn missing_fields_and_bad_files_fall_back() {
        let theme = Theme::parse("{ name: 'Partial' }").expect("parses");
        let default = Theme::default_dark();
        assert_eq!(theme.name, "Partial");
        assert_eq!(theme.colors.chrome, default.colors.chrome);
        assert_eq!(theme.layout.address_width, default.layout.address_width);
        assert!(Theme::parse("{").is_err());
        assert!(Theme::parse("{}").is_ok());
    }

    #[test]
    fn builtin_themes_round_trip_through_ptheme_text() {
        for theme in [Theme::default_dark(), Theme::default_light()] {
            let text = theme.to_ptheme();
            let parsed = Theme::parse(&text).expect("generated file parses");
            assert_eq!(parsed.name, theme.name);
            assert_eq!(parsed.dark, theme.dark);
            assert_eq!(parsed.colors, theme.colors);
            assert_eq!(parsed.page, theme.page);
            assert_eq!(parsed.sizes, theme.sizes);
            assert_eq!(parsed.icons, theme.icons);
        }
    }

    #[test]
    fn generated_chrome_css_uses_theme_colors_and_layout() {
        let theme = Theme::default_light();
        let metrics = Metrics::for_window(&theme, 1200, 800);
        let css = theme.chrome_css(&metrics);
        assert!(css.contains(&color_hex(&theme.colors.chrome)));
        assert!(css.contains(&format!("height: {}px", metrics.chrome_height)));
        assert!(css.contains("width: 25%"));
        assert!(css.contains(".pwb-address"));
        // A `ui` block is appended last so it can override the generated rules.
        let mut themed = theme.clone();
        themed.ui_pss = ".pwb-tab { border-radius: 2px; }".to_string();
        let css = themed.chrome_css(&metrics);
        let base = css.find(".pwb-tab {").expect("base tab rule");
        let over = css.find("border-radius: 2px").expect("override");
        assert!(over > base);
    }

    #[test]
    fn metrics_stay_in_a_sane_range_on_any_window() {
        let theme = Theme::default_dark();
        let small = Metrics::for_window(&theme, 760, 480);
        let large = Metrics::for_window(&theme, 3840, 2160);
        assert!(small.ui_font >= 12.0, "tiny window text stays readable");
        assert!(
            large.ui_font <= 18.0,
            "4K window does not double the chrome"
        );
        assert!(small.chrome_height < large.chrome_height);
        let page_small = PageDefaults::for_window(&theme, 760, 480);
        let page_large = PageDefaults::for_window(&theme, 2560, 1440);
        assert!(page_small.font >= 14.0);
        assert!(page_large.font <= 21.0);
    }

    #[test]
    fn page_defaults_only_fill_missing_attributes() {
        use pwb_core::spec::Tag;
        let mut root = crate::dom::DomNode {
            id: 0,
            tag: Tag::Document,
            attributes: HashMap::new(),
            text: None,
            children: vec![
                crate::dom::DomNode {
                    id: 1,
                    tag: Tag::View,
                    attributes: HashMap::from([
                        ("class".to_string(), "card".to_string()),
                        ("background-color".to_string(), "#000000".to_string()),
                    ]),
                    text: None,
                    children: Vec::new(),
                },
                crate::dom::DomNode {
                    id: 2,
                    tag: Tag::View,
                    attributes: HashMap::new(),
                    text: None,
                    children: Vec::new(),
                },
            ],
        };
        let mut theme = Theme::default_dark();
        theme.page_pss = "view { background-color: #112233; padding: 9px; }".to_string();
        apply_page_defaults(&mut root, &theme);
        assert_eq!(
            root.children[0]
                .attributes
                .get("background-color")
                .map(String::as_str),
            Some("#000000"),
            "authored colors win"
        );
        assert_eq!(
            root.children[0]
                .attributes
                .get("padding")
                .map(String::as_str),
            Some("9px")
        );
        assert_eq!(
            root.children[1]
                .attributes
                .get("background-color")
                .map(String::as_str),
            Some("#112233")
        );
    }

    #[test]
    fn store_writes_builtins_once_and_reloads_changed_files() {
        let dir = std::env::temp_dir().join(format!(
            "pwb-theme-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = ThemeStore::new(dir.clone());
        store.ensure_builtin_files();
        let dark_path = dir.join("default-dark.ptheme");
        assert!(dark_path.exists());
        assert!(dir.join("default-light.ptheme").exists());
        // Seeded themes are written line by line, not as one long line.
        let seeded = std::fs::read_to_string(&dark_path).unwrap();
        assert!(seeded.lines().count() > 40, "theme is formatted");
        assert!(seeded.contains("\n  colors: {\n"));

        // A single-line file from an older build is rewritten in the readable
        // form; a multi-line file with local edits is left alone.
        std::fs::write(&dark_path, "{ name: 'One Liner', dark: true }").unwrap();
        store.ensure_builtin_files();
        let normalized = std::fs::read_to_string(&dark_path).unwrap();
        assert!(normalized.lines().count() > 40);
        assert!(normalized.contains("Default Dark"));
        std::fs::write(&dark_path, "{\n  name: 'Mine',\n  dark: true,\n}\n").unwrap();
        store.ensure_builtin_files();
        assert!(std::fs::read_to_string(&dark_path)
            .unwrap()
            .contains("Mine"));
        // Restore the builtin so the reload assertions below read real fields.
        std::fs::write(&dark_path, Theme::default_dark().to_ptheme()).unwrap();

        let list = store.list();
        assert!(list.iter().any(|t| t.id == "default-light"));

        // First poll reports the loaded theme, an unchanged file reports
        // nothing, and an edit reports the new contents.
        assert!(store.reload_if_changed("default-light").is_some());
        assert!(store.reload_if_changed("default-light").is_none());
        let path = dir.join("default-light.ptheme");
        std::fs::write(&path, "{ name: 'Reloaded Light', dark: false }").unwrap();
        let reloaded = store
            .reload_if_changed("default-light")
            .expect("changed file reloads");
        assert_eq!(reloaded.name, "Reloaded Light");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_css_parses_and_styles_a_chrome_tree() {
        // The generated stylesheet must go through the real PSS parser and
        // produce attributes the layout engine understands.
        let theme = Theme::default_dark();
        let metrics = Metrics::for_window(&theme, 900, 650);
        let css = theme.chrome_css(&metrics);
        // Icon-only buttons must lay their glyph out in a row so the centered
        // alignment actually applies (a column stacks it into the top-left).
        assert!(css.contains(".pwb-iconbtn { flex-direction: row;"));
        assert!(css.contains(".pwb-tab-close { flex-direction: row;"));
        assert!(css.contains(".pwb-newtab { flex-direction: row;"));
        // The tab pins its close button to the right edge.
        assert!(css.contains("justify-content: space-between;"));
        let rules = pwb_web::parse::parse_pss_stylesheet(&css);
        let mut node = PlainNode::new(1, pwb_core::spec::Tag::View);
        node.attributes
            .insert("class".to_string(), "pwb-chrome".to_string());
        pwb_web::parse::apply_stylesheet(&mut node, &rules);
        assert_eq!(
            node.attributes.get("position").map(String::as_str),
            Some("fixed")
        );
        assert_eq!(
            node.attributes.get("height").map(String::as_str),
            Some(format!("{}px", metrics.chrome_height).as_str())
        );
    }
}
