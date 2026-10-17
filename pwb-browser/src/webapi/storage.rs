//! `localStorage` / `sessionStorage` operations.

use crate::webapi::state::WebApiState;

/// `kind` is `local` or `session`; `op` is `get`/`set`/`remove`/`clear`/`key`/`length`.
pub fn storage_op(state: &WebApiState, kind: &str, op: &str, key: &str, value: &str) -> String {
    let local = kind == "local";
    if local {
        state.load_local();
    }
    if local {
        let mut map = state.local_storage.borrow_mut();
        match op {
            "get" => map.get(key).cloned().unwrap_or_default(),
            "set" => {
                map.insert(key.to_string(), value.to_string());
                drop(map);
                state.save_local();
                String::new()
            }
            "remove" => {
                map.remove(key);
                drop(map);
                state.save_local();
                String::new()
            }
            "clear" => {
                map.clear();
                drop(map);
                state.save_local();
                String::new()
            }
            "key" => {
                let keys: Vec<String> = map.keys().cloned().collect();
                keys.get(key.parse::<usize>().unwrap_or(0))
                    .cloned()
                    .unwrap_or_default()
            }
            "length" => map.len().to_string(),
            _ => String::new(),
        }
    } else {
        let mut map = state.session_storage.borrow_mut();
        match op {
            "get" => map.get(key).cloned().unwrap_or_default(),
            "set" => {
                map.insert(key.to_string(), value.to_string());
                String::new()
            }
            "remove" => {
                map.remove(key);
                String::new()
            }
            "clear" => {
                map.clear();
                String::new()
            }
            "key" => {
                let keys: Vec<String> = map.keys().cloned().collect();
                keys.get(key.parse::<usize>().unwrap_or(0))
                    .cloned()
                    .unwrap_or_default()
            }
            "length" => map.len().to_string(),
            _ => String::new(),
        }
    }
}
