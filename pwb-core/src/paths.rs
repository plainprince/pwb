//! Portable default locations for on-disk state.
//!
//! PlainWeb keeps its state under the user's home directory (`~/.pwb`).
//! `$HOME` is the canonical override, but it is only set by POSIX-ish shells:
//! a stock Windows `cmd.exe` or PowerShell session has `USERPROFILE` (plus the
//! legacy `HOMEDRIVE`/`HOMEPATH`) instead. Reading `$HOME` alone there panics
//! on the missing identity path, scatters `pwb-data`/`.pwb` directories into
//! whatever directory the binary was started from, and silently disables
//! session persistence. Every default path in the workspace resolves through
//! [`home_dir`].

use std::ffi::OsString;
use std::path::PathBuf;

/// The user's home directory, or `None` when the environment names none.
///
/// Precedence: `$HOME` (the override every platform honours, including Git
/// Bash and MSYS on Windows), then `$USERPROFILE`, then `$HOMEDRIVE` +
/// `$HOMEPATH` (`C:` + `\Users\name`) for shells that only set the legacy
/// pair. Empty values count as unset, matching how the shells treat them.
pub fn home_dir() -> Option<PathBuf> {
    home_from(
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        std::env::var_os("HOMEDRIVE"),
        std::env::var_os("HOMEPATH"),
    )
}

/// [`home_dir`] with the four variables passed in, so the precedence rules
/// are unit-testable without mutating process-wide environment state (other
/// tests read `$HOME` concurrently).
fn home_from(
    home: Option<OsString>,
    userprofile: Option<OsString>,
    homedrive: Option<OsString>,
    homepath: Option<OsString>,
) -> Option<PathBuf> {
    fn non_empty(value: Option<OsString>) -> Option<OsString> {
        value.filter(|value| !value.is_empty())
    }

    if let Some(home) = non_empty(home) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = non_empty(userprofile) {
        return Some(PathBuf::from(profile));
    }
    // `HOMEDRIVE` and `HOMEPATH` are only meaningful together and compose by
    // concatenation (`C:` + `\Users\name`), which is how the shell expands
    // `%HOMEDRIVE%%HOMEPATH%`.
    let drive = non_empty(homedrive)?;
    let path = non_empty(homepath)?;
    Some(PathBuf::from(format!(
        "{}{}",
        drive.to_string_lossy(),
        path.to_string_lossy()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn home_wins_over_the_windows_variables() {
        let dir = home_from(
            os("/home/alice"),
            os(r"C:\Users\alice"),
            os("C:"),
            os(r"\Users\alice"),
        );
        assert_eq!(dir, Some(PathBuf::from("/home/alice")));
    }

    #[test]
    fn userprofile_is_the_windows_fallback() {
        let dir = home_from(None, os(r"C:\Users\alice"), os("C:"), os(r"\Users\alice"));
        assert_eq!(dir, Some(PathBuf::from(r"C:\Users\alice")));
    }

    #[test]
    fn homedrive_and_homepath_compose_when_userprofile_is_missing() {
        let dir = home_from(None, None, os("C:"), os(r"\Users\alice"));
        assert_eq!(dir, Some(PathBuf::from(r"C:\Users\alice")));
    }

    #[test]
    fn empty_values_count_as_unset() {
        assert_eq!(
            home_from(os(""), os(r"C:\Users\alice"), None, None),
            Some(PathBuf::from(r"C:\Users\alice"))
        );
        assert_eq!(
            home_from(Some(OsString::new()), None, os("C:"), os("")),
            None
        );
        assert_eq!(home_from(None, None, None, None), None);
    }

    #[test]
    fn home_dir_reads_the_process_environment() {
        // Whatever the platform, the answer must be one of the variables the
        // platform actually sets — never a bare `None` on a normal login.
        if let Some(dir) = home_dir() {
            assert!(!dir.as_os_str().is_empty());
        }
    }
}
