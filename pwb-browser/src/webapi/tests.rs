use super::*;
use crate::dom::DomNode;
use std::collections::HashMap;
use std::time::Duration;

#[cfg(not(feature = "permissive-cors"))]
use crate::webapi::network::cors_validate;

#[test]
fn test_base64_round_trip() {
    assert_eq!(
        btoa("Hello, PlainWeb!").unwrap(),
        "SGVsbG8sIFBsYWluV2ViIQ=="
    );
    assert_eq!(
        atob("SGVsbG8sIFBsYWluV2ViIQ==").unwrap(),
        "Hello, PlainWeb!"
    );
    // Latin-1 chars (≤ U+00FF) are allowed; anything above must fail.
    assert_eq!(btoa("héllo").unwrap(), "aOlsbG8=");
    assert!(btoa("€10").is_err(), "char above U+00FF must fail btoa");
    assert!(atob("!!not-base64!!").is_err());
}

#[test]
fn test_random_bytes_are_latin1() {
    let bytes = random_bytes(64);
    assert_eq!(bytes.chars().count(), 64);
    assert!(bytes.chars().all(|c| (c as u32) <= 0xFF));
    // Extremely unlikely to be all the same byte.
    let distinct = bytes
        .chars()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(distinct > 1);
}

#[test]
fn test_random_uuid_format() {
    let uuid = random_uuid();
    let parts: Vec<&str> = uuid.split('-').collect();
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0].len(), 8);
    assert_eq!(parts[1].len(), 4);
    assert_eq!(parts[2].len(), 4);
    assert_eq!(parts[3].len(), 4);
    assert_eq!(parts[4].len(), 12);
    assert!(uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
}

#[test]
fn test_performance_now_monotonic() {
    let a = performance_now();
    std::thread::sleep(Duration::from_millis(5));
    let b = performance_now();
    assert!(b >= a);
    assert!(b - a >= 1.0);
}

#[test]
fn test_storage_ops() {
    let state = WebApiState::new();
    storage_op(&state, "local", "set", "k", "v");
    assert_eq!(storage_op(&state, "local", "get", "k", ""), "v");
    assert_eq!(storage_op(&state, "local", "length", "", ""), "1");
    assert_eq!(storage_op(&state, "local", "key", "0", ""), "k");
    storage_op(&state, "local", "remove", "k", "");
    assert_eq!(storage_op(&state, "local", "get", "k", ""), "");
    storage_op(&state, "local", "set", "a", "1");
    storage_op(&state, "local", "clear", "", "");
    assert_eq!(storage_op(&state, "local", "length", "", ""), "0");
    // sessionStorage is independent.
    storage_op(&state, "session", "set", "s", "x");
    assert_eq!(storage_op(&state, "session", "get", "s", ""), "x");
    assert_eq!(storage_op(&state, "local", "get", "s", ""), "");
}

#[test]
fn test_detached_nodes() {
    let state = WebApiState::new();
    let node = DomNode {
        id: 1,
        tag: pwb_core::spec::Tag::View,
        attributes: HashMap::new(),
        text: None,
        children: Vec::new(),
    };
    add_detached(&state, 1, node);
    assert_eq!(next_free_id(&state, 5), 6);
    assert!(take_detached(&state, 1).is_some());
    assert!(take_detached(&state, 1).is_none());
}

#[cfg(not(feature = "permissive-cors"))]
#[test]
fn test_cors_validate() {
    assert!(cors_validate("file:///x/y.pwb", &[]).is_ok());
    assert!(cors_validate("/relative/path", &[]).is_ok());
    assert!(cors_validate("https://api.example.com/x", &[]).is_err());
    assert!(cors_validate(
        "https://api.example.com/x",
        &[("access-control-allow-origin".to_string(), "*".to_string())]
    )
    .is_ok());
}

#[test]
fn test_next_op_id_increases() {
    let a = next_op_id();
    let b = next_op_id();
    assert!(b > a);
}

#[test]
fn test_timer_lifecycle() {
    // A 0ms timeout is due immediately.
    register_timer(101, 0.0, false);
    assert!(next_timer_deadline().is_some());
    assert_eq!(pop_due_timers(), vec![(101, None)]);
    // Fired timeouts are removed and not re-armed automatically.
    assert!(pop_due_timers().is_empty());

    // A far-future interval stays pending until cleared.
    register_timer(102, 60_000.0, true);
    assert!(pop_due_timers().is_empty());
    assert!(next_timer_deadline().is_some());
    clear_timer(102);
    assert!(next_timer_deadline().is_none());

    // reschedule_timer re-arms an id as an interval; 0ms fires on the
    // next pop and reports its interval.
    register_timer(103, 60_000.0, false);
    reschedule_timer(103, 0.0);
    assert_eq!(pop_due_timers(), vec![(103, Some(0.0))]);
    assert!(next_timer_deadline().is_none());

    // Negative delays clamp to zero.
    register_timer(104, -5.0, false);
    assert_eq!(pop_due_timers(), vec![(104, None)]);
}

