---
id: SR-003
title: "Code review of locked-build metrics export (SWM-242)"
type: SpecReview
analysis: code-review
scope: "agent-ix/dev-tools; tools/locked-build/src/{main,metrics,model,runtime,store,watchdog,config,error}.rs, tools/locked-build/src/{model,runtime}/tests.rs, tools/locked-build/{README.md,reference.md}; PR dev-tools#25"
review_set: subset
---

## Summary

Ticket: SWM-242. Reviewed head 97ba21775eaaa6c794fb1acfa3c69fff3c7a450c (base 8b67c654269ddeceaee6d980a4c1901dce809d66) in the ticket worktree, code-review with the rust-review lane. Reference contract: dev-team FR-015, FR-010, FR-011 at dev-team PR 39 head ea0d2e6024900686658c5feb88b15fcbeb776b31, and ix-board FR-041. Counters are written in the same atomic state transaction as the transition they count, but three high defects are reproduced: an in-place upgrade kills running builds, the blocked-seconds counter runs on with no waiter, and a valid policy with priority above 2 wedges the host.

## Verdict

**FAIL**. Three high findings.

Gates at the reviewed head: `cargo fmt --check` exit 0; `cargo clippy --locked --all-targets --all-features -- -D warnings` exit 0; `cargo test --locked` exit 0 (31 passed, 2 ignored). No deny.toml exists, so cargo deny was not run. The PR has no CI checks and changes no workflow. promtool was unavailable.

Verified by reproduction on draco with scratch state directories: three concurrent waiters blocked for about 5 s exported 4.909 blocked seconds (counted once); node_exporter 1.13.1 textfile collector accepted the document (node_textfile_scrape_error 0, 88 series in and out); terminal counts survive prune; the finish terminal guard prevents a second count. No vendoring, duplication or unrequested file-tracking machinery was found. No AssuranceProfile exists in this repository.

Not findings by dispatch decision: no governor, pause or abort; pause series absent; hold deadline unchanged; abort outcomes never increment.

## Findings

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-001 | high | Adding `metrics` to the deny_unknown_fields State at schema_version 1 makes every old-binary process fail on the first new-binary write; old watchdogs then terminate their running builds. Reproduced: an old `sleep 40` build was killed after one new-binary `run`, recorded failed/supervisor_lost; the old binary cannot read the state afterwards. | tools/locked-build/src/model.rs:109-110 |
| FND-002 | high | An open admission-blocked interval is closed only by a later admit poll. After the last waiter is cancelled, interrupted or dies, the exported counter keeps growing with an empty queue and the idle time is later persisted. Reproduced: 4.5 s blocked, cancel, 20 s idle, exported 24.830 and then persisted 24893 ms. | tools/locked-build/src/runtime.rs:528-529; tools/locked-build/src/metrics.rs:272-277 |
| FND-003 | high | Metrics accept only priorities 0..2 while policy allows any maximum_priority. A priority 3 request cannot be admitted, cancelled or finished because the metric error aborts the transaction, and it heads the queue so every other request waits behind it. Reproduced. | tools/locked-build/src/metrics.rs:104-114; tools/locked-build/src/model.rs:358; tools/locked-build/src/runtime.rs:544 |
| FND-004 | medium | active_block stores a monotonic clock value and the reboot path never resets it. After reboot the exported counter drops by the live part (Prometheus reads a counter reset and re-adds the total), and once new uptime passes the old start, unrelated uptime is counted as blocked. | tools/locked-build/src/runtime.rs:222-296; tools/locked-build/src/metrics.rs:92 |
| FND-005 | medium | TYPE lines are written as Gauge, Counter and Histogram. The text format allows lowercase only; the Prometheus server parser rejects the document with invalid metric type. node_exporter's textfile parser tolerates it, stdout or direct serving does not. | tools/locked-build/src/metrics.rs:187,206,230,240,250,268 |
| FND-006 | medium | The textfile mode 0644 is masked by the process umask. Under umask 077 the file is 0600 and a node_exporter running as another user cannot read it, so all series vanish. Reproduced. | tools/locked-build/src/metrics.rs:322-326 |
| FND-007 | medium | reason="operator" is exported but can never increase: nothing produces the operator reason, and the only operator hold that exists (capacity_units 0) yields capacity_shrunk, which is not counted. A held host reports 0 operator-blocked seconds. | tools/locked-build/src/metrics.rs:160,271; tools/locked-build/src/runtime.rs:390-397 |
| FND-008 | low | `metrics` against a mistyped or missing --state path creates a new store and exports a healthy all-zero host instead of failing. Reproduced. | tools/locked-build/src/runtime.rs:367-372 |
| FND-009 | low | write_atomic never removes its temporary file when write, sync or rename fails; each failing periodic export leaves another hidden file in the collector directory. | tools/locked-build/src/metrics.rs:315-329 |
| FND-010 | low | The snapshot is taken under the state lock but written after it is released. Of two overlapping exports the older can rename last, so counters step backwards and Prometheus counts a reset. | tools/locked-build/src/main.rs:134-141 |
| FND-011 | low | Metrics persist as fixed positional arrays with no per-field default. Adding an outcome, bucket or blocked reason (FR-015 already names swap and compressor) makes existing state fail to load. | tools/locked-build/src/metrics.rs:84-101 |
| FND-012 | low | current_block_reason calls admission_reason, which is O(n) twice, for every queued request on every waiter poll under the exclusive lock, although only State::next can yield a resource reason. | tools/locked-build/src/runtime.rs:374-401 |
| FND-013 | low | Queue wait is measured from enqueued_clock_ms, which reboot recovery resets, so a request that waited across a reboot is observed with only its post-recovery wait. | tools/locked-build/src/runtime.rs:235,541 |
| FND-014 | low | The outcome label is derived from free-text reason strings with a catch-all, and non-terminal phases map to failed; a misspelled or new termination reason is silently counted as another valid outcome (rust-style closed-enum rule). | tools/locked-build/src/metrics.rs:62-79 |

