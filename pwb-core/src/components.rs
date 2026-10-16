//! PlainWeb reusable UI component primitives (`pwb-core::components`).
//!
//! Both `pwb-web` (the authoring/bundler layer) and `pwb-browser` (the runtime)
//! share these component abstractions. They live here in `pwb-core` so neither
//! layer has to depend on the other to access them.

use std::collections::HashMap;

/// Represents a sidebar/navbar configuration
#[derive(Debug, Clone)]
pub struct SidebarConfig {
    pub id: String,
    pub position: SidebarPosition,
    pub width: f32,
    pub style: SidebarStyle,
    pub items: Vec<SidebarItem>,
}

/// Position of the sidebar
#[derive(Debug, Clone)]
pub enum SidebarPosition {
    Left,
    Right,
    Top,
    Bottom,
}

/// Style of the sidebar
#[derive(Debug, Clone)]
pub enum SidebarStyle {
    Sticky,
    Wide,
    Hamburger,
    Custom(String),
}

/// A single item in the sidebar
#[derive(Debug, Clone)]
pub struct SidebarItem {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub content: String,
    pub action: Option<String>,
    pub children: Vec<SidebarItem>,
}

/// Component registry for managing reusable components
#[derive(Default)]
pub struct ComponentRegistry {
    sidebars: HashMap<String, SidebarConfig>,
    navbars: HashMap<String, SidebarConfig>,
    tabs: HashMap<String, TabConfig>,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a sidebar
    pub fn register_sidebar(&mut self, config: SidebarConfig) {
        self.sidebars.insert(config.id.clone(), config);
    }

    /// Register a navbar
    pub fn register_navbar(&mut self, config: SidebarConfig) {
        self.navbars.insert(config.id.clone(), config);
    }

    /// Register tabs
    pub fn register_tabs(&mut self, config: TabConfig) {
        self.tabs.insert(config.id.clone(), config);
    }

    /// Get a sidebar by ID
    pub fn get_sidebar(&self, id: &str) -> Option<&SidebarConfig> {
        self.sidebars.get(id)
    }

    /// Get a navbar by ID
    pub fn get_navbar(&self, id: &str) -> Option<&SidebarConfig> {
        self.navbars.get(id)
    }

    /// Get tabs by ID
    pub fn get_tabs(&self, id: &str) -> Option<&TabConfig> {
        self.tabs.get(id)
    }
}

/// Tab set configuration
#[derive(Debug, Clone)]
pub struct TabConfig {
    pub id: String,
    pub tabs: Vec<TabItem>,
}

/// A single tab
#[derive(Debug, Clone)]
pub struct TabItem {
    pub id: String,
    pub label: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidebar(id: &str) -> SidebarConfig {
        SidebarConfig {
            id: id.into(),
            position: SidebarPosition::Left,
            width: 240.0,
            style: SidebarStyle::Sticky,
            items: vec![SidebarItem {
                id: "home".into(),
                label: "Home".into(),
                icon: Some("home".into()),
                content: "Welcome".into(),
                action: None,
                children: vec![SidebarItem {
                    id: "sub".into(),
                    label: "Sub".into(),
                    icon: None,
                    content: "".into(),
                    action: Some("#sub".into()),
                    children: vec![],
                }],
            }],
        }
    }

    #[test]
    fn test_register_and_get_sidebar() {
        let mut registry = ComponentRegistry::new();
        assert!(registry.get_sidebar("main").is_none());
        registry.register_sidebar(sidebar("main"));
        let got = registry.get_sidebar("main").expect("registered");
        assert_eq!(got.width, 240.0);
        assert!(matches!(got.position, SidebarPosition::Left));
        assert!(matches!(got.style, SidebarStyle::Sticky));
        assert_eq!(got.items.len(), 1);
        assert_eq!(got.items[0].children.len(), 1);
        assert_eq!(got.items[0].icon.as_deref(), Some("home"));
        assert_eq!(got.items[0].children[0].action.as_deref(), Some("#sub"));
    }

    #[test]
    fn test_sidebar_positions_and_styles_construct() {
        // Every variant is constructible and assignable.
        let mut config = sidebar("p");
        config.position = SidebarPosition::Right;
        config.position = SidebarPosition::Top;
        config.position = SidebarPosition::Bottom;
        config.position = SidebarPosition::Left;
        config.style = SidebarStyle::Wide;
        config.style = SidebarStyle::Hamburger;
        config.style = SidebarStyle::Custom("rounded".into());
        config.style = SidebarStyle::Sticky;
        assert!(matches!(config.style, SidebarStyle::Sticky));
    }

    #[test]
    fn test_register_and_get_navbar_is_separate_from_sidebars() {
        let mut registry = ComponentRegistry::new();
        registry.register_sidebar(sidebar("top"));
        registry.register_navbar(sidebar("top"));
        assert!(registry.get_sidebar("top").is_some());
        assert!(registry.get_navbar("top").is_some());
        assert!(registry.get_navbar("missing").is_none());
    }

    #[test]
    fn test_register_and_get_tabs() {
        let mut registry = ComponentRegistry::new();
        let tabs = TabConfig {
            id: "code".into(),
            tabs: vec![
                TabItem {
                    id: "src".into(),
                    label: "Source".into(),
                    content: "fn main() {}".into(),
                },
                TabItem {
                    id: "out".into(),
                    label: "Output".into(),
                    content: "ok".into(),
                },
            ],
        };
        registry.register_tabs(tabs);
        let got = registry.get_tabs("code").expect("registered");
        assert_eq!(got.tabs.len(), 2);
        assert_eq!(got.tabs[0].label, "Source");
        assert!(registry.get_tabs("missing").is_none());
    }

    #[test]
    fn test_reregistering_overwrites_by_id() {
        let mut registry = ComponentRegistry::new();
        registry.register_sidebar(sidebar("main"));
        let mut wider = sidebar("main");
        wider.width = 320.0;
        registry.register_sidebar(wider);
        assert_eq!(registry.get_sidebar("main").unwrap().width, 320.0);
    }
}
