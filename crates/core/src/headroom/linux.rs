//! Only probe the configured roots and distinct projected checkout paths.
//! A checkout underneath the managed root can itself be on another device,
//! so lexical containment cannot replace its directory lookup. Capacity and
//! labels are resolved only for selected devices, on the host refresh interval.
//!
//! statx identifies a directory's device, statfs supplies capacity, and statmount
//! queries only that directory's mount ID when supported. An observed directory
//! path is the fallback label. Mount roots alone cannot prove dedicated temp:
//! a bind mount may expose only part of a tmpfs filesystem.

use super::{DedicatedTemp, Volume, WarningVolume};
use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub(super) fn sample<'a>(
    managed: &'a Path,
    recorded: impl IntoIterator<Item = &'a Path>,
    temporary: &Path,
) -> Vec<WarningVolume> {
    sample_with(
        managed, recorded, temporary, directory, capacity, mount_info,
    )
}

#[derive(Clone, Copy)]
struct Directory {
    device: u64,
    mount_id: Option<u64>,
    at_mount_root: bool,
}

struct Capacity {
    unit: u64,
    total: u64,
    available: u64,
    free: u64,
    is_tmpfs: bool,
}

impl Capacity {
    fn used_bytes(&self) -> Option<u64> {
        if self.unit == 0 || self.total == 0 {
            return None;
        }
        self.total.checked_sub(self.free)?.checked_mul(self.unit)
    }
}

struct MountInfo {
    point: PathBuf,
    magic: u32,
    whole_filesystem: bool,
}

fn sample_with<'a>(
    managed: &'a Path,
    recorded: impl IntoIterator<Item = &'a Path>,
    temporary: &Path,
    mut directory: impl FnMut(&Path) -> io::Result<Directory>,
    mut capacity: impl FnMut(&Path) -> Option<Capacity>,
    mut mount_info: impl FnMut(u64) -> Option<MountInfo>,
) -> Vec<WarningVolume> {
    // The managed location may not have been created yet. Only absence permits
    // falling back. Permission errors and non-directory components stay unknown.
    let mut seen = HashSet::new();
    let mut devices = HashSet::new();
    let mut worktrees = Vec::new();
    for path in managed.ancestors() {
        seen.insert(path);
        match directory(path) {
            Ok(dir) => {
                devices.insert(dir.device);
                worktrees.push((path, dir));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
    }
    for path in recorded {
        if !path.is_absolute() || path.as_os_str().as_bytes().starts_with(b"//") {
            continue;
        }
        if seen.insert(path)
            && let Ok(dir) = directory(path)
            && devices.insert(dir.device)
        {
            worktrees.push((path, dir));
        }
    }

    let temporary_dir = directory(temporary).ok();
    let separate_temp = temporary_dir.filter(|dir| !devices.contains(&dir.device));
    let mut result = Vec::new();
    let candidates = worktrees
        .into_iter()
        .map(|(path, dir)| (path, dir, true))
        .chain(separate_temp.map(|dir| (temporary, dir, false)));
    for (path, dir, worktree) in candidates {
        let mount = dir.mount_id.and_then(&mut mount_info);
        if mount.as_ref().is_some_and(|m| !local_filesystem(m.magic)) {
            continue;
        }
        let Some(counts) = capacity(path) else {
            continue;
        };
        let shared = worktree && temporary_dir.is_some_and(|temp| temp.device == dir.device);
        let path_label = mount.as_ref().map_or(path, |m| &m.point);
        let temp_label = if shared
            && (mount.is_none() || temporary_dir.and_then(|d| d.mount_id) != dir.mount_id)
        {
            temporary
        } else {
            path_label
        };
        let whole_tmpfs = !worktree
            && counts.is_tmpfs
            && dir.at_mount_root
            && mount.as_ref().is_some_and(|m| m.whole_filesystem);
        if let Some(sample) = volume(
            dir,
            counts,
            worktree.then_some(path_label),
            (!worktree || shared).then_some(temp_label),
            whole_tmpfs,
        ) {
            result.push(sample);
        }
    }
    result
}

fn volume(
    dir: Directory,
    counts: Capacity,
    worktree: Option<&Path>,
    temporary: Option<&Path>,
    whole_tmpfs: bool,
) -> Option<WarningVolume> {
    Some(WarningVolume {
        volume: Volume {
            device: dir.device,
            available_bytes: counts.available.checked_mul(counts.unit)?,
            total_bytes: counts.total.checked_mul(counts.unit)?,
            is_tmpfs: Some(counts.is_tmpfs),
        },
        worktrees: worktree.is_some(),
        temporary: temporary.is_some(),
        worktree_mount: worktree.map(label).unwrap_or_default(),
        temp_mount: temporary.map(label).unwrap_or_default(),
        dedicated_temp: whole_tmpfs.then(|| DedicatedTemp {
            used_bytes: counts.used_bytes(),
        }),
    })
}

fn label(path: &Path) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    for chunk in path.as_os_str().as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            if crate::terminal::is_hidden(c) || matches!(c, '\\' | '\u{2028}' | '\u{2029}') {
                out.extend(c.escape_default());
            } else {
                out.push(c);
            }
        }
        for byte in chunk.invalid() {
            write!(out, "\\x{byte:02x}").expect("writing to String cannot fail");
        }
    }
    out
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::from)
}