## Examined Scope

- examined: tools/locked-build/src/metrics.rs, main.rs, model.rs (State, finish, admission_reason), runtime.rs (recover, status, metrics, current_block_reason, cancel, admit_worker, supervise, prune), store.rs, watchdog.rs (run, observe), README.md and reference.md metric sections, test changes.
- context_only: config.rs, error.rs, policy.example.json, dev-team FR-005/009/010/011/015, ix-board FR-041.

## Rejected candidates

- Capitalized TYPE breaking node_exporter: rejected, node_exporter 1.13.1 accepts it (the direct-parser defect remains as FND-005).
- Blocked seconds counted per waiter: rejected by trace and reproduction.
- A counter incremented twice by recover or a second finish: rejected, finish returns early for terminal requests and counter and phase persist together.

## Dispositions

Round 2, head f4c1fab8eac28c9a7c5c20702c20adef427e43e3. Supersedes the round 1 comment another reviewer posted at a32a08f.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-001 | fixed | Reproduced: an old-binary build ran to success beside new-binary use of the same state, and the old binary still reads the state. |
| FND-002 | still-open | Cancel and empty-queue paths are fixed. A queued request whose supervisor and client are dead still accrues: 6.433 to 21.496 s over 15 s with no worker process and a policy that no longer blocks. |
| FND-003 | fixed | Priority 3 admitted and finished, priority 4 cancelled, lower priority work ran. |
| FND-004 | fixed | Boot mismatch resets the open interval; a simulated reboot left the exported value unchanged. |
| FND-005 | fixed | TYPE tokens are lowercase; node_exporter 1.13.1 accepts all 114 series. |
| FND-006 | fixed | Under umask 077 the textfile is 0644. |
| FND-007 | fixed | The operator series is no longer exported. |
| FND-008 | fixed | A missing state path exits 2 and is not created. |
| FND-009 | fixed | The temporary file is removed on failure; test present. |
| FND-010 | fixed | Snapshot and replacement run under the state lock (consequence recorded as FND-016). |
| FND-011 | fixed | Named defaulted maps load older data (the reverse direction is FND-015). |
| FND-012 | fixed | Only State::next is evaluated (the way it is evaluated is FND-017). |
| FND-013 | fixed | Queue wait uses persisted UTC timestamps (consequence recorded as FND-018). |
| FND-014 | fixed | Outcome derives from the typed event phase. |
| FND-015 | still-open | Found in round 2; no fix yet. |
| FND-016 | still-open | Found in round 2; no fix yet. |
| FND-017 | still-open | Found in round 2; no fix yet. |
| FND-018 | still-open | Found in round 2; no fix yet. |
| FND-019 | still-open | Found in round 2; no fix yet. |
| FND-020 | still-open | Found in round 2; no fix yet. |
| FND-021 | still-open | Found in round 2; no fix yet. |
| FND-002 | fixed | Round 3, head 838f5bc: a dead supervisor and client no longer accrue (5.124 then 5.123 s over 15 s; the 1 ms drop is FND-024). |
| FND-015 | fixed | Round 3: a live build survived an unknown reason key and a corrupt sidecar; a failed sidecar write after the state commit only logs. |
| FND-016 | fixed | Round 3: the textfile is written under a separate export lock after the state lock is released. |
| FND-017 | fixed | Round 3: admission_reason, clock_ms and validate_state_directory are reused (a smaller new copy is FND-025). |
| FND-018 | fixed | Round 3: event wait_ms is monotonic again; the reboot addition is documented. |
| FND-019 | fixed | Round 3: the sidecar is rebound to a recreated store (reproduced). |
| FND-020 | fixed | Round 3: an open interval is no longer checkpointed on every transaction. |
| FND-021 | fixed | Round 3: the other label is gone; priorities above 2 are counted as gate, documented in reference.md. |
| FND-022 | still-open | Found in round 3; no fix yet. |
| FND-023 | still-open | Found in round 3; no fix yet. |
| FND-024 | still-open | Found in round 3; no fix yet. |
| FND-025 | still-open | Found in round 3; no fix yet. |
| FND-026 | still-open | Found in round 3 at head bf7b218; no fix yet. |
| FND-027 | still-open | Found in round 3 at head bf7b218; no fix yet. |
| FND-022 | fixed | Round 4, head 88ea2d1: the interval is a host-level accumulator; cancel and interrupt of the head with another waiter no longer drop it (8.726 then 9.002 s; 14.153 then 15.258 s). |
| FND-023 | fixed | Round 4: a stale heartbeat no longer expires an interval while a queued worker is alive; 8 waiters counted 23.7 of 23.7 s, 16 waiters 35.7 of 38.6 s between scrapes lasting about 1.9 s each. |
| FND-024 | fixed | Round 4: expiry is persisted; 80 scrapes after all waiters died showed no decrease and active_block was cleared. |
| FND-025 | fixed | Round 4: from_label is removed; the persisted reason is the enum. |
| FND-026 | fixed | Round 4: both documents give the full Terra file path and state that --output has no default. |
| FND-027 | fixed | Round 4: the user directory is a placeholder and no tracked file holds the personal path. |
| FND-028 | still-open | Found in round 4; no fix yet. |
| FND-029 | still-open | Found in round 4; no fix yet. |
| FND-030 | still-open | Found in round 4; no fix yet. |
| FND-028 | fixed | Round 5, head cc0a563: the end-of-transaction reclassification is removed. Metrics median 20 ms at 1 waiter and 325 ms at 8 (bf7b218: 13 and 299; 88ea2d1: 62-63 and 767-890); cancel at 8 waiters 374 ms. |
| FND-029 | fixed | Round 5: exposition checkpoints both start stamps first. Stalled then killed waiters read 11.723 s before and after; the head-cancel case went 9.007 to 9.090 s; the capped reboot case read 5.500 s; a 120-scrape churn run showed no decrease. The new test fails when the checkpoint call is removed. |
| FND-030 | fixed | Round 5: the note is deleted and quire validate of reviews/**/*.md exits 0. |

