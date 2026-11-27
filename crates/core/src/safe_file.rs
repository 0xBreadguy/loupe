//! Bounded, no-follow reads of files under a T3 data folder.
//!
//! Moved from `telemetry::gather` so Telemetry Check and Storage's cleanup-rules reader
//! share one set of defenses: every path component is opened with `O_NOFOLLOW`, the final
//! file with `O_NONBLOCK` (a FIFO never blocks) and must be a regular file, and the caller
//! caps the size before reading.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _};
use std::path::Path;

/// Walk `components` under `root` component-by-component with `openat(..., O_NOFOLLOW)` so
/// neither intermediate directories (e.g. `userdata -> /attacker`) nor the final file can
/// follow a symlink. Opens the final regular file with `O_NONBLOCK` so FIFOs never block.
pub(crate) fn open_under_root_nofollow(
    root: &Path,
    components: &[&str],
) -> Result<(File, fs::Metadata), ReadError> {
    if components.is_empty() {
        return Err(ReadError::Absent);
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

        let root_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    ReadError::Absent
                } else {
                    ReadError::Unreadable(io_kind_label(e.kind()))
                }
            })?;
        let mut dir_fd: OwnedFd = root_file.into();

        for &dir_name in &components[..components.len() - 1] {
            let c_name =
                CString::new(dir_name).map_err(|_| ReadError::Unreadable("invalid path".into()))?;
            let next_raw = unsafe {
                libc::openat(
                    dir_fd.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if next_raw < 0 {
                let err = io::Error::last_os_error();
                return Err(map_nofollow_open_err(err));
            }
            dir_fd = unsafe { OwnedFd::from_raw_fd(next_raw) };
        }

        let last = *components.last().unwrap();
        let c_last =
            CString::new(last).map_err(|_| ReadError::Unreadable("invalid path".into()))?;
        let file_raw = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                c_last.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if file_raw < 0 {
            let err = io::Error::last_os_error();
            return Err(map_nofollow_open_err(err));
        }
        let file = unsafe { File::from_raw_fd(file_raw) };
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
    #[cfg(not(unix))]
    {
        let mut cur = root.to_path_buf();
        for &comp in components {
            cur.push(comp);
            if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(ReadError::Unreadable("symlink not followed".into()));
            }
        }
        let file = OpenOptions::new().read(true).open(&cur).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ReadError::Absent
            } else {
                ReadError::Unreadable(io_kind_label(e.kind()))
            }
        })?;
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
}

#[cfg(unix)]
fn map_nofollow_open_err(err: io::Error) -> ReadError {
    if err.kind() == io::ErrorKind::NotFound {
        ReadError::Absent
    } else if err.raw_os_error() == Some(libc::ELOOP) || err.raw_os_error() == Some(libc::ENOTDIR) {
        // Note: on Linux `openat(..., O_DIRECTORY | O_NOFOLLOW)` returns ELOOP on a symlink;
        // on macOS/BSD it can return ENOTDIR when a symlink is opened with O_DIRECTORY | O_NOFOLLOW.
        ReadError::Unreadable("symlink not followed".into())
    } else {
        ReadError::Unreadable(io_kind_label(err.kind()))
    }
}

#[cfg(unix)]
pub(crate) fn stat_under_root_nofollow(root: &Path, components: &[&str]) -> Option<libc::stat> {
    use std::ffi::CString;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

    if components.is_empty() {
        return None;
    }
    let root_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
        .ok()?;
    let mut dir_fd: OwnedFd = root_file.into();
    for &dir_name in &components[..components.len() - 1] {
        let c_name = CString::new(dir_name).ok()?;
        let next_raw = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next_raw < 0 {
            return None;
        }
        dir_fd = unsafe { OwnedFd::from_raw_fd(next_raw) };
    }
    let last = *components.last()?;
    let c_last = CString::new(last).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir_fd.as_raw_fd(),
            c_last.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 { Some(st) } else { None }
}

pub(crate) fn sig_under_root_nofollow(
    root: &Path,
    components: &[&str],
) -> Option<(u64, u64, i128, u64)> {
    #[cfg(unix)]
    {
        let st = stat_under_root_nofollow(root, components)?;
        let mtime_ns = (st.st_mtime as i128) * 1_000_000_000i128 + (st.st_mtime_nsec as i128);
        Some((
            st.st_dev as u64,
            st.st_ino as u64,
            mtime_ns,
            st.st_size.max(0) as u64,
        ))
    }
    #[cfg(not(unix))]
    {
        let mut cur = root.to_path_buf();
        for &comp in components {
            cur.push(comp);
            if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return None;
            }
        }
        file_sig(&cur, false)
    }
}

pub(crate) enum ReadError {
    Absent,
    Unreadable(String),
    TooLarge(u64),
}

pub(crate) fn io_kind_label(kind: io::ErrorKind) -> String {
    match kind {
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        io::ErrorKind::InvalidData => "not valid UTF-8".into(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

/// Read at most `cap` bytes of an already opened regular file as UTF-8.
pub(crate) fn read_bounded(file: File, meta: &fs::Metadata, cap: u64) -> Result<String, ReadError> {
    if meta.len() > cap {
        return Err(ReadError::TooLarge(cap));
    }
    let mut buf = String::new();
    file.take(cap + 1)
        .read_to_string(&mut buf)
        .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
    if buf.len() as u64 > cap {
        return Err(ReadError::TooLarge(cap));
    }
    Ok(buf)
}

#[cfg(not(unix))]
fn file_sig(path: &Path, _follow_symlinks: bool) -> Option<(u64, u64, i128, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    Some((0, 0, mtime, meta.len()))
}
