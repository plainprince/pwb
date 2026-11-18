//! Cross-platform keyboard shortcuts.
//!
//! One resolver serves every platform: `Cmd` on macOS and `Ctrl` on
//! Linux/Windows are both accepted everywhere (users often remap, and winit
//! reports the same key codes regardless of layout). Matching uses the physical
//! key first and the logical character second, because some backends deliver
//! `Ctrl+C` as a control character rather than "c".
//!
//! Only commands the browser actually implements are listed — no dead
//! shortcuts for features that would silently do nothing.

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// An action bound to a key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Back,
    Forward,
    Reload,
    FocusAddress,
    NewTab,
    CloseTab,
    ReopenTab,
    NextTab,
    PrevTab,
    SelectTab(usize),
    ToggleBookmark,
    SavePage,
    SelectAll,
    Copy,
    Cut,
    Paste,
    DevTools,
    /// Open devtools with element-pick mode on (`Ctrl/Cmd+Shift+C`).
    Inspect,
    Home,
}

fn code(physical: PhysicalKey, expected: KeyCode) -> bool {
    matches!(physical, PhysicalKey::Code(c) if c == expected)
}

fn character(logical: &Key, expected: char) -> bool {
    matches!(logical, Key::Character(c) if c.eq_ignore_ascii_case(&expected.to_string()))
}

/// Match a letter by physical key or by the character the layout produced.
fn letter(physical: PhysicalKey, logical: &Key, expected: KeyCode, ch: char) -> bool {
    code(physical, expected) || character(logical, ch)
}

fn named(logical: &Key, expected: NamedKey) -> bool {
    matches!(logical, Key::Named(key) if *key == expected)
}

