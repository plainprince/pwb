//! Native dialogs and JS primitives: alert/confirm/prompt, crypto/base64/performance.

use std::sync::OnceLock;
use std::time::Instant;

// ---------------------------------------------------------------------------
// alert / confirm / prompt
// ---------------------------------------------------------------------------

pub fn show_alert(msg: &str) {
    #[cfg(target_os = "macos")]
    macos::alert(msg);
    #[cfg(not(target_os = "macos"))]
    if !native::alert(msg) {
        println!("[JS Alert] {}", msg);
    }
}

pub fn show_confirm(msg: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return macos::confirm(msg);
    }
    #[cfg(not(target_os = "macos"))]
    match native::confirm(msg) {
        Some(answer) => answer,
        None => {
            println!("[JS Confirm] {}", msg);
            true
        }
    }
}

pub fn show_prompt(msg: &str, default_val: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        return macos::prompt(msg, default_val);
    }
    #[cfg(not(target_os = "macos"))]
    match native::prompt(msg, default_val) {
        Some(value) => value,
        None => {
            println!("[JS Prompt] {} (default: {})", msg, default_val);
            Some(default_val.to_string())
        }
    }
}

/// Cross-platform dialogs for Linux/Windows via `tinyfiledialogs`.
///
/// Unix builds drive `zenity`, `kdialog`, or `yad`; Windows uses the native
/// TaskDialog API. When no GUI backend is usable — headless CI, a server
/// without a display, or `PWB_HEADLESS` — the caller falls back to the console
/// behaviour instead of failing or hanging on a modal nobody can see.
#[cfg(not(target_os = "macos"))]
mod native {
    use tinyfiledialogs::{
        input_box, message_box_ok, message_box_ok_cancel, MessageBoxIcon, OkCancel,
    };

    const TITLE: &str = "PlainWeb";

    /// Whether a modal dialog can be shown in this process right now.
    pub(super) fn available() -> bool {
        if cfg!(test) || std::env::var_os("PWB_HEADLESS").is_some() {
            return false;
        }
        #[cfg(unix)]
        {
            let has_display = std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some();
            if !has_display {
                return false;
            }
            // tinyfiledialogs shells out to one of these on Unix. Probing
            // first keeps alert/confirm from silently turning into no-ops
            // (and confirm into a spurious `false`).
            if !["zenity", "kdialog", "yad"]
                .iter()
                .any(|name| in_path(name))
            {
                return false;
            }
        }
        true
    }

    fn in_path(name: &str) -> bool {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths).any(|dir| {
                    let candidate = dir.join(name);
                    candidate.is_file()
                })
            })
            .unwrap_or(false)
    }

    /// Returns `false` when no GUI dialog was available (caller prints instead).
    pub(super) fn alert(msg: &str) -> bool {
        if !available() {
            return false;
        }
        message_box_ok(TITLE, msg, MessageBoxIcon::Info);
        true
    }

    /// `Some(answer)` when a dialog was shown; `None` to use the console path.
    pub(super) fn confirm(msg: &str) -> Option<bool> {
        if !available() {
            return None;
        }
        Some(
            message_box_ok_cancel(TITLE, msg, MessageBoxIcon::Question, OkCancel::Ok)
                == OkCancel::Ok,
        )
    }

    /// `Some(None)` means the user cancelled; `None` means no GUI dialog.
    pub(super) fn prompt(msg: &str, default_val: &str) -> Option<Option<String>> {
        if !available() {
            return None;
        }
        Some(input_box(TITLE, msg, default_val))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSTextField};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

    pub fn alert(msg: &str) {
        unsafe {
            let mtm = MainThreadMarker::new().expect("alert must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.runModal();
        }
    }

    pub fn confirm(msg: &str) -> bool {
        unsafe {
            let mtm = MainThreadMarker::new().expect("confirm must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            alert.runModal() == NSAlertFirstButtonReturn
        }
    }

    pub fn prompt(msg: &str, default_val: &str) -> Option<String> {
        unsafe {
            let mtm = MainThreadMarker::new().expect("prompt must run on the main thread");
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(msg));
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let field = NSTextField::new(mtm);
            field.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(220.0, 24.0),
            ));
            field.setStringValue(&NSString::from_str(default_val));
            alert.setAccessoryView(Some(&*field));
            if alert.runModal() == NSAlertFirstButtonReturn {
                let value: Retained<NSString> = field.stringValue();
                Some(value.to_string())
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// crypto / base64 / performance
// ---------------------------------------------------------------------------

/// `n` random bytes as a Latin-1 string (each char code 0-255), so the JS side
/// can fill a Uint8Array without base64 round-trips.
pub fn random_bytes(n: u32) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n as usize];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.into_iter().map(|b| b as char).collect()
}

pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `btoa`: binary string (chars U+0000..=U+00FF) to base64.
pub fn btoa(s: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes: Result<Vec<u8>, ()> = s
        .chars()
        .map(|c| {
            let cp = c as u32;
            if cp > 0xFF {
                Err(())
            } else {
                Ok(cp as u8)
            }
        })
        .collect();
    let bytes = bytes.map_err(|_| "Invalid character in binary string (must be Latin-1)")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// `atob`: base64 to a binary string (each char code 0-255).
pub fn atob(s: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| "Invalid base64 string")?;
    Ok(bytes.into_iter().map(|b| b as char).collect())
}

static PERF_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the first call (monotonic, never goes backwards).
pub fn performance_now() -> f64 {
    let origin = *PERF_ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_secs_f64() * 1000.0
}
