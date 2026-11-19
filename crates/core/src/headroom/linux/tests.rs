use super::*;

fn dir(device: u64, mount_id: u64, at_mount_root: bool) -> Directory {
    Directory {
        device,
        mount_id: Some(mount_id),
        at_mount_root,
    }
}

fn counts(is_tmpfs: bool) -> Capacity {
    Capacity {
        unit: 4_096,
        total: 100,
        available: 30,
        free: 40,
        is_tmpfs,
    }
}

#[test]
fn samples_managed_and_explicit_devices_once_including_mounts_under_managed() {
    let managed = Path::new("/synthetic/data/worktrees");
    let nested_mount = managed.join("nested-mount");
    let explicit = Path::new("/synthetic/extra/checkout");
    let temp = Path::new("/synthetic/temp");
    let mut explicit_probes = 0;
    let mut measured = Vec::new();
    let result = sample_with(
        managed,
        [
            nested_mount.as_path(),
            Path::new("/synthetic/data/worktrees/normal"),
            explicit,
            explicit,
            Path::new("/synthetic/stale"),
            Path::new("/synthetic/inaccessible"),
            Path::new("/synthetic/file"),
            Path::new("/synthetic/nfs"),
            Path::new("relative"),
            Path::new("ssh://host/checkout"),
            Path::new("//host/share/checkout"),
        ],
        temp,
        |path| match path.to_str().unwrap() {
            "/synthetic/data/worktrees" => Ok(dir(1, 10, true)),
            "/synthetic/data/worktrees/nested-mount" => Ok(dir(2, 20, true)),
            "/synthetic/data/worktrees/normal" => Ok(dir(1, 10, false)),
            "/synthetic/extra/checkout" => {
                explicit_probes += 1;
                Ok(dir(3, 30, false))
            }
            "/synthetic/temp" => Ok(dir(3, 31, true)), // Same device through another mount.
            "/synthetic/nfs" => Ok(dir(4, 40, true)),
            "/synthetic/stale" => Err(io::ErrorKind::NotFound.into()),
            "/synthetic/inaccessible" => Err(io::ErrorKind::PermissionDenied.into()),
            "/synthetic/file" => Err(io::ErrorKind::NotADirectory.into()),
            _ => panic!("unexpected probe of {}", path.display()),
        },
        |path| {
            measured.push(path.to_owned());
            Some(counts(path == explicit))
        },
        |id| {
            (id == 40).then(|| MountInfo {
                point: "/synthetic/nfs".into(),
                magic: libc::NFS_SUPER_MAGIC as u32,
                whole_filesystem: true,
            })
        },
    );
    assert_eq!(explicit_probes, 1);
    assert_eq!(measured, [managed, nested_mount.as_path(), explicit]);
    assert_eq!(result.len(), 3);
    assert!(
        result
            .iter()
            .all(|v| v.worktrees && v.dedicated_temp.is_none())
    );
    assert_eq!(result[0].worktree_mount, managed.to_str().unwrap());
    assert_eq!(result[1].volume.device, 2);
    assert!(result[2].temporary);
    assert_eq!(result[2].temp_mount, temp.to_str().unwrap());
}

#[test]
fn only_a_missing_managed_directory_falls_back_to_an_existing_ancestor() {
    let managed = Path::new("/synthetic/data/new/worktrees");
    let ancestor = Path::new("/synthetic/data");
    for (kind, expected) in [
        (io::ErrorKind::NotFound, Some(ancestor)),
        (io::ErrorKind::PermissionDenied, None),
        (io::ErrorKind::NotADirectory, None),
    ] {
        let mut measured = None;
        let result = sample_with(
            managed,
            [Path::new("/synthetic/gone/checkout")],
            Path::new("/synthetic/gone/temp"),
            |path| {
                if path == managed {
                    Err(kind.into())
                } else if path == ancestor {
                    Ok(dir(1, 10, false))
                } else {
                    assert!(path == managed.parent().unwrap() || !path.starts_with(ancestor));
                    Err(io::ErrorKind::NotFound.into())
                }
            },
            |path| {
                measured = Some(path.to_owned());
                Some(counts(false))
            },
            |_| None,
        );
        assert_eq!(measured.as_deref(), expected);
        assert_eq!(result.len(), usize::from(expected.is_some()));
        if let Some(path) = expected {
            assert_eq!(result[0].worktree_mount, path.to_str().unwrap());
            assert!(!result[0].temporary);
        }
    }
}

#[test]
fn dedicated_temp_requires_a_verified_tmpfs_root_on_its_own_device() {
    let managed = Path::new("/synthetic/worktrees");
    let temp = Path::new("/synthetic/temp");
    for (at_mount_root, whole_filesystem, is_tmpfs, used) in [
        (true, Some(true), true, Some(60 * 4_096)),
        (true, Some(false), true, None), // Bind mount of a tmpfs subdirectory.
        (false, Some(true), true, None), // Directory inside an ordinary tmpfs mount.
        (true, None, true, None),        // Mount metadata unsupported or denied.
        (true, Some(true), false, None),
    ] {
        let result = sample_with(
            managed,
            [],
            temp,
            |path| {
                if path == managed {
                    Ok(dir(1, 10, false))
                } else {
                    assert_eq!(path, temp);
                    Ok(dir(2, 20, at_mount_root))
                }
            },
            |path| Some(counts(path == temp && is_tmpfs)),
            |id| {
                (id == 20).then_some(())?;
                Some(MountInfo {
                    point: "/synthetic/temp\n\\\u{202e}é".into(),
                    magic: libc::TMPFS_MAGIC as u32,
                    whole_filesystem: whole_filesystem?,
                })
            },
        );
        assert_eq!(result.len(), 2);
        let sample = &result[1];
        assert!(sample.temporary && !sample.worktrees);
        assert_eq!(
            sample.dedicated_temp,
            used.map(|bytes| DedicatedTemp {
                used_bytes: Some(bytes)
            })
        );
        assert_eq!(sample.volume.available_bytes, 30 * 4_096);
        assert_eq!(
            sample.temp_mount,
            if whole_filesystem.is_some() {
                r"/synthetic/temp\n\\\u{202e}é"
            } else {
                "/synthetic/temp"
            },
        );
    }
}

#[test]
fn labels_preserve_utf8_and_escape_other_bytes() {
    let path = Path::new(std::ffi::OsStr::from_bytes(
        b"/scratch/cafe\xcc\x81'/\xff\x1b",
    ));
    assert_eq!(label(path), r"/scratch/café'/\xff\u{1b}");
}

#[test]
fn unavailable_used_bytes_do_not_turn_into_zero_usage() {
    let mut fs = counts(true);
    fs.free = fs.total + 1;
    assert_eq!(fs.used_bytes(), None);
    fs.total = 0;
    fs.free = 0;
    assert_eq!(fs.used_bytes(), None);
}

#[test]
fn kernel_metadata_samples_synthetic_directories() {
    // The replay can trace this test without mounting anything or inventorying the host.
    let root = std::env::temp_dir().join(format!("t3-headroom-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let managed = root.join("worktrees");
    let temp = root.join("temp");
    std::fs::create_dir(&managed).unwrap();
    std::fs::create_dir(&temp).unwrap();

    let result = sample(&managed, [root.join("gone").as_path()], &temp);

    std::fs::remove_dir(&temp).unwrap();
    std::fs::remove_dir(&managed).unwrap();
    std::fs::remove_dir(&root).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].worktrees && result[0].temporary);
    assert!(result[0].volume.total_bytes > 0);
    assert!(result[0].dedicated_temp.is_none());
}
