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
