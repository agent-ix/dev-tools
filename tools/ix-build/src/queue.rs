// SPDX-License-Identifier: MIT
//! Durable operator admission holds, independent of request lifetime and order.
use crate::error::{Error, ErrorCode, Result};
use crate::store::Store;
use serde::{Deserialize, Serialize};

/// Persisted host admission state machine. Missing legacy state means Open.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mode {
    /// New leases may be admitted subject to policy.
    #[default]
    Open,
    /// New admissions are held without changing any request's order.
    Held {
        /// Durable pause details.
        hold: Hold,
    },
}

/// Current host-wide admission hold. No request is removed or cancelled.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    /// Unique hold identity, used to avoid releasing another operator's hold.
    pub id: String,
    /// UTC creation time, retained across process and host restarts.
    pub since_ms: u64,
    /// Duration after which the monitoring rule should alert.
    pub alert_after_ms: u64,
    /// Operator pause or installation drain.
    pub reason: String,
    /// Absolute UTC deadline for the overdue-hold alert.
    pub deadline_ms: u64,
}

pub(crate) fn pause(store: &Store, alert_after_ms: u64) -> Result<Hold> {
    let _guard = store.try_lock_member("admission.lock")?.ok_or_else(|| {
        Error::new(
            ErrorCode::Configuration,
            "admission transition is busy; retry queue pause",
        )
    })?;
    pause_locked(store, "operator", alert_after_ms)
}

pub(crate) fn pause_locked(store: &Store, kind: &str, alert_after_ms: u64) -> Result<Hold> {
    if store.queue_hold()?.is_some() {
        return Err(Error::new(
            ErrorCode::Transition,
            "queue is already paused; use queue resume before install drain",
        ));
    }
    let hold = Hold {
        id: crate::store::random_id()?,
        since_ms: crate::runtime::now_ms()?,
        alert_after_ms,
        reason: kind.to_owned(),
        deadline_ms: crate::runtime::now_ms()?.saturating_add(alert_after_ms),
    };
    store.write_atomic(
        "queue-hold.json",
        &serde_json::to_vec(&Mode::Held { hold: hold.clone() })?,
    )?;
    Ok(hold)
}

pub(crate) fn resume(store: &Store) -> Result<()> {
    let _guard = store.try_lock_member("admission.lock")?.ok_or_else(|| {
        Error::new(
            ErrorCode::Transition,
            "installation drain is active; queue resume cannot release its admission guard",
        )
    })?;
    crate::install::recover_transaction(store)?;
    resume_locked(store, None)
}

pub(crate) fn resume_locked(store: &Store, expected: Option<&str>) -> Result<()> {
    if let Some(hold) = store.queue_hold()? {
        if expected.is_some_and(|id| id != hold.id) {
            return Err(Error::new(
                ErrorCode::Transition,
                "admission hold identity changed; refusing to release it",
            ));
        }
        store.write_atomic("queue-hold.json", &serde_json::to_vec(&Mode::Open)?)?;
    } else {
        return Err(Error::new(
            ErrorCode::Transition,
            "queue resume requires Held admission mode",
        ));
    }
    Ok(())
}

pub(crate) fn append_metrics(text: &mut String, host: &str, hold: Option<&Hold>, now: u64) {
    let host = crate::metrics::escape_label(host);
    for (name, help, value) in [
        (
            "ix_build_queue_paused",
            "Whether admission is durably paused.",
            f64::from(hold.is_some()),
        ),
        (
            "ix_build_queue_hold_age_seconds",
            "Age of the durable admission hold in seconds.",
            hold.map_or(0.0, |h| now.saturating_sub(h.since_ms) as f64 / 1000.0),
        ),
        (
            "ix_build_queue_hold_alert_after_seconds",
            "Configured hold age alert threshold in seconds.",
            hold.map_or(0.0, |h| h.alert_after_ms as f64 / 1000.0),
        ),
    ] {
        text.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name}{{host=\"{host}\"}} {value}\n"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_pause_persists_across_reopen_and_resumes_without_touching_requests() -> Result<()> {
        let root = tempfile::tempdir()?;
        let policy = root.path().join("policy.json");
        std::fs::write(
            &policy,
            serde_json::to_vec(&crate::config::Config::default())?,
        )?;
        let store = Store::open(&root.path().join("state"), &policy)?;
        crate::runtime::enqueue(
            &store,
            "operator-fixture",
            "SWM-428",
            1,
            0,
            None,
            &["true".into()],
        )?;
        let before = std::fs::read(store.path.join("state.json"))?;
        let hold = pause(&store, 1000)?;
        assert_eq!(hold.reason, "operator");
        assert!(hold.deadline_ms >= hold.since_ms + 1000);
        drop(store);
        let store = Store::open_existing(&root.path().join("state"), &policy)?;
        assert_eq!(store.queue_hold()?.map(|hold| hold.id), Some(hold.id));
        assert!(store.try_admission_lock()?.is_none());
        assert!(crate::runtime::status(&store)?.queue_hold.is_some());
        resume(&store)?;
        assert!(store.try_admission_lock()?.is_some());
        assert_eq!(std::fs::read(store.path.join("state.json"))?, before);
        assert_eq!(
            std::fs::read_to_string(store.path.join("queue-hold.json"))?,
            "{\"state\":\"open\"}"
        );
        Ok(())
    }

    #[test]
    fn queue_mode_illegal_transitions_have_stable_errors_and_never_release_other_holds()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Store {
            path: root.path().to_owned(),
            config_path: root.path().join("policy.json"),
        };
        assert_eq!(
            resume(&store).err().map(|e| e.code),
            Some(ErrorCode::Transition)
        );
        let hold = pause(&store, 1000)?;
        assert_eq!(
            pause(&store, 2000).err().map(|e| e.code),
            Some(ErrorCode::Transition)
        );
        assert_eq!(
            resume_locked(&store, Some("wrong-owner"))
                .err()
                .map(|e| e.code),
            Some(ErrorCode::Transition)
        );
        assert_eq!(store.queue_hold()?.map(|h| h.id), Some(hold.id));
        resume(&store)?;
        assert_eq!(
            resume(&store).err().map(|e| e.code),
            Some(ErrorCode::Transition)
        );
        Ok(())
    }

    #[test]
    fn queue_hold_metrics_use_only_host_and_hand_written_values() {
        let mut text = String::new();
        append_metrics(
            &mut text,
            "fixture",
            Some(&Hold {
                id: "private".into(),
                since_ms: 1000,
                alert_after_ms: 2000,
                deadline_ms: 3000,
                reason: "operator".into(),
            }),
            4500,
        );
        assert!(text.contains("ix_build_queue_paused{host=\"fixture\"} 1\n"));
        assert!(text.contains("ix_build_queue_hold_age_seconds{host=\"fixture\"} 3.5\n"));
        assert!(text.contains("ix_build_queue_hold_alert_after_seconds{host=\"fixture\"} 2\n"));
        assert!(!text.contains("private"));
    }
}
