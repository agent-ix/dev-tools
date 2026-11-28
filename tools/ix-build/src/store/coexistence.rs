// SPDX-License-Identifier: MIT
//! Legacy state remains authoritative; identity-bound new metadata survives old rewrites.
use super::*;
use crate::model::{Event, Request};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MARKER: &str = "coexistence.json";
pub(super) const METRICS: &str = "metrics-v2.json";
const EXTENSIONS: &str = "state-v2.json";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    pub schema_version: u32,
    pub store_id: String,
    pub baseline_event_sequence: u64,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Extensions {
    schema_version: u32,
    store_id: String,
    requests: BTreeMap<String, Vec<Request>>,
    events: BTreeMap<String, Vec<Event>>,
}
fn same_request(left: &Request, right: &Request) -> bool {
    left.id == right.id
        && left.sequence == right.sequence
        && left.enqueued_at_ms == right.enqueued_at_ms
        && left.enqueued_clock_ms == right.enqueued_clock_ms
        && left.owner == right.owner
        && left.ticket == right.ticket
        && left.command == right.command
        && left.cwd == right.cwd
}
fn same_worker(left: &Request, right: &Request) -> bool {
    same_request(left, right)
        && left.worker_generation == right.worker_generation
        && left.worker.as_ref().map(|p| (p.pid, p.started_seconds))
            == right.worker.as_ref().map(|p| (p.pid, p.started_seconds))
}
fn same_outcome(left: &Request, right: &Request) -> bool {
    left.phase == right.phase
        && left.exit_code == right.exit_code
        && left.signal == right.signal
        && left.reason == right.reason
        && same_diagnostic(&left.trace.diagnostic, &right.trace.diagnostic)
}
fn same_event(left: &Event, right: &Event) -> bool {
    left.event_id == right.event_id
        && left.request_id == right.request_id
        && left.observed_at_ms == right.observed_at_ms
        && left.kind == right.kind
        && left.state == right.state
        && left.reason == right.reason
        && same_diagnostic(&left.trace.diagnostic, &right.trace.diagnostic)
}
fn same_diagnostic(left: &ix_build::wire::Diagnostic, right: &ix_build::wire::Diagnostic) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.code = left.code.map(ix_build::wire::Code::legacy_equivalent);
    right.code = right.code.map(ix_build::wire::Code::legacy_equivalent);
    left == right
}
impl Extensions {
    pub(super) fn load(store: &Store, state: &State) -> Result<Self> {
        let existing = store.read_member::<Self>(EXTENSIONS, STATE_LIMIT * 2)?;
        if let Some(marker) = store.read_member::<Marker>(MARKER, 4096)?
            && (marker.store_id != state.store_id || existing.is_none())
        {
            return Err(Error::new(
                ErrorCode::State,
                "coexistence metadata missing or belongs to another store",
            ));
        }
        let value = existing.unwrap_or_else(|| Self {
            schema_version: 1,
            store_id: state.store_id.clone(),
            ..Self::default()
        });
        if value.schema_version != 1 || value.store_id != state.store_id {
            return Err(Error::new(
                ErrorCode::State,
                "coexistence extension identity/version mismatch",
            ));
        }
        Ok(value)
    }
    pub(super) fn apply(&self, state: &mut State) {
        for request in &mut state.requests {
            let Some(versions) = self.requests.get(&request.id.0) else {
                continue;
            };
            if request.client.is_none()
                && let Some(saved) = versions
                    .iter()
                    .rev()
                    .find(|saved| same_request(request, saved) && saved.client.is_some())
            {
                request.client = saved.client.clone();
                request.client_pid_namespace = saved.client_pid_namespace.clone();
            }
            if request.telemetry.repo.is_empty()
                && let Some(saved) = versions
                    .iter()
                    .rev()
                    .find(|saved| same_request(request, saved) && !saved.telemetry.repo.is_empty())
            {
                request.telemetry = saved.telemetry.clone();
            }
            let Some(saved) = versions
                .iter()
                .rev()
                .find(|saved| same_worker(request, saved))
            else {
                continue;
            };
            if let (Some(current), Some(previous)) =
                (&mut request.lease_record, &saved.lease_record)
                && current.command.as_ref().map(|p| (p.pid, p.started_seconds))
                    == previous
                        .command
                        .as_ref()
                        .map(|p| (p.pid, p.started_seconds))
            {
                if current.cache_wrapper.is_none() {
                    current.cache_wrapper = previous.cache_wrapper.clone();
                }
                if current.cache_wrapper_status == crate::model::CacheWrapperStatus::Unrecorded {
                    current.cache_wrapper_status = previous.cache_wrapper_status;
                }
                if current.memory_scope.is_none() {
                    current.memory_scope = previous.memory_scope.clone();
                }
                current.scope_oom_killed |= previous.scope_oom_killed;
            }
            if let Some(exact) = self.requests.get(&request.id.0).and_then(|versions| {
                versions.iter().rev().find(|candidate| {
                    same_worker(request, candidate) && same_outcome(request, candidate)
                })
            }) {
                request.trace.diagnostic = exact.trace.diagnostic.clone();
            }
        }
        for event in &mut state.events {
            let Some(saved) = self
                .events
                .get(&event.event_id)
                .and_then(|versions| versions.iter().rev().find(|saved| same_event(event, saved)))
            else {
                continue;
            };
            if event.request_id == saved.request_id
                && event.observed_at_ms == saved.observed_at_ms
                && event.kind == saved.kind
                && event.state == saved.state
                && event.reason == saved.reason
                && same_diagnostic(&event.trace.diagnostic, &saved.trace.diagnostic)
            {
                if event.telemetry.repo.is_empty() {
                    event.telemetry = saved.telemetry.clone();
                }
                event.trace.diagnostic = saved.trace.diagnostic.clone();
            }
        }
    }
    pub(super) fn retain(&mut self, state: &State) {
        let requests: BTreeSet<_> = state.requests.iter().map(|r| &r.id.0).collect();
        self.requests.retain(|id, versions| {
            versions.retain(|saved| {
                state
                    .requests
                    .iter()
                    .any(|current| same_worker(current, saved) && same_outcome(current, saved))
            });
            requests.contains(id) && !versions.is_empty()
        });
        let events: BTreeSet<_> = state.events.iter().map(|e| &e.event_id).collect();
        self.events.retain(|id, versions| {
            versions.retain(|saved| {
                state
                    .events
                    .iter()
                    .any(|current| same_event(current, saved))
            });
            events.contains(id) && !versions.is_empty()
        });
    }
    pub(super) fn capture(&mut self, state: &State) {
        for request in &state.requests {
            let versions = self.requests.entry(request.id.0.clone()).or_default();
            if let Some(saved) = versions
                .iter_mut()
                .find(|saved| same_worker(request, saved) && same_outcome(request, saved))
            {
                *saved = request.clone();
            } else {
                versions.push(request.clone());
            }
        }
        for event in &state.events {
            let versions = self.events.entry(event.event_id.clone()).or_default();
            if let Some(saved) = versions.iter_mut().find(|saved| same_event(event, saved)) {
                *saved = event.clone();
            } else {
                versions.push(event.clone());
            }
        }
    }
    pub(super) fn save(&self, store: &Store) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > STATE_LIMIT * 2 {
            return Err(Error::new(
                ErrorCode::State,
                "coexistence extension limit reached",
            ));
        }
        store.write_atomic(EXTENSIONS, &bytes)
    }
}

