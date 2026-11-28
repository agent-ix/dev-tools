// SPDX-License-Identifier: MIT
//! Ordered install stages. Only Verified can resume; every earlier drop restores.
use super::*;
use std::marker::PhantomData;

pub(super) struct Checked {
    home: PathBuf,
    shared: PathBuf,
}
pub(super) struct Held;
pub(super) struct Idle;
pub(super) struct Swapped;
pub(super) struct Verified;

pub(super) struct Transaction<S> {
    inner: Option<Inner>,
    stage: PhantomData<S>,
}

struct Inner {
    home: PathBuf,
    shared: PathBuf,
    timeout: Option<Duration>,
    hold: Option<InstallHold>,
    journal: Option<Journal>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    stage: String,
    entries: Vec<Entry>,
    #[serde(default)]
    monitoring: Option<MonitoringSnapshot>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    original: Original,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitoringSnapshot {
    home: PathBuf,
    active: Vec<String>,
    enabled: Vec<String>,
}

fn monitoring_command(args: &[std::ffi::OsString]) -> Result<bool> {
    Ok(Command::new(if cfg!(target_os = "macos") {
        "launchctl"
    } else {
        "systemctl"
    })
    .args(args)
    .output()?
    .status
    .success())
}

impl MonitoringSnapshot {
    fn units() -> &'static [&'static str] {
        if cfg!(target_os = "macos") {
            &[
                "com.agent-ix.locked-build-metrics",
                "com.agent-ix.locked-build-prune",
                "com.agentix.locked-build-metrics",
                "com.agent-ix.ix-build-metrics",
                "com.agent-ix.ix-build-prune",
            ]
        } else {
            &[
                "locked-build-metrics.timer",
                "locked-build-prune.timer",
                "ix-build-metrics.timer",
                "ix-build-prune.timer",
            ]
        }
    }

    fn file(home: &Path, unit: &str) -> PathBuf {
        if cfg!(target_os = "macos") {
            home.join("Library/LaunchAgents")
                .join(format!("{unit}.plist"))
        } else {
            home.join(".config/systemd/user").join(unit)
        }
    }

    fn capture(home: &Path) -> Result<Self> {
        Self::capture_with(home, &mut monitoring_command)
    }

    fn capture_with(
        home: &Path,
        control: &mut impl FnMut(&[std::ffi::OsString]) -> Result<bool>,
    ) -> Result<Self> {
        let mut snapshot = Self {
            home: home.to_owned(),
            active: Vec::new(),
            enabled: Vec::new(),
        };
        for unit in Self::units() {
            if !Self::file(home, unit).exists() {
                continue;
            }
            let active = if cfg!(target_os = "macos") {
                control(
                    &["print", &format!("gui/{}/{unit}", nix::unistd::getuid())]
                        .map(std::ffi::OsString::from),
                )?
            } else {
                if control(&["--user", "is-enabled", unit].map(std::ffi::OsString::from))? {
                    snapshot.enabled.push((*unit).into());
                }
                control(&["--user", "is-active", unit].map(std::ffi::OsString::from))?
            };
            if active {
                snapshot.active.push((*unit).into());
            }
        }
        Ok(snapshot)
    }

    fn stop(&self) -> Result<()> {
        self.stop_with(&mut monitoring_command)
    }

