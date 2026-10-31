//! Interactive Event Handling System (`pwb-browser::event_handler`).
//!
//! Handles user interaction events (clicks, hover, input) and dispatches them
//! to registered PlainJS (`.pjs`) event callbacks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps `(element_id, event_type)` -> list of event callback JS snippets.
type HandlerMap = HashMap<(String, String), Vec<String>>;

/// Global Event Registry shared between UI Window and QuickJS runtime.
#[derive(Default, Clone)]
pub struct EventHandlerRegistry {
    /// Maps `(element_id, event_type)` -> list of event callback JS snippets (for new API)
    handlers: Arc<Mutex<HandlerMap>>,

    /// Legacy: Maps `element_id` -> list of event callback JS snippets (for backward compatibility)
    click_handlers: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl EventHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an event handler for a specific element and event type
    pub fn add_handler(&self, element_id: usize, event_type: &str, js_code: String) {
        let mut handlers = self.handlers.lock().unwrap();
        let key = (element_id.to_string(), event_type.to_string());
        handlers.entry(key).or_default().push(js_code);
    }

    /// Get all handlers for a specific element and event type
    pub fn get_handlers(&self, element_id: usize, event_type: &str) -> Vec<String> {
        let handlers = self.handlers.lock().unwrap();
        let key = (element_id.to_string(), event_type.to_string());
        handlers.get(&key).cloned().unwrap_or_default()
    }

    /// Register a click handler (legacy API for backward compatibility)
    pub fn register_click_handler(&self, element_id: &str, js_callback: String) {
        let mut handlers = self.click_handlers.lock().unwrap();
        handlers
            .entry(element_id.to_string())
            .or_default()
            .push(js_callback);
    }

    /// Get click handlers (legacy API for backward compatibility)
    pub fn get_click_handlers(&self, element_id: &str) -> Vec<String> {
        let handlers = self.click_handlers.lock().unwrap();
        handlers.get(element_id).cloned().unwrap_or_default()
    }

    /// Drop every registered handler. Called on top-level navigation so a
    /// handler from the previous page cannot run against the next page's DOM
    /// (node ids are reused across bundles).
    pub fn clear(&self) {
        self.handlers.lock().unwrap().clear();
        self.click_handlers.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_handler_registry() {
        let registry = EventHandlerRegistry::new();
        registry.register_click_handler("main-btn", "console.log('clicked!');".to_string());

        let handlers = registry.get_click_handlers("main-btn");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0], "console.log('clicked!');");
    }
}