impl Store {
    pub(crate) fn coexisting(&self) -> Result<bool> {
        let Some(marker) = self.read_member::<Marker>(MARKER, 4096)? else {
            return Ok(false);
        };
        if marker.schema_version != 1 {
            return Err(Error::new(
                ErrorCode::StoreVersion,
                "unsupported coexistence marker",
            ));
        }
        Ok(true)
    }
    pub(crate) fn begin_coexistence(&self) -> Result<()> {
        let state = self
            .read_member::<State>("state.json", STATE_LIMIT)?
            .ok_or_else(|| Error::new(ErrorCode::State, "legacy state missing"))?;
        if let Some(marker) = self.read_member::<Marker>(MARKER, 4096)? {
            if marker.schema_version != 1 || marker.store_id != state.store_id {
                return Err(Error::new(
                    ErrorCode::State,
                    "coexistence store identity mismatch",
                ));
            }
            Extensions::load(self, &state)?;
            self.metrics_file()?;
            return Ok(());
        }
        self.reject_link(METRICS_FILE)?;
        let metrics = match fs::read(self.path.join(METRICS_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
            Err(error) => return Err(error.into()),
        };
        let mut parsed: crate::metrics::MetricsState = serde_json::from_slice(&metrics)?;
        parsed.bind_store(&state.store_id);
        self.write_atomic(METRICS, &serde_json::to_vec(&parsed)?)?;
        let mut extra = Extensions::load(self, &state)?;
        extra.capture(&state);
        extra.save(self)?;
        self.write_atomic(
            MARKER,
            &serde_json::to_vec(&Marker {
                schema_version: 1,
                store_id: state.store_id,
                baseline_event_sequence: parsed.exported_sequence(),
            })?,
        )
    }
    pub(super) fn metrics_file(&self) -> Result<&'static str> {
        Ok(if self.coexisting()? {
            self.reject_link(METRICS)?;
            if !self.path.join(METRICS).try_exists()? {
                return Err(Error::new(
                    ErrorCode::State,
                    "coexistence metrics ledger missing; restore metrics-v2.json from backup",
                ));
            }
            METRICS
        } else {
            METRICS_FILE
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<(tempfile::TempDir, Store, crate::model::RequestId)> {
        let root = tempfile::tempdir()?;
        let policy = root.path().join("policy.json");
        fs::write(
            &policy,
            serde_json::to_vec(&crate::config::Config::default())?,
        )?;
        let store = Store::open(&root.path().join("state"), &policy)?;
        let id = crate::runtime::enqueue(
            &store,
            "coexistence",
            "SWM-436",
            1,
            0,
            None,
            &["true".into()],
        )?;
        store.begin_coexistence()?;
        Ok((root, store, id))
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn pending_generation_cannot_replace_committed_metadata_before_base_publication() -> Result<()>
    {
        let (_root, store, id) = fixture()?;
        let (mut base, _) = store.snapshot()?;
        let expected_client = base.find(&id)?.client.clone();
        assert!(expected_client.is_some());
        let mut extra = Extensions::load(&store, &base)?;
        extra.retain(&base);
        extra.capture(&base);
        let mut pending: State = serde_json::from_slice(&serde_json::to_vec(&base)?)?;
        pending.find_mut(&id)?.worker_generation += 1;
        pending.find_mut(&id)?.client = None;
        extra.capture(&pending);
        extra.save(&store)?;
        // Crash boundary: sidecar durable, canonical state still previous generation.
        base.find_mut(&id)?.client = None;
        Extensions::load(&store, &base)?.apply(&mut base);
        assert_eq!(base.find(&id)?.client, expected_client);
        assert_eq!(base.find(&id)?.worker_generation, 0);
        Extensions::load(&store, &pending)?.apply(&mut pending);
        assert_eq!(pending.find(&id)?.client, expected_client);
        // An old writer pruned the request: sidecars must never resurrect it.
        base.requests.clear();
        extra.apply(&mut base);
        assert!(base.requests.is_empty());
        Ok(())
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn legacy_projection_retains_new_lease_metadata_and_legacy_completion_wins() -> Result<()> {
        let (_root, store, id) = fixture()?;
        store.transaction(|state, _| {
            state.find_mut(&id)?.lease_record = Some(crate::model::LeaseRecord {
                cache_wrapper: Some("/fixture/kache".into()),
                cache_wrapper_status: crate::model::CacheWrapperStatus::Enabled,
                memory_scope: Some("fixture.scope".into()),
                scope_oom_killed: true,
                ..Default::default()
            });
            Ok(())
        })?;
        let raw = fs::read(store.path.join("state.json"))?;
        let value: serde_json::Value = serde_json::from_slice(&raw)?;
        assert_eq!(value["schema_version"], 1);
        assert!(value.get("min_reader_version").is_none());
        let lease = &value["requests"][0]["lease_record"];
        for key in [
            "cache_wrapper",
            "cache_wrapper_status",
            "memory_scope",
            "scope_oom_killed",
        ] {
            assert!(lease.get(key).is_none());
        }
        let (restored, _) = store.snapshot()?;
        let record = restored
            .find(&id)?
            .lease_record
            .as_ref()
            .ok_or_else(|| Error::new(ErrorCode::State, "missing lease"))?;
        assert_eq!(
            record.cache_wrapper_status,
            crate::model::CacheWrapperStatus::Enabled
        );
        assert_eq!(record.memory_scope.as_deref(), Some("fixture.scope"));
        assert!(record.scope_oom_killed);
        // Simulate the old writer's terminal update and loss of unknown fields.
        let mut legacy: State = serde_json::from_slice(&raw)?;
        let request = legacy.find_mut(&id)?;
        request.phase = crate::model::Phase::Failed;
        request.exit_code = Some(7);
        request.ended_at_ms = Some(crate::runtime::now_ms()?);
        request.ended_clock_ms = Some(crate::runtime::clock_ms()?);
        fs::write(
            store.path.join("state.json"),
            ix_build::wire::persistence::to_vec(&legacy)?,
        )?;
        let (restored, _) = store.snapshot()?;
        assert_eq!(restored.find(&id)?.exit_code, Some(7));
        assert!(
            restored
                .find(&id)?
                .lease_record
                .as_ref()
                .is_some_and(|r| r.scope_oom_killed)
        );
        Ok(())
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn legacy_history_is_preserved_and_missing_new_observations_are_explicit() -> Result<()> {
        let (_root, store, _) = fixture()?;
        let original = fs::read(store.path.join(METRICS_FILE))?;
        let mut state: State = serde_json::from_slice(&fs::read(store.path.join("state.json"))?)?;
        state.next_event += 10;
        state.pruned_events += 10;
        fs::write(
            store.path.join("state.json"),
            ix_build::wire::persistence::to_vec(&state)?,
        )?;
        store.transaction(|_, _| Ok(()))?;
        let once = fs::read(store.path.join(METRICS))?;
        assert!(String::from_utf8_lossy(&once).contains("legacy_pruned_history"));
        store.transaction(|_, _| Ok(()))?;
        assert_eq!(fs::read(store.path.join(METRICS))?, once);
        assert_eq!(fs::read(store.path.join(METRICS_FILE))?, original);
        Ok(())
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn precise_new_diagnostic_survives_legacy_rewrite_but_not_a_different_outcome() -> Result<()> {
        let (_root, store, id) = fixture()?;
        store.transaction(|state, config| {
            state.finish(
                &id,
                crate::model::Completion {
                    code: Some(1),
                    ..Default::default()
                },
                config,
                crate::runtime::now_ms()?,
                crate::runtime::clock_ms()?,
            )?;
            state.find_mut(&id)?.trace.diagnostic.code =
                Some(ix_build::wire::Code::TargetUnavailable);
            Ok(())
        })?;
        let mut legacy: State = serde_json::from_slice(&fs::read(store.path.join("state.json"))?)?;
        assert_eq!(
            legacy.find(&id)?.trace.diagnostic.code,
            Some(ix_build::wire::Code::StateUnavailable)
        );
        legacy.find_mut(&id)?.client = None;
        fs::write(
            store.path.join("state.json"),
            ix_build::wire::persistence::to_vec(&legacy)?,
        )?;
        assert_eq!(
            store.snapshot()?.0.find(&id)?.trace.diagnostic.code,
            Some(ix_build::wire::Code::TargetUnavailable)
        );
        legacy.find_mut(&id)?.exit_code = Some(7);
        fs::write(
            store.path.join("state.json"),
            ix_build::wire::persistence::to_vec(&legacy)?,
        )?;
        assert_eq!(
            store.snapshot()?.0.find(&id)?.trace.diagnostic.code,
            Some(ix_build::wire::Code::StateUnavailable)
        );
        Ok(())
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn legacy_worker_claim_preserves_request_identity_without_reusing_lease_metadata() -> Result<()>
    {
        let (_root, store, id) = fixture()?;
        store.transaction(|state, _| {
            let request = state.find_mut(&id)?;
            request.telemetry.repo = "stable-repo".into();
            request.lease_record = Some(crate::model::LeaseRecord {
                memory_scope: Some("stale-generation.scope".into()),
                ..Default::default()
            });
            Ok(())
        })?;
        let expected = store.snapshot()?.0.find(&id)?.client.clone();
        let mut legacy: State = serde_json::from_slice(&fs::read(store.path.join("state.json"))?)?;
        let request = legacy.find_mut(&id)?;
        request.worker_generation += 1;
        request.worker = Some(crate::process::Processes::self_identity()?);
        request.client = None;
        request.client_pid_namespace = None;
        request.telemetry = Default::default();
        fs::write(
            store.path.join("state.json"),
            ix_build::wire::persistence::to_vec(&legacy)?,
        )?;
        store.transaction(|state, _| {
            let request = state.find(&id)?;
            assert_eq!(request.client, expected);
            assert_eq!(request.telemetry.repo, "stable-repo");
            assert!(
                request
                    .lease_record
                    .as_ref()
                    .is_some_and(|r| r.memory_scope.is_none())
            );
            Ok(())
        })?;
        let (state, _) = store.snapshot()?;
        assert_eq!(state.find(&id)?.client, expected);
        assert_eq!(state.find(&id)?.telemetry.repo, "stable-repo");
        Ok(())
    }
}