## New findings (disposition pass 2)

Fix diff 97ba217..f4c1fab only. Gates at the new head: fmt check, clippy with warnings denied, and locked tests exit 0 (44 passed, 2 ignored).

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-015 | medium | Every transaction now fails if metrics.json cannot be read. A reason key unknown to the running binary made a live build be killed (reproduced); a failed sidecar write after state.json is committed also returns an error for a transition that already happened. | tools/locked-build/src/store.rs:122-136,182-188; tools/locked-build/src/metrics.rs:145,165 |
| FND-016 | medium | The textfile write, with file and directory fsync, now runs while holding the scheduler lock, so a slow collector disk stalls admission and supervision. FR-015 says reading metrics never blocks admission longer than the state-read time. | tools/locked-build/src/runtime.rs:374-380 |
| FND-017 | high | The fix adds second copies of existing rules: current_block_reason restates admission_reason's capacity and sample checks and repeats the reason list of from_admission_reason, store.rs re-implements runtime::clock_ms with silent fallbacks, and open_existing copies the state-directory check. High under the code-review duplication rule. | tools/locked-build/src/runtime.rs:395-423; tools/locked-build/src/store.rs:45-58,153-157 |
| FND-018 | low | Event wait_ms is now wall-clock difference while reference.md still says durations use a monotonic clock and status wait_ms remains monotonic; a clock step distorts the histogram and events. | tools/locked-build/src/model.rs:246-249; tools/locked-build/reference.md:57 |
| FND-019 | low | metrics.json is not tied to store_id. If state.json is recreated beside it, new events are ignored until their sequence passes the old one, so counters freeze silently. Reproduced. | tools/locked-build/src/metrics.rs:156-193 |
| FND-020 | low | While a blocked interval is open every transaction changes metrics.json, so status, recover and wait polls now rewrite and fsync both files, defeating the unchanged-state skip. | tools/locked-build/src/store.rs:158-177 |
| FND-021 | low | priority="other" is a label value that FR-015 and FR-011 do not define, and its zero series are exported on every host. | tools/locked-build/src/metrics.rs:25-47 |