/// Resolve a pressed key (plus the current modifiers) to a browser command.
///
/// Returns `None` for shortcuts the browser does not implement, so the page
/// still receives its own keys. `Alt` combos are only considered when no
/// primary modifier is held: on Windows `AltGr` arrives as Ctrl+Alt, and that
/// must not trigger a browser action.
pub fn command(physical: PhysicalKey, logical: &Key, mods: ModifiersState) -> Option<Command> {
    let shift = mods.shift_key();

    // Chrome's macOS devtools binding is Cmd+Option+I/J (element pick: +C).
    // Kept macOS-only: on Windows Ctrl+Alt is AltGr, and typing must win.
    if cfg!(target_os = "macos")
        && mods.alt_key()
        && (mods.control_key() || mods.super_key())
        && !shift
    {
        if letter(physical, logical, KeyCode::KeyI, 'i')
            || letter(physical, logical, KeyCode::KeyJ, 'j')
        {
            return Some(Command::DevTools);
        }
        if letter(physical, logical, KeyCode::KeyC, 'c') {
            return Some(Command::Inspect);
        }
    }

    let primary = (mods.control_key() || mods.super_key()) && !mods.alt_key();

    if primary {
        if shift {
            if letter(physical, logical, KeyCode::KeyT, 't') {
                return Some(Command::ReopenTab);
            }
            if letter(physical, logical, KeyCode::KeyR, 'r') {
                return Some(Command::Reload);
            }
            if letter(physical, logical, KeyCode::KeyI, 'i')
                || letter(physical, logical, KeyCode::KeyJ, 'j')
            {
                return Some(Command::DevTools);
            }
            if letter(physical, logical, KeyCode::KeyC, 'c') {
                return Some(Command::Inspect);
            }
            if code(physical, KeyCode::Tab) || named(logical, NamedKey::Tab) {
                return Some(Command::PrevTab);
            }
            // Ctrl/Cmd+Shift+[ or ] also walks history in Chrome/Firefox.
            if code(physical, KeyCode::BracketLeft) || character(logical, '[') {
                return Some(Command::Back);
            }
            if code(physical, KeyCode::BracketRight) || character(logical, ']') {
                return Some(Command::Forward);
            }
            return None;
        }

        if letter(physical, logical, KeyCode::KeyA, 'a') {
            return Some(Command::SelectAll);
        }
        if letter(physical, logical, KeyCode::KeyC, 'c') {
            return Some(Command::Copy);
        }
        if letter(physical, logical, KeyCode::KeyX, 'x') {
            return Some(Command::Cut);
        }
        if letter(physical, logical, KeyCode::KeyV, 'v') {
            return Some(Command::Paste);
        }
        if letter(physical, logical, KeyCode::KeyL, 'l') {
            return Some(Command::FocusAddress);
        }
        if letter(physical, logical, KeyCode::KeyT, 't') {
            return Some(Command::NewTab);
        }
        if letter(physical, logical, KeyCode::KeyW, 'w') {
            return Some(Command::CloseTab);
        }
        if letter(physical, logical, KeyCode::KeyS, 's') {
            return Some(Command::SavePage);
        }
        if letter(physical, logical, KeyCode::KeyD, 'd') {
            return Some(Command::ToggleBookmark);
        }
        if letter(physical, logical, KeyCode::KeyR, 'r') {
            return Some(Command::Reload);
        }
        if code(physical, KeyCode::Tab) || named(logical, NamedKey::Tab) {
            return Some(Command::NextTab);
        }
        if code(physical, KeyCode::PageDown) {
            return Some(Command::NextTab);
        }
        if code(physical, KeyCode::PageUp) {
            return Some(Command::PrevTab);
        }
        if code(physical, KeyCode::BracketLeft) || character(logical, '[') {
            return Some(Command::Back);
        }
        if code(physical, KeyCode::BracketRight) || character(logical, ']') {
            return Some(Command::Forward);
        }
        // Ctrl/Cmd+1…8 selects that tab, 9 selects the last one (Chrome,
        // Firefox, and Edge all behave this way).
        for (index, key) in [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
        ]
        .into_iter()
        .enumerate()
        {
            if code(physical, key) || character(logical, char::from(b'1' + index as u8)) {
                return Some(Command::SelectTab(index));
            }
        }
        if code(physical, KeyCode::Digit9) || character(logical, '9') {
            return Some(Command::SelectTab(usize::MAX));
        }
        return None;
    }

    if mods.alt_key() && !mods.control_key() && !mods.super_key() {
        if code(physical, KeyCode::ArrowLeft) {
            return Some(Command::Back);
        }
        if code(physical, KeyCode::ArrowRight) {
            return Some(Command::Forward);
        }
        if letter(physical, logical, KeyCode::KeyD, 'd') {
            return Some(Command::FocusAddress);
        }
        if named(logical, NamedKey::Home) || code(physical, KeyCode::Home) {
            return Some(Command::Home);
        }
        return None;
    }

    if mods.control_key() || mods.super_key() || mods.alt_key() {
        return None;
    }

    // Unmodified function keys.
    if code(physical, KeyCode::F12) {
        return Some(Command::DevTools);
    }
    if code(physical, KeyCode::F5) {
        return Some(Command::Reload);
    }
    if code(physical, KeyCode::F6) {
        return Some(Command::FocusAddress);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{command, Command};
    use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

    fn press(code: KeyCode, ch: &str, mods: ModifiersState) -> Option<Command> {
        command(PhysicalKey::Code(code), &Key::Character(ch.into()), mods)
    }

    #[test]
    fn primary_modifier_works_as_ctrl_or_super() {
        for mods in [ModifiersState::CONTROL, ModifiersState::SUPER] {
            assert_eq!(press(KeyCode::KeyA, "a", mods), Some(Command::SelectAll));
            assert_eq!(press(KeyCode::KeyC, "c", mods), Some(Command::Copy));
            assert_eq!(press(KeyCode::KeyV, "v", mods), Some(Command::Paste));
            assert_eq!(press(KeyCode::KeyT, "t", mods), Some(Command::NewTab));
            assert_eq!(press(KeyCode::KeyW, "w", mods), Some(Command::CloseTab));
            assert_eq!(press(KeyCode::KeyL, "l", mods), Some(Command::FocusAddress));
        }
    }

    #[test]
    fn shifted_commands_are_distinct_from_unshifted_ones() {
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(press(KeyCode::KeyT, "t", mods), Some(Command::ReopenTab));
        assert_eq!(press(KeyCode::KeyR, "r", mods), Some(Command::Reload));
        assert_eq!(press(KeyCode::KeyC, "c", mods), Some(Command::Inspect));
        assert_eq!(press(KeyCode::KeyI, "i", mods), Some(Command::DevTools));
        assert_eq!(press(KeyCode::KeyJ, "j", mods), Some(Command::DevTools));
        // Ctrl+Shift+C must never fall through to Copy.
        assert_ne!(press(KeyCode::KeyC, "c", mods), Some(Command::Copy));
    }

    #[test]
    fn tab_switching_accepts_tab_and_page_keys() {
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            command(
                PhysicalKey::Code(KeyCode::Tab),
                &Key::Named(NamedKey::Tab),
                ctrl
            ),
            Some(Command::NextTab)
        );
        assert_eq!(
            command(
                PhysicalKey::Code(KeyCode::Tab),
                &Key::Named(NamedKey::Tab),
                ctrl | ModifiersState::SHIFT
            ),
            Some(Command::PrevTab)
        );
        assert_eq!(press(KeyCode::PageDown, "", ctrl), Some(Command::NextTab));
        assert_eq!(press(KeyCode::PageUp, "", ctrl), Some(Command::PrevTab));
    }

    #[test]
    fn number_keys_select_tabs_and_nine_is_last() {
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            press(KeyCode::Digit1, "1", ctrl),
            Some(Command::SelectTab(0))
        );
        assert_eq!(
            press(KeyCode::Digit8, "8", ctrl),
            Some(Command::SelectTab(7))
        );
        assert_eq!(
            press(KeyCode::Digit9, "9", ctrl),
            Some(Command::SelectTab(usize::MAX))
        );
    }

    #[test]
    fn alt_combinations_do_not_steal_altgr_typing() {
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(press(KeyCode::KeyD, "d", ctrl_alt), None);
        // Plain Alt+D focuses the address bar; Alt+Left goes back.
        assert_eq!(
            press(KeyCode::KeyD, "d", ModifiersState::ALT),
            Some(Command::FocusAddress)
        );
        assert_eq!(
            command(
                PhysicalKey::Code(KeyCode::ArrowLeft),
                &Key::Named(NamedKey::ArrowLeft),
                ModifiersState::ALT
            ),
            Some(Command::Back)
        );
    }

    #[test]
    fn function_keys_and_bare_letters() {
        assert_eq!(
            command(
                PhysicalKey::Code(KeyCode::F12),
                &Key::Named(NamedKey::F12),
                ModifiersState::empty()
            ),
            Some(Command::DevTools)
        );
        assert_eq!(
            command(
                PhysicalKey::Code(KeyCode::F5),
                &Key::Named(NamedKey::F5),
                ModifiersState::empty()
            ),
            Some(Command::Reload)
        );
        // A bare letter is page input, not a browser shortcut.
        assert_eq!(press(KeyCode::KeyA, "a", ModifiersState::empty()), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_option_devtools_combos() {
        let cmd_alt = ModifiersState::SUPER | ModifiersState::ALT;
        assert_eq!(press(KeyCode::KeyI, "i", cmd_alt), Some(Command::DevTools));
        assert_eq!(press(KeyCode::KeyJ, "j", cmd_alt), Some(Command::DevTools));
        assert_eq!(press(KeyCode::KeyC, "c", cmd_alt), Some(Command::Inspect));
    }

    #[test]
    fn bracket_keys_walk_history() {
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(press(KeyCode::BracketLeft, "[", ctrl), Some(Command::Back));
        assert_eq!(
            press(KeyCode::BracketRight, "]", ctrl),
            Some(Command::Forward)
        );
    }
}