    fn stop_with(
        &self,
        control: &mut impl FnMut(&[std::ffi::OsString]) -> Result<bool>,
    ) -> Result<()> {
        for unit in Self::units() {
            if !Self::file(&self.home, unit).exists() {
                continue;
            }
            let status = if cfg!(target_os = "macos") {
                let target = format!("gui/{}/{unit}", nix::unistd::getuid());
                if !control(&["print", &target].map(std::ffi::OsString::from))? {
                    continue;
                }
                control(&["bootout", &target].map(std::ffi::OsString::from))?
            } else {
                let service = unit.replace(".timer", ".service");
                control(
                    &["--user", "disable", "--now", unit, &service].map(std::ffi::OsString::from),
                )?
            };
            if !status {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "monitoring rollback stop failed",
                ));
            }
        }
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        self.resume_with(&mut monitoring_command)
    }

    fn resume_with(
        &self,
        control: &mut impl FnMut(&[std::ffi::OsString]) -> Result<bool>,
    ) -> Result<()> {
        if !cfg!(target_os = "macos") {
            if self.active.is_empty() && self.enabled.is_empty() {
                return Ok(());
            }
            if !control(&["--user", "daemon-reload"].map(std::ffi::OsString::from))? {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "monitoring rollback reload failed",
                ));
            }
            for unit in &self.enabled {
                if !control(&["--user", "enable", unit].map(std::ffi::OsString::from))? {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "monitoring rollback enable failed",
                    ));
                }
            }
        }
        for unit in &self.active {
            let status = if cfg!(target_os = "macos") {
                control(&[
                    "bootstrap".into(),
                    format!("gui/{}", nix::unistd::getuid()).into(),
                    Self::file(&self.home, unit).into_os_string(),
                ])?
            } else {
                control(&["--user", "start", unit].map(std::ffi::OsString::from))?
            };
            if !status {
                return Err(Error::new(
                    ErrorCode::Configuration,
                    "monitoring rollback restart failed",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Original {
    Missing,
    File { saved: PathBuf },
    Symlink { target: PathBuf },
    Directory,
}

impl Checked {
    pub(super) fn new(home: &Path, shared: &Path) -> Result<Self> {
        if shared.join("install-transaction.json").exists() {
            let journal: Journal =
                serde_json::from_slice(&std::fs::read(shared.join("install-transaction.json"))?)?;
            if journal.stage == "committed" {
                std::fs::remove_file(shared.join("install-transaction.json"))?;
                std::fs::File::open(shared)?.sync_all()?;
            } else {
                return Err(Error::new(
                    ErrorCode::Transition,
                    "interrupted install requires queue resume recovery before another install",
                ));
            }
        }
        Ok(Self {
            home: home.to_owned(),
            shared: shared.to_owned(),
        })
    }
    pub(super) fn hold(self, timeout: Option<Duration>) -> Result<Transaction<Held>> {
        let hold = begin_hold(&self.shared, timeout)?;
        Ok(Transaction {
            inner: Some(Inner {
                home: self.home,
                shared: self.shared,
                timeout,
                hold: Some(hold),
                journal: None,
            }),
            stage: PhantomData,
        })
    }
}

impl<S> Transaction<S> {
    fn inner(&mut self) -> Result<&mut Inner> {
        self.inner
            .as_mut()
            .ok_or_else(|| Error::new(ErrorCode::State, "install transaction consumed"))
    }
    fn advance<T>(mut self) -> Transaction<T> {
        Transaction {
            inner: self.inner.take(),
            stage: PhantomData,
        }
    }
}

impl Transaction<Held> {
    pub(super) fn wait_idle(mut self) -> Result<Transaction<Idle>> {
        let inner = self.inner()?;
        let hold = inner
            .hold
            .take()
            .ok_or_else(|| Error::new(ErrorCode::State, "install hold missing"))?;
        inner.hold = Some(wait_idle(hold, &inner.shared, inner.timeout)?);
        Ok(self.advance())
    }
}

impl Transaction<Idle> {
    pub(super) fn swap(mut self, source: &Path, target: &Path) -> Result<Transaction<Swapped>> {
        let inner = self.inner()?;
        let backup = inner.shared.join("install-backup");
        std::fs::create_dir_all(&backup)?;
        let mut paths = vec![
            target.to_owned(),
            inner.home.join(".local/bin/locked-build"),
            inner.home.join(".config/locked-build/policy.json"),
            inner.shared.join("policy.json"),
            inner.shared.join("service-binary.json"),
            inner.shared.join("install-committed.json"),
            inner.shared.join("host-namespace.json"),
            inner.shared.join("policy-ledger.json"),
            inner.home.join(".ix/ix-build/install.json"),
            inner.home.join(".ix/ix-build/policy.json"),
            inner.home.join(".config/ix-build/policy.json"),
            inner.home.join(".ix/ix-build/state"),
            inner.home.join(".local/lib/ix-build/shims/cargo"),
            inner.home.join(".local/share/dev-tools/installed.json"),
        ];
        for profile in [".profile", ".bashrc", ".zshrc"] {
            paths.push(inner.home.join(profile));
        }
        for unit in [
            "locked-build-metrics.service",
            "locked-build-metrics.timer",
            "locked-build-prune.service",
            "locked-build-prune.timer",
            "ix-build-metrics.service",
            "ix-build-metrics.timer",
            "ix-build-prune.service",
            "ix-build-prune.timer",
        ] {
            paths.push(inner.home.join(".config/systemd/user").join(unit));
        }
        for plist in [
            "com.agent-ix.locked-build-metrics.plist",
            "com.agent-ix.locked-build-prune.plist",
            "com.agentix.locked-build-metrics.plist",
            "com.agent-ix.ix-build-metrics.plist",
            "com.agent-ix.ix-build-prune.plist",
        ] {
            paths.push(inner.home.join("Library/LaunchAgents").join(plist));
        }
        let mut journal = Journal {
            stage: "before_swap".into(),
            entries: Vec::new(),
            monitoring: Some(MonitoringSnapshot::capture(&inner.home)?),
        };
        for (index, path) in paths.into_iter().enumerate() {
            let mut entry = Entry {
                path,
                original: Original::Missing,
            };
            match std::fs::symlink_metadata(&entry.path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    entry.original = Original::Symlink {
                        target: std::fs::read_link(&entry.path)?,
                    }
                }
                Ok(meta) if meta.is_file() => {
                    let saved = backup.join(index.to_string());
                    std::fs::copy(&entry.path, &saved)?;
                    std::fs::File::open(&saved)?.sync_all()?;
                    entry.original = Original::File { saved };
                }
                Ok(meta) if meta.is_dir() => entry.original = Original::Directory,
                Ok(_) => {
                    return Err(Error::new(
                        ErrorCode::Configuration,
                        "unsupported install backup entry",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            journal.entries.push(entry);
        }
        std::fs::File::open(&backup)?.sync_all()?;
        std::fs::File::open(&inner.shared)?.sync_all()?;
        write_journal(&inner.shared, &journal)?;
        inner.journal = Some(journal);
        copy_file_if_changed(source, target)?;
        if let Some(parent) = target.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        if let Some(journal) = &mut inner.journal {
            journal.stage = "swapped".into();
            write_journal(&inner.shared, journal)?;
        }
        Ok(self.advance())
    }
}

impl Transaction<Swapped> {
    pub(super) fn verify(
        mut self,
        verify: impl FnOnce() -> Result<()>,
    ) -> Result<Transaction<Verified>> {
        verify()?;
        let inner = self.inner()?;
        if let Some(journal) = &mut inner.journal {
            for entry in &journal.entries {
                if let Ok(meta) = std::fs::metadata(&entry.path)
                    && meta.is_file()
                {
                    std::fs::File::open(&entry.path)?.sync_all()?;
                }
                if let Some(parent) = entry.path.parent()
                    && parent.exists()
                {
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
            journal.stage = "verified".into();
            write_journal(&inner.shared, journal)?;
        }
        Ok(self.advance())
    }
}

impl Transaction<Verified> {
    pub(super) fn resume(
        mut self,
        successor: impl FnOnce(std::fs::File, &str) -> Result<Option<std::process::Child>>,
    ) -> Result<()> {
        let inner = self.inner()?;
        let token = inner
            .hold
            .as_ref()
            .and_then(|hold| hold.durable.as_ref())
            .map(|(_, id)| id.clone())
            .ok_or_else(|| Error::new(ErrorCode::State, "install commit token missing"))?;
        let mut pending = PendingChild(None);
        if let Some(election) = inner.hold.as_ref().and_then(|hold| hold.election.as_ref()) {
            pending.0 = successor(election.try_clone()?, &token)?;
        }
        crate::store::Store {
            path: inner.shared.clone(),
            config_path: inner.shared.join("policy.json"),
        }
        .write_atomic("install-committed.json", &serde_json::to_vec(&token)?)?;
        if let Some(journal) = &mut inner.journal {
            journal.stage = "committed".into();
            write_journal(&inner.shared, journal)?;
        }
        std::fs::remove_file(inner.shared.join("install-transaction.json"))?;
        std::fs::File::open(&inner.shared)?.sync_all()?;
        if let Some(hold) = inner.hold.as_mut() {
            hold.open()?;
        }
        inner.journal = None;
        drop(inner.hold.take());
        if let Some(child) = pending.0.take() {
            crate::scheduler::detach(child);
        }
        Ok(())
    }
}

struct PendingChild(Option<std::process::Child>);
impl Drop for PendingChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl<S> Drop for Transaction<S> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.as_mut()
            && let Some(mut journal) = inner.journal.take()
        {
            let store = crate::store::Store {
                path: inner.shared.clone(),
                config_path: inner.shared.join("policy.json"),
            };
            let rollback_intent = (|| -> Result<()> {
                if store.queue_hold()?.is_none() {
                    let hold = crate::queue::pause_locked(&store, "install_rollback", 900_000)?;
                    if let Some(guard) = inner.hold.as_mut() {
                        guard.durable = Some((inner.shared.clone(), hold.id));
                    }
                }
                journal.stage = "rolling_back".into();
                write_journal(&inner.shared, &journal)
            })();
            if let Err(error) = rollback_intent {
                if let Some(hold) = inner.hold.as_mut() {
                    hold.durable = None;
                }
                eprintln!(
                    "install rollback intent could not be persisted: {error}; recovery required"
                );
                return;
            }
            if let Err(error) = restore(&journal) {
                if let Some(hold) = inner.hold.as_mut() {
                    // Restore failure cannot safely expose a mixed install.
                    // Keep the persisted hold for explicit repaired recovery.
                    hold.durable = None;
                }
                eprintln!(
                    "install rollback failed: {error}; backup retained at {}",
                    inner.shared.display()
                );
            } else if let Err(error) =
                std::fs::remove_file(inner.shared.join("install-transaction.json"))
            {
                eprintln!("install rollback journal cleanup failed: {error}");
            }
        }
        // Inner's hold drops after rollback, so no grant can race restoration.
    }
}

fn write_journal(shared: &Path, journal: &Journal) -> Result<()> {
    crate::store::Store {
        path: shared.to_owned(),
        config_path: shared.join("policy.json"),
    }
    .write_atomic("install-transaction.json", &serde_json::to_vec(journal)?)
}

fn restore(journal: &Journal) -> Result<()> {
    if !matches!(
        journal.stage.as_str(),
        "before_swap" | "swapped" | "verified" | "committed" | "rolling_back"
    ) {
        return Err(Error::new(
            ErrorCode::State,
            "unknown install transaction stage; refusing recovery",
        ));
    }
    if let Some(monitoring) = &journal.monitoring {
        monitoring.stop()?;
    }
    for entry in journal.entries.iter().rev() {
        if matches!(entry.original, Original::Directory) {
            continue;
        }
        if let Original::File { saved } = &entry.original {
            let next = entry.path.with_extension("rollback.next");
            std::fs::copy(saved, &next)?;
            std::fs::File::open(&next)?.sync_all()?;
            std::fs::rename(next, &entry.path)?;
        } else {
            match std::fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if let Original::Symlink { target } = &entry.original {
                symlink(target, &entry.path)?;
            }
        }
        if let Some(parent) = entry.path.parent()
            && parent.exists()
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
    }
    if let Some(monitoring) = &journal.monitoring {
        monitoring.resume()?;
    }
    Ok(())
}

pub(super) fn recover(store: &crate::store::Store) -> Result<()> {
    let path = store.path.join("install-transaction.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let _state = store.try_lock_member("lock")?.ok_or_else(|| {
                Error::new(
                    ErrorCode::Transition,
                    "state writer is busy; retry queue resume recovery",
                )
            })?;
            let journal: Journal = serde_json::from_slice(&bytes)?;
            if !matches!(
                journal.stage.as_str(),
                "before_swap" | "swapped" | "verified" | "committed" | "rolling_back"
            ) {
                return Err(Error::new(
                    ErrorCode::State,
                    "unknown install transaction stage; refusing recovery",
                ));
            }
            if journal.stage != "committed" {
                restore(&journal)?;
            }
            std::fs::remove_file(path)?;
            std::fs::File::open(&store.path)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trace: FR-011-AC-7, FR-015-AC-10. Provenance: SWM-431.
    #[test]
    fn monitoring_rollback_restarts_only_previously_active_producers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let old = MonitoringSnapshot::units()[0];
        let new = MonitoringSnapshot::units()[MonitoringSnapshot::units().len() - 2];
        for unit in [old, new] {
            let path = MonitoringSnapshot::file(root.path(), unit);
            std::fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| Error::new(ErrorCode::State, "unit parent"))?,
            )?;
            std::fs::write(path, b"unit")?;
        }
        let mut calls = Vec::new();
        let new_running = std::cell::Cell::new(false);
        let mut control = |args: &[std::ffi::OsString]| -> Result<bool> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let query = args
                .iter()
                .any(|arg| matches!(arg.as_str(), "print" | "is-active" | "is-enabled"));
            let old_active = args.iter().any(|arg| arg.ends_with(old))
                || (new_running.get() && args.iter().any(|arg| arg.ends_with(new)));
            calls.push(args);
            Ok(!query || old_active)
        };
        let snapshot = MonitoringSnapshot::capture_with(root.path(), &mut control)?;
        assert_eq!(snapshot.active, [old]);
        let bytes = serde_json::to_vec(&snapshot)?;
        let recovered: MonitoringSnapshot = serde_json::from_slice(&bytes)?;
        new_running.set(true);
        recovered.stop_with(&mut control)?;
        recovered.resume_with(&mut control)?;
        if cfg!(target_os = "macos") {
            assert!(
                calls
                    .iter()
                    .any(|args| args[0] == "bootout" && args[1].ends_with(old))
            );
            assert!(
                calls
                    .iter()
                    .any(|args| args[0] == "bootout" && args[1].ends_with(new))
            );
            let starts: Vec<_> = calls.iter().filter(|args| args[0] == "bootstrap").collect();
            assert_eq!(starts.len(), 1);
            assert!(starts[0][2].ends_with(&format!("{old}.plist")));
        } else {
            assert!(
                calls
                    .iter()
                    .any(|args| args.iter().any(|arg| arg == "disable")
                        && args.iter().any(|arg| arg == new))
            );
            let starts: Vec<_> = calls.iter().filter(|args| args[1] == "start").collect();
            assert_eq!(starts.len(), 1);
            assert_eq!(starts[0][2], old);
        }
        let mut fail = |_: &[std::ffi::OsString]| Ok(false);
        assert!(recovered.resume_with(&mut fail).is_err());
        Ok(())
    }

    #[test]
    fn install_transaction_crash_at_each_step_preserves_hold_and_recovers_backup() -> Result<()> {
        for stage in ["check", "hold", "idle", "swap", "verify", "rollback"] {
            let root = tempfile::tempdir()?;
            let shared = root.path().join("shared");
            let home = root.path().join("home");
            std::fs::create_dir_all(&shared)?;
            std::fs::create_dir_all(home.join(".local/bin"))?;
            std::fs::write(home.join(".local/bin/ix-build"), b"old")?;
            std::fs::write(root.path().join("artifact"), b"new")?;
            let status = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "install::transaction::tests::install_transaction_crash_child",
                    "--ignored",
                    "--test-threads=4",
                ])
                .env("IX_INSTALL_CRASH_ROOT", root.path())
                .env("IX_INSTALL_CRASH_STAGE", stage)
                .stdout(std::process::Stdio::null())
                .status()?;
            assert_eq!(status.code(), Some(80), "crash fixture {stage}");
            let store = crate::store::Store {
                config_path: shared.join("policy.json"),
                path: shared.clone(),
            };
            if stage == "check" {
                assert!(store.queue_hold()?.is_none());
                continue;
            }
            assert!(store.queue_hold()?.is_some(), "{stage} lost durable hold");
            assert!(store.try_admission_lock()?.is_none());
            if matches!(stage, "swap" | "verify") {
                assert_eq!(std::fs::read(home.join(".local/bin/ix-build"))?, b"new");
            }
            crate::queue::resume(&store)?;
            assert!(store.try_admission_lock()?.is_some());
            assert_eq!(
                std::fs::read(home.join(".local/bin/ix-build"))?,
                b"old",
                "{stage} failed rollback"
            );
            assert!(!shared.join("install-transaction.json").exists());
        }
        Ok(())
    }

    #[test]
    #[ignore = "owned child exits between typed install stages without running destructors"]
    fn install_transaction_crash_child() -> Result<()> {
        let root = PathBuf::from(
            std::env::var_os("IX_INSTALL_CRASH_ROOT")
                .ok_or_else(|| Error::new(ErrorCode::State, "fixture root missing"))?,
        );
        let stage = std::env::var("IX_INSTALL_CRASH_STAGE")
            .map_err(|e| Error::new(ErrorCode::State, e.to_string()))?;
        let checked = Checked::new(&root.join("home"), &root.join("shared"))?;
        if stage == "check" {
            std::process::exit(80);
        }
        let held = checked.hold(Some(Duration::from_secs(1)))?;
        if stage == "hold" {
            std::process::exit(80);
        }
        let idle = held.wait_idle()?;
        if stage == "idle" {
            std::process::exit(80);
        }
        let swapped = idle.swap(
            &root.join("artifact"),
            &root.join("home/.local/bin/ix-build"),
        )?;
        if stage == "swap" {
            std::process::exit(80);
        }
        let mut verified = swapped.verify(|| Ok(()))?;
        if stage == "rollback" {
            let inner = verified.inner()?;
            let journal = inner
                .journal
                .as_mut()
                .ok_or_else(|| Error::new(ErrorCode::State, "journal missing"))?;
            journal.stage = "rolling_back".into();
            write_journal(&inner.shared, journal)?;
            std::fs::write(root.join("home/.local/bin/ix-build"), b"partial rollback")?;
        }
        std::process::exit(80)
    }

    #[test]
    fn install_transaction_rejects_corrupt_recovery_metadata_without_mutating_files() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let store = crate::store::Store {
            config_path: root.path().join("policy.json"),
            path: root.path().to_owned(),
        };
        let target = root.path().join("binary");
        std::fs::write(&target, b"keep")?;
        for value in [
            serde_json::json!({"stage":"swapped","entries":[{"path":target,"save":"missing"}]}),
            serde_json::json!({"stage":"unknown","entries":[]}),
        ] {
            std::fs::write(
                store.path.join("install-transaction.json"),
                serde_json::to_vec(&value)?,
            )?;
            assert!(recover(&store).is_err());
            assert_eq!(std::fs::read(&target)?, b"keep");
        }
        Ok(())
    }
}