## New findings (disposition pass 3)

Fix diffs f4c1fab..bf7b218 only (bf7b218 changes README.md and reference.md; code is identical to 838f5bc), rated by real impact. Gates at 838f5bc and bf7b218: fmt check, clippy with warnings denied, and locked tests exit 0 (54 passed, 2 ignored). Requirement tags were not checked: dev-tools has no specs and no trace library.

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-022 | high | Cancelling the selected blocked request while another is queued discards the whole open interval: expiry finds no queued heartbeat and clears it without committing. Reproduced: exported 8.608 s, then 0.000 right after the cancel. Interrupt, recover and hold expiry of that request take the same path by code reading. Prometheus reads a drop as a counter reset and adds the remaining total to the increase again. | tools/locked-build/src/metrics.rs:388-393; tools/locked-build/src/store.rs:145-153 |
| FND-023 | medium | The interval expires when the selected waiter has not polled for two poll_ms, which live waiters exceed under lock contention, and the gap is dropped. Measured on draco with memory blocked: 1 and 3 waiters counted fully, 8 counted 17.0 of 20.3 s and 18.0 of 20.2 s, 16 counted 7.5 of 21.0 s and 10.0 of 20.7 s (second run summed over all reasons). | tools/locked-build/src/metrics.rs:394-404,552-557 |
| FND-024 | medium | Expiry applied at load is not seen as a metrics change, so it is not persisted and each scrape recomputes it from the wall and monotonic clocks. With a dead selected waiter, 6 of 60 scrapes went down by 1 ms and blocked_millis stayed empty; Prometheus treats each drop as a counter reset. | tools/locked-build/src/store.rs:119-137,160; tools/locked-build/src/metrics.rs:394-401 |
| FND-025 | low | BlockReason::from_label restates the label strings of label() in reverse, a second copy the compiler does not keep in step. | tools/locked-build/src/metrics.rs:120-147 |
| FND-026 | low | The Terra WSL text gives only a directory and no file name: passing it as --output fails with Is a directory, and a file named other than ix_build_metrics.prom is not matched by the staleness alert of ix-local-observability PR 33. README also calls this a default output path, but --output has no default and metrics prints to stdout without it. | tools/locked-build/README.md:52-54; tools/locked-build/reference.md:101-103 |
| FND-027 | low | The commit puts a personal Windows user path into README.md and reference.md of this public repository; a placeholder for the user directory would carry the same instruction. | tools/locked-build/README.md:53; tools/locked-build/reference.md:103 |

## New findings (disposition pass 4)

Fix diff bf7b218..88ea2d1 only, rated by real impact. Gates at 88ea2d1: fmt check, clippy with warnings denied, and locked tests exit 0 (58 passed, 2 ignored). A 120-scrape run with cancels, reason switches and worker kills showed no counter decrease, and the capped reboot case read the expected 5.500 s. Requirement tags were not checked: no specs or trace library.

| ID | Severity | Summary | Refs |
|---|---|---|---|
| FND-028 | medium | Every transaction with an open interval, including metrics and status reads, now takes a full host sample under the state lock. Against bf7b218 with real sampling: 1 waiter, metrics 11-13 to 62-63 ms and client CPU 11-24 to 46-54 percent; 8 waiters, metrics 219-267 to 767-890 ms and cancel 233-256 to 812-1405 ms; 16 waiters, cancel 0.85 to 1.9 s. FR-015 says a metrics read takes no admission decision. | tools/locked-build/src/store.rs:171-175 |
| FND-029 | low | Exposition counts the open interval up to now, but expiry and the reboot cap can commit less. Reproduced: waiters stopped, scraped 12.010 s, killed, next scrape 4.296 s. With the head cancelled while the only other queued request has no heartbeat yet (state edited to that shape), 9.001 fell to 0.500 s. Both need an unusual timing. | tools/locked-build/src/metrics.rs:358-381,512-519 |
| FND-030 | low | The added note is not an artifact and makes quire validate of reviews/**/*.md fail. | reviews/swm-242-round3-fix-dispositions.md:1 |