#[test]
fn test_drain_async_queues_events() {
    let state = WebApiState::new();
    assert!(drain_async(&state).is_empty());
    state
        .async_queue
        .lock()
        .unwrap()
        .push_back(AsyncEvent::Fetch {
            id: 7,
            result: Err("boom".into()),
        });
    let events = drain_async(&state);
    assert_eq!(events.len(), 1);
    assert!(drain_async(&state).is_empty());
    match &events[0] {
        AsyncEvent::Fetch { id, result } => {
            assert_eq!(*id, 7);
            match result {
                Err(e) => assert_eq!(e, "boom"),
                Ok(_) => panic!("expected an error result"),
            }
        }
        _ => panic!("expected fetch event"),
    }
}

#[test]
fn test_put_detached_and_next_free_id() {
    let state = WebApiState::new();
    let mk = |id: usize| DomNode {
        id,
        tag: pwb_core::spec::Tag::View,
        attributes: HashMap::new(),
        text: None,
        children: Vec::new(),
    };
    put_detached(&state, mk(1));
    put_detached(&state, mk(3));
    // next_free_id starts above tree_max and skips taken ids.
    assert_eq!(next_free_id(&state, 0), 2, "1 is taken");
    assert_eq!(next_free_id(&state, 2), 4, "3 is taken");
    assert!(take_detached(&state, 1).is_some());
    assert!(take_detached(&state, 1).is_none());
    assert!(take_detached(&state, 3).is_some());
}

#[test]
fn test_local_storage_persists_through_storage_path() {
    let dir = std::env::temp_dir().join(format!("pwb-webapi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("local_storage.json");

    let state = WebApiState::new();
    *state.storage_path.borrow_mut() = Some(path.clone());
    storage_op(&state, "local", "set", "theme", "dark");
    assert!(path.exists(), "set persists to disk");

    // A fresh session with the same path loads the persisted map.
    let state2 = WebApiState::new();
    *state2.storage_path.borrow_mut() = Some(path.clone());
    assert_eq!(storage_op(&state2, "local", "get", "theme", ""), "dark");

    // remove + clear also persist.
    storage_op(&state2, "local", "remove", "theme", "");
    let state3 = WebApiState::new();
    *state3.storage_path.borrow_mut() = Some(path.clone());
    assert_eq!(storage_op(&state3, "local", "get", "theme", ""), "");

    // A corrupt file is ignored (in-memory map stays empty, no panic).
    std::fs::write(&path, "{not json").unwrap();
    let state4 = WebApiState::new();
    *state4.storage_path.borrow_mut() = Some(path.clone());
    assert_eq!(storage_op(&state4, "local", "get", "x", ""), "");

    // Unknown ops return empty strings on both kinds.
    assert_eq!(storage_op(&state4, "local", "bogus", "k", "v"), "");
    assert_eq!(storage_op(&state4, "session", "bogus", "k", "v"), "");
    assert_eq!(storage_op(&state4, "local", "key", "not-a-number", ""), "");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(feature = "permissive-cors"))]
#[test]
fn test_cors_validate_direct() {
    // Local/file URLs are always allowed.
    assert!(cors_validate("file:///x/y.pwb", &[]).is_ok());
    assert!(cors_validate("/relative/path", &[]).is_ok());
    assert!(cors_validate("welcome.pwb", &[]).is_ok());
    // Remote URLs need an explicit ACAO:* header.
    assert!(cors_validate("https://api.example.com/x", &[]).is_err());
    assert!(cors_validate(
        "https://api.example.com/x",
        &[("Access-Control-Allow-Origin".to_string(), "*".to_string())]
    )
    .is_ok());
    assert!(
        cors_validate(
            "https://api.example.com/x",
            &[(
                "access-control-allow-origin".to_string(),
                "self".to_string()
            )]
        )
        .is_err(),
        "only the wildcard origin passes"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn test_alert_confirm_prompt_headless_fallbacks() {
    // On non-macOS these are print-and-continue stubs.
    show_alert("coverage probe");
    assert!(show_confirm("continue?"));
    assert_eq!(show_prompt("name", "Ada").as_deref(), Some("Ada"));
}

#[test]
fn test_ws_open_connection_failure_queues_error_and_close() {
    let state = WebApiState::new();
    // Port 9 (discard) on localhost: nothing listens, the connect fails
    // promptly and the background thread reports Error + Close.
    let id = ws_open(&state, "ws://127.0.0.1:9");
    assert!(id > 0);

    let mut events = Vec::new();
    for _ in 0..100 {
        events.extend(drain_async(&state));
        if events.len() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        events.len() >= 2,
        "connect failure queues at least error+close, got {}",
        events.len()
    );
    let mut saw_error = false;
    let mut saw_close = false;
    for ev in &events {
        if let AsyncEvent::Ws { id: eid, event } = ev {
            assert_eq!(*eid, id);
            match event {
                WsEvent::Error(_) => saw_error = true,
                WsEvent::Close => saw_close = true,
                _ => {}
            }
        }
    }
    assert!(saw_error && saw_close, "expected Error then Close events");

    // Queueing outbound messages for a dead id is a harmless no-op.
    ws_send(&state, id, "ignored");
    ws_close(&state, id);
}
