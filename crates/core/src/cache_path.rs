//! Location of the optional, disposable Insights cache.
use std::path::{Path, PathBuf};

/// Return the cache database path, or `None` when caching is disabled or no home is known.
pub fn insights_file() -> Option<PathBuf> {
    resolve(
        std::env::var_os("T3_AGENTS_NO_CACHE").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("T3_AGENTS_INSTALL_NAME").as_deref(),
    )
}

fn resolve(
    disabled: Option<&std::ffi::OsStr>,
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    name: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if disabled == Some(std::ffi::OsStr::new("1")) {
        return None;
    }
    let base = xdg
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| {
            home.map(Path::new)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".cache"))
        })?;
    let mut dir = base.join("t3-agents");
    if let Some(name) = name.and_then(|s| s.to_str()).filter(|s| valid_name(s)) {
        if name != "t3-agents" {
            dir = base.join(name);
        }
    }
    Some(dir.join("insights.sqlite3"))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.bytes();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    fn path(disabled: &str, xdg: &str, home: &str, name: &str) -> Option<PathBuf> {
        resolve(
            Some(OsStr::new(disabled)),
            Some(OsStr::new(xdg)),
            Some(OsStr::new(home)),
            Some(OsStr::new(name)),
        )
    }
    #[test]
    fn cache_paths_follow_xdg_and_install_name() {
        assert_eq!(
            path("", "/cache", "/home/u", "t3-agents"),
            Some("/cache/t3-agents/insights.sqlite3".into())
        );
        assert_eq!(
            path("", "/cache", "/home/u", "other"),
            Some("/cache/other/insights.sqlite3".into())
        );
        assert_eq!(
            path("", "relative", "/home/u", "../bad"),
            Some("/home/u/.cache/t3-agents/insights.sqlite3".into())
        );
        assert_eq!(path("1", "/cache", "/home/u", ""), None);
        assert_eq!(path("", "relative", "relative", ""), None);
    }
}
