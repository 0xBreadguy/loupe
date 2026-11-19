//! Filesystem headroom from directory metadata and filesystem counters.
use std::path::Path;

#[cfg(target_os = "linux")]
mod linux;

/// A volume's capacity and space available to the calling user, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub device: u64,
    pub available_bytes: u64,
    pub total_bytes: u64,
    /// Whether this filesystem is tmpfs; unknown on unsupported platforms.
    pub is_tmpfs: Option<bool>,
}

/// Strict thresholds: exactly 10% and exactly 1 GiB are not low by themselves.
/// A zero total is unknown rather than healthy or low.
pub fn is_low_space(available: u64, total: u64) -> Option<bool> {
    if total == 0 {
        return None;
    }
    Some(available < 1024 * 1024 * 1024 || (available as u128) * 10 < total as u128)
}

/// A sampled device and the roles it serves. Labels are display only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarningVolume {
    pub volume: Volume,
    pub worktrees: bool,
    pub temporary: bool,
    pub worktree_mount: String,
    pub temp_mount: String,
    /// Present only when effective temp is verified to be the root of a tmpfs
    /// filesystem on a device separate from the watched worktrees.
    pub dedicated_temp: Option<DedicatedTemp>,
}

/// Whole-filesystem usage for a verified dedicated temp volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedicatedTemp {
    /// Allocated bytes, computed from total minus all free blocks (including
    /// reserves). Unavailable if the filesystem supplies unusable counters.
    pub used_bytes: Option<u64>,
}

impl WarningVolume {
    /// Unknown headroom cannot establish that a volume is low.
    pub fn low(&self) -> bool {
        is_low_space(self.volume.available_bytes, self.volume.total_bytes) == Some(true)
    }
}

/// Fold synthetic measurements by device, preferring the injected temp counters
/// for a shared device. Synthetic measurements do not imply a dedicated tmpfs.
pub fn warning_volumes(
    worktrees: impl IntoIterator<Item = (Volume, String)>,
    temporary: Option<(Volume, String)>,
) -> Vec<WarningVolume> {
    let mut out: Vec<WarningVolume> = Vec::new();
    for (volume, mount) in worktrees {
        if let Some(existing) = out.iter_mut().find(|v| v.volume.device == volume.device) {
            existing.worktrees = true;
            if existing.worktree_mount.is_empty() {
                existing.worktree_mount = mount;
            }
        } else {
            out.push(WarningVolume {
                volume,
                worktrees: true,
                temporary: false,
                worktree_mount: mount,
                temp_mount: String::new(),
                dedicated_temp: None,
            });
        }
    }
    if let Some((volume, mount)) = temporary {
        if let Some(existing) = out.iter_mut().find(|v| v.volume.device == volume.device) {
            existing.temporary = true;
            existing.volume = volume;
            existing.temp_mount = mount;
        } else {
            out.push(WarningVolume {
                volume,
                worktrees: false,
                temporary: true,
                worktree_mount: String::new(),
                temp_mount: mount,
                dedicated_temp: None,
            });
        }
    }
    out
}

/// Take one capacity sample per watched device. The managed path can fall back
/// to its nearest existing ancestor if absent. Other checkouts must be explicit,
/// absolute local directory paths. Neither probing nor labeling opens files,
/// enumerates directory entries, or reads a mount table.
#[cfg(target_os = "linux")]
pub fn sample<'a>(
    managed: &'a Path,
    recorded: impl IntoIterator<Item = &'a Path>,
    temporary: &Path,
) -> Vec<WarningVolume> {
    linux::sample(managed, recorded, temporary)
}

#[cfg(not(target_os = "linux"))]
pub fn sample<'a>(
    _managed: &'a Path,
    _recorded: impl IntoIterator<Item = &'a Path>,
    _temporary: &Path,
) -> Vec<WarningVolume> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thresholds_are_strict_and_unknown_is_not_healthy() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(is_low_space(gib - 1, 10 * gib), Some(true));
        assert_eq!(is_low_space(gib, 10 * gib), Some(false));
        assert_eq!(is_low_space(gib, 10 * gib + 1), Some(true));
        assert_eq!(is_low_space(0, 0), None);
    }
}
