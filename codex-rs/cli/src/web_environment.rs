use std::ffi::OsString;

/// Keep credentials from the parent Codex process out of MCP shell commands and
/// the tunnel-client process that launches them.
pub(crate) fn safe_subprocess_environment() -> impl Iterator<Item = (OsString, OsString)> {
    const SAFE_VARIABLES: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "COLORTERM",
        "WEB2CODEX_WEB_DATA_DIR",
        // The main Web2Codex process replaces inherited CODEX_HOME with its
        // isolated home before launching MCP and tunnel subprocesses.
        "CODEX_HOME",
        "XDG_DATA_HOME",
        #[cfg(windows)]
        "USERPROFILE",
        #[cfg(windows)]
        "HOMEDRIVE",
        #[cfg(windows)]
        "HOMEPATH",
        #[cfg(windows)]
        "SystemRoot",
        #[cfg(windows)]
        "WINDIR",
        #[cfg(windows)]
        "COMSPEC",
        #[cfg(windows)]
        "PATHEXT",
        #[cfg(windows)]
        "APPDATA",
        #[cfg(windows)]
        "LOCALAPPDATA",
    ];

    SAFE_VARIABLES
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
}