fn directory(path: &Path) -> io::Result<Directory> {
    let path = c_path(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statx>::uninit();
    // SAFETY: the path is NUL-terminated and stat is read only on success.
    let stat = unsafe {
        if libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_NO_AUTOMOUNT | libc::AT_STATX_DONT_SYNC,
            libc::STATX_TYPE | libc::STATX_MNT_ID_UNIQUE,
            stat.as_mut_ptr(),
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        stat.assume_init()
    };
    if stat.stx_mask & libc::STATX_TYPE == 0
        || stat.stx_mode & libc::S_IFMT as u16 != libc::S_IFDIR as u16
    {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    if stat.stx_attributes & libc::STATX_ATTR_AUTOMOUNT as u64 != 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    // SAFETY: access only tests search permission for this NUL-terminated directory path.
    if unsafe { libc::access(path.as_ptr(), libc::X_OK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mount_root = libc::STATX_ATTR_MOUNT_ROOT as u64;
    Ok(Directory {
        device: libc::makedev(stat.stx_dev_major, stat.stx_dev_minor) as u64,
        mount_id: (stat.stx_mask & libc::STATX_MNT_ID_UNIQUE != 0).then_some(stat.stx_mnt_id),
        at_mount_root: stat.stx_attributes_mask & stat.stx_attributes & mount_root != 0,
    })
}

fn capacity(path: &Path) -> Option<Capacity> {
    let path = c_path(path).ok()?;
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated and fs is read only on success.
    let fs = unsafe {
        if libc::statfs(path.as_ptr(), fs.as_mut_ptr()) != 0 {
            return None;
        }
        fs.assume_init()
    };
    let magic = fs.f_type as u32;
    if !local_filesystem(magic) {
        return None;
    }
    let unit = if fs.f_frsize > 0 {
        fs.f_frsize
    } else {
        fs.f_bsize
    };
    Some(Capacity {
        unit: u64::try_from(unit).ok()?,
        total: fs.f_blocks as u64,
        available: fs.f_bavail as u64,
        free: fs.f_bfree as u64,
        is_tmpfs: magic == libc::TMPFS_MAGIC as u32,
    })
}

fn local_filesystem(magic: u32) -> bool {
    // statfs does not distinguish local FUSE implementations from remote ones
    // such as sshfs. Keep only known local filesystem types (linux/magic.h).
    const LOCAL: &[u32] = &[
        libc::TMPFS_MAGIC as u32,
        libc::EXT4_SUPER_MAGIC as u32,
        libc::BTRFS_SUPER_MAGIC as u32,
        libc::F2FS_SUPER_MAGIC as u32,
        libc::BCACHEFS_SUPER_MAGIC as u32,
        libc::NILFS_SUPER_MAGIC as u32,
        libc::REISERFS_SUPER_MAGIC as u32,
        libc::MSDOS_SUPER_MAGIC as u32,
        libc::OVERLAYFS_SUPER_MAGIC as u32,
        libc::ECRYPTFS_SUPER_MAGIC as u32,
        libc::HUGETLBFS_MAGIC as u32,
        libc::UDF_SUPER_MAGIC as u32,
        0x858458f6, // RAMFS
        0x58465342, // XFS
        0x2fc12fc1, // ZFS
        0x3153464a, // JFS
        0x2011bab0, // exFAT
        0x5346544e, // NTFS
    ];
    LOCAL.contains(&magic)
}

// statx's MOUNT_ROOT also describes bind mounts of subdirectories. statmount
// proves that a mount root is the filesystem root by returning "/" in mnt_root.
// If the kernel, sandbox, or architecture cannot provide it, retain the
// sampled directory label and leave dedicated-temp usage unavailable.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn mount_info(mount_id: u64) -> Option<MountInfo> {
    use std::ffi::CStr;

    // Linux UAPI include/uapi/linux/mount.h, mnt_id_req v0 and the stable
    // statmount prefix through mnt_point. Strings start at byte 512.
    #[repr(C)]
    struct Request {
        size: u32,
        reserved: u32,
        mount_id: u64,
        mask: u64,
    }

    #[repr(C)]
    struct Reply {
        size: u32,
        options: u32,
        mask: u64,
        device: [u32; 2],
        magic: u64,
        unused: [u64; 8],
        root: u32,
        point: u32,
        reserved: [u64; 51],
        strings: [u8; 8192],
    }

    const SYS_STATMOUNT: libc::c_long = 457; // x86-64 and arm64 syscall tables
    const SB_ROOT_AND_POINT: u64 = 0x01 | 0x08 | 0x10;
    const { assert!(std::mem::offset_of!(Reply, strings) == 512) };
    let request = Request {
        size: size_of::<Request>() as u32,
        reserved: 0,
        mount_id,
        mask: SB_ROOT_AND_POINT,
    };
    // SAFETY: both C layouts match the kernel ABI. The initialized output
    // buffer is writable for the supplied size and remains live for parsing.
    let reply: Reply = unsafe {
        let mut reply = std::mem::MaybeUninit::<Reply>::zeroed();
        if libc::syscall(
            SYS_STATMOUNT,
            &request,
            reply.as_mut_ptr(),
            size_of::<Reply>(),
            0u32,
        ) != 0
        {
            return None;
        }
        reply.assume_init()
    };
    if reply.mask & SB_ROOT_AND_POINT != SB_ROOT_AND_POINT {
        return None;
    }
    let strings = reply
        .strings
        .get(..(reply.size as usize).checked_sub(512)?)?;
    let string = |offset| CStr::from_bytes_until_nul(strings.get(offset as usize..)?).ok();
    let point = Path::new(std::ffi::OsStr::from_bytes(string(reply.point)?.to_bytes()));
    point.is_absolute().then(|| MountInfo {
        point: point.to_owned(),
        magic: reply.magic as u32,
        whole_filesystem: string(reply.root).is_some_and(|root| root.to_bytes() == b"/"),
    })
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn mount_info(_mount_id: u64) -> Option<MountInfo> {
    None
}

#[cfg(test)]
mod tests;
