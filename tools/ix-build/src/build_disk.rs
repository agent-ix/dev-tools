// SPDX-License-Identifier: MIT
//! Host build disk selection; policy remains the authority after initialization.

use crate::error::{Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct Candidate {
    pub root: PathBuf,
    pub available: u64,
    pub speed: u8,
    pub home: bool,
}

pub(crate) fn select(
    candidates: &[Candidate],
    mut writable: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let mut candidates: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.home || candidate.available >= 100_000_000_000)
        .collect();
    candidates.sort_by(|left, right| {
        (right.speed, right.home, right.available)
            .cmp(&(left.speed, left.home, left.available))
            .then_with(|| left.root.cmp(&right.root))
    });
    candidates
        .into_iter()
        .find(|candidate| writable(&candidate.root))
        .map(|candidate| candidate.root.clone())
}

pub(crate) fn default_root(home: Option<&Path>, disks: &sysinfo::Disks) -> Result<PathBuf> {
    let host_root = std::path::absolute(crate::writable::validated_host_root(home)?)?;
    let home_disk = disks
        .list()
        .iter()
        .filter(|disk| host_root.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count());
    let mut candidates = Vec::new();
    if home_disk.is_none_or(|disk| {
        eligible_home(
            disk.file_system(),
            disk.is_removable(),
            disk.is_read_only(),
            cfg!(target_os = "macos"),
        )
    }) {
        candidates.push(Candidate {
            root: host_root,
            available: home_disk.map_or(0, |disk| disk.available_space()),
            speed: home_disk.map_or(0, disk_speed),
            home: true,
        });
    }
    for disk in disks.list() {
        if disk_speed(disk) == 0
            || !eligible(disk.file_system(), disk.is_removable(), disk.is_read_only())
        {
            continue;
        }
        candidates.push(Candidate {
            root: disk
                .mount_point()
                .join(format!("ix-build-{}", nix::unistd::geteuid())),
            available: disk.available_space(),
            speed: disk_speed(disk),
            home: false,
        });
    }
    select(&candidates, writable_probe).ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "no writable build disk for target_root",
        )
    })
}

fn eligible(fs: &std::ffi::OsStr, removable: bool, readonly: bool) -> bool {
    !removable && !readonly && local_filesystem(fs)
}

fn eligible_home(fs: &std::ffi::OsStr, removable: bool, readonly: bool, macos: bool) -> bool {
    // /Users firmlinks into writable APFS Data even when the longest lexical
    // mount prefix is sealed '/'. The HOME file probe is authoritative there.
    eligible(fs, removable, readonly) || (macos && fs == "apfs" && !removable)
}

fn local_filesystem(fs: &std::ffi::OsStr) -> bool {
    matches!(
        fs.to_str(),
        Some("ext4" | "ext3" | "xfs" | "btrfs" | "f2fs" | "zfs" | "apfs" | "hfs" | "hfsplus")
    )
}

fn disk_speed(disk: &sysinfo::Disk) -> u8 {
    storage_speed(disk.name(), disk.kind())
}

fn storage_speed(name: &std::ffi::OsStr, kind: sysinfo::DiskKind) -> u8 {
    let nvme = name
        .to_str()
        .and_then(|name| Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.strip_prefix("nvme")
                .is_some_and(|tail| tail.starts_with(|ch: char| ch.is_ascii_digit()))
        });
    if cfg!(target_os = "linux") && nvme {
        3
    } else {
        match kind {
            sysinfo::DiskKind::SSD => 2,
            sysinfo::DiskKind::HDD => 1,
            _ => 0,
        }
    }
}

fn writable_probe(root: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(parent) = root.parent()
        && std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .is_err()
    {
        return false;
    }
    let Ok(root) = crate::writable::private_directory(root) else {
        return false;
    };
    let Ok(id) = crate::store::random_id() else {
        return false;
    };
    let path = root.join(format!(".write-probe-{id}"));
    let writable = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .is_ok();
    let _ = std::fs::remove_file(path);
    writable
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provenance: SWM-412. Trace: FR-011-AC-7
    #[test]
    fn fresh_home_without_ix_or_an_extra_disk_can_initialize() -> Result<()> {
        let home = tempfile::tempdir()?;
        let expected = home.path().join(".ix/ix-build");
        assert!(!home.path().join(".ix").exists());
        assert_eq!(
            default_root(Some(home.path()), &sysinfo::Disks::new())?,
            expected
        );
        assert!(expected.is_dir());
        assert!(std::fs::read_dir(expected)?.next().is_none());
        Ok(())
    }

    /// Provenance: SWM-412. Trace: FR-011-AC-7
    #[test]
    fn disk_metadata_excludes_network_and_temporary_mounts() {
        for fs in ["tmpfs", "proc", "nfs4", "cifs", "overlay"] {
            assert!(!local_filesystem(std::ffi::OsStr::new(fs)));
        }
        for fs in ["ext4", "xfs", "btrfs", "apfs"] {
            assert!(local_filesystem(std::ffi::OsStr::new(fs)));
        }
        assert!(!eligible(std::ffi::OsStr::new("ext4"), true, false));
        assert!(!eligible(std::ffi::OsStr::new("ext4"), false, true));
        assert!(!eligible(std::ffi::OsStr::new("nfs4"), false, false));
        assert!(!eligible(std::ffi::OsStr::new("tmpfs"), false, false));
        assert!(eligible_home(
            std::ffi::OsStr::new("apfs"),
            false,
            true,
            true
        ));
        assert!(!eligible_home(
            std::ffi::OsStr::new("nfs4"),
            false,
            false,
            true
        ));
        assert!(!eligible_home(
            std::ffi::OsStr::new("ext4"),
            false,
            true,
            false
        ));
        assert_eq!(
            storage_speed(std::ffi::OsStr::new("/dev/sda1"), sysinfo::DiskKind::SSD),
            2
        );
        assert_eq!(
            storage_speed(std::ffi::OsStr::new("/dev/sdb1"), sysinfo::DiskKind::HDD),
            1
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            storage_speed(
                std::ffi::OsStr::new("/dev/nvme0n1p1"),
                sysinfo::DiskKind::Unknown(-1)
            ),
            3
        );
    }

    /// Provenance: SWM-412. Trace: FR-011-AC-7
    #[test]
    fn nvme_wins_over_root_sata_ssd_without_a_throughput_benchmark() {
        let candidates = [
            Candidate {
                root: "/home/peter/.ix/ix-build".into(),
                available: 550_000_000_000,
                speed: 2,
                home: true,
            },
            Candidate {
                root: "/mnt/build/ix-build-1000".into(),
                available: 520_000_000_000,
                speed: 3,
                home: false,
            },
        ];
        assert_eq!(
            select(&candidates, |_| true),
            Some(PathBuf::from("/mnt/build/ix-build-1000"))
        );
        assert_eq!(
            select(&candidates, |path| !path.starts_with("/mnt/build")),
            Some(PathBuf::from("/home/peter/.ix/ix-build"))
        );
        let mut full = candidates.clone();
        full[1].available = 90_000_000_000;
        assert_eq!(
            select(&full, |_| true),
            Some(PathBuf::from("/home/peter/.ix/ix-build"))
        );
    }
}
