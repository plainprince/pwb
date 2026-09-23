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
