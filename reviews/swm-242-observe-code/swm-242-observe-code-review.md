---
id: SR-005
title: "Code review of ix-build observe mode (SWM-242)"
type: SpecReview
analysis: code-review
scope: "agent-ix/dev-tools; tools/ix-build/src/{observe,main,runtime,store}.rs, Cargo.toml, README.md, reference.md, tests/observe.rs; PR dev-tools#27 at c4ef5039f28fa2004af3ec3730a878681717d42c"
review_set: subset
---

## Summary

Ticket: SWM-242. Reviewed head c4ef5039f28fa2004af3ec3730a878681717d42c in the ticket worktree, code-review with the rust-review lane, against dev-team FR-015 observe mode at origin/main (merge cfb92253551a108363591ba2813ada51a5309653). The binary changes builds it only claims to observe: exit status, signal dispositions, job control, fail-open execution and the 1000 ms bounds are all broken in reproduced cases.

## Verdict

**FAIL**. Nine high, nine medium and six low findings. Not mergeable.

Gates at the reviewed head on macOS (Darwin 25.4.0, rustc 1.98.1): cargo fmt --check exit 0; cargo clippy --locked --all-targets --all-features -- -D warnings exit 0; cargo test --locked exit 0 (44 unit, 29 observe, 15 supervision passed; 2 ignored). No deny.toml, so cargo deny was not run. The repository has no CI workflows. promtool was not run.

Reproduced with scratch state directories and the release binary, using small C probes for signal dispositions, mask, descriptors, wait status, and a DYLD interposer that stalls or fails the observe.json rename. Held without defect: inherited descriptors 7 and 8, signal mask, process group, environment, working directory, macOS nofile limit, signal-death passthrough when the observer itself receives no signal, group delivery at most twice, held-lock start delay of about 1.01 to 1.03 s, 30 and 100 long-running concurrent observers without lost records, 30 observers on a store with 10000 raw statuses, legacy admitted store without mode.json, disk full with stderr on a healthy sink. With 100 simultaneous near-instant commands about 20 percent of outcomes were dropped with a lock-timeout warning, which fail-open permits; their warning lines interleaved.

Linux paths were read only, not run (FND-007, FND-017). No AssuranceProfile exists in this repository. The requirement-tag rule does not apply: dev-tools has no spec directory and no trace library; tests use Trace comments.

## Findings

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-001 | high | The observer kills itself with any signal it received, even when the command handled it and exited normally: a command trapping TERM or INT and exiting 0 is reported as signal death. Because the first received signal wins, SIGWINCH before a SIGTERM death turns it into exit code 143. Reproduced. Fix: delete received_signal and mimic only the child's status.signal(). | tools/ix-build/src/observe.rs:357-368; tools/ix-build/src/observe.rs:314 |
| FND-002 | high | Handlers are installed before spawn, so signals the caller had ignored are reset to default in the command. Direct run ignored 1,2,3,13,15; observed command ignored none. nohup and shell background builds are killed by HUP or INT that a direct run survives. Reproduced. Fix: spawn before registering handlers so the child inherits dispositions, and preserve or explicitly specify inherited SIGPIPE ignore, which the Rust runtime and Command reset. | tools/ix-build/src/observe.rs:210-221; tools/ix-build/src/observe.rs:371-405 |
| FND-003 | high | SIGTSTP, SIGTTIN and SIGTTOU are caught, so on Ctrl-Z the command stops but the caller-started process keeps running and the shell never regains the terminal. After a stop and continue, the observer raises SIGSTOP on itself once the command has completed and stays stopped. Reproduced. Fix: do not register the three stop signals, or forward then stop self immediately, and never replay them after exit. | tools/ix-build/src/observe.rs:387-390; tools/ix-build/src/observe.rs:357-362 |
| FND-004 | high | Warnings use eprintln, which panics when stderr cannot be written. With a broken pipe or a log file on a full disk, a fail-open invocation exits 101 and the command never runs; after a successful command a failed completion write exits 101. Reproduced both. Fix: write each warning as one preformatted line with writeln on stderr and ignore the error; same for the spawn-failure line and main. | tools/ix-build/src/observe.rs:777-779; tools/ix-build/src/observe.rs:224 |
| FND-005 | high | The parse fallback consumes the -- delimiter as the value of a valueless --kind, --priority or --queued-at-ms, finds no command and exits 2. A wrapper with an unquoted unset queue variable (--queued-at-ms $Q -- cargo build) never runs the build. Reproduced. Fix: never take -- as an option value in either parsing loop. | tools/ix-build/src/observe.rs:101-131 |
| FND-006 | high | The 1000 ms bounds cover only lock waiting and one config read. State reads, writes, fsync and rename run on the main thread: a 3 s stall delayed command start 3.45 s; a 5 s stall after exit held exit and stdout 5.04 s; a 4 s stall after spawn delayed a PID-only SIGTERM by the full stall. Reproduced. Fix: run each observer phase on a worker thread with recv_timeout, abandon it with a cancel flag, and enter the wait and forwarding loop immediately after spawn. | tools/ix-build/src/observe.rs:408-462; tools/ix-build/src/observe.rs:464-508; tools/ix-build/src/observe.rs:236-271 |
| FND-007 | high | On Linux the preflight sysinfo process refresh calls setrlimit to raise the RLIMIT_NOFILE soft limit to the hard limit, and the command spawned afterwards inherits it, so observed builds run with a different resource limit than direct ones (for example 1024 becomes the hard limit). Read in sysinfo 0.37.2 remaining_files; not run on Linux. Fix: call sysinfo::set_open_files_limit(0) first and restore the saved limit with rustix before spawn, or read /proc/PID/stat directly. | tools/ix-build/src/observe.rs:436; tools/ix-build/src/observe.rs:731-740 |
| FND-008 | high | Observed queue waits are an unbounded Vec rewritten and fsynced on every operation. Past 32 MiB (about 2.4 million observations) every later observe warns and records nothing and metrics and status exit 2 until the store is reset by hand. Reproduced with a seeded store. Fix: persist fixed bucket counts, count and sum. | tools/ix-build/src/observe.rs:48; tools/ix-build/src/observe.rs:259-263; tools/ix-build/src/observe.rs:523-529 |
| FND-009 | high | Second copies of existing rules: histogram bounds restate metrics::BUCKETS_SECONDS in milliseconds, escape is byte-for-byte escape_label, priority labels and metric rendering restate metrics.rs, and write_state and the private-directory check restate store.rs. Graded high by the code-review duplication rule. Fix: reuse the metrics Histogram, constants and escaping (also fixes FND-008) and share the atomic write. | tools/ix-build/src/observe.rs:709-714; tools/ix-build/src/observe.rs:760-765; tools/ix-build/src/observe.rs:531-562 |
| FND-010 | medium | Every spawn error returns 127, so a non-executable file or a directory returns 127 where a direct exec returns 126. Reproduced. Fix: return 127 only for NotFound and 126 otherwise. | tools/ix-build/src/observe.rs:221-232 |
| FND-011 | medium | status, prune, recover, or a rejected run on an empty store fixes it as admitted although no request was recorded, so every later observe warns and records nothing. Reproduced all four. FR-015 says the first recorded request fixes the mode. Fix: write mode.json only when a request is recorded and have observe treat a requestless state.json as unfixed. | tools/ix-build/src/store.rs:84-99; tools/ix-build/src/observe.rs:489-491 |
| FND-012 | medium | If the post-spawn identity write fails, the busy row keeps command_pid null; killing the observer then frees the slot while the command is still alive. Reproduced with an injected ENOSPC: busy 1 before, 0 after, command running. Fix: retry the identity write from the wait loop until it succeeds. | tools/ix-build/src/observe.rs:241-271; tools/ix-build/src/observe.rs:662-668 |
| FND-013 | medium | The observed metrics --output path bypasses lock_metrics_export, so an older snapshot can rename over a newer one and counters step backwards. Reproduced: published ok went 2 then back to 1. Fix: take the export lock for observed stores too and drop the duplicate dispatch in main. | tools/ix-build/src/main.rs:122-133; tools/ix-build/src/runtime.rs:380-386 |
| FND-014 | medium | metrics and status ignore --config and export the capacity stored by the last observe: after changing capacity_units from 4 to 9 both still say 4, even with --config /nonexistent. Fix: read the host policy at export. | tools/ix-build/src/observe.rs:566-598; tools/ix-build/src/main.rs:122-141 |
| FND-015 | medium | The observed JSON snapshot has no schema_version or observed_at_ms, omits the zero ok or failed series and started-but-unfinished priorities that Prometheus shows, and dumps raw per-command waits instead of the histogram. FR-015 requires the versioned snapshot carrying the same values. Fix: emit version, observation time and the same series as render. | tools/ix-build/src/observe.rs:581-622 |
| FND-016 | medium | A symlinked host policy is rejected although admitted mode canonicalises it, so such a host records nothing and warns on every build. Reproduced. Fix: use fs::metadata and keep the regular-file and size checks. | tools/ix-build/src/observe.rs:415-418 |
| FND-017 | medium | On Linux an inherited ignored SIGCHLD auto-reaps the child, so waitpid returns ECHILD and the fallback reports exit 1 for every command, success included, without forwarding signals while blocked in wait. Read only; macOS returned the correct status. Fix: give SIGCHLD a handler after spawn, or reset it so the child can be waited for. | tools/ix-build/src/observe.rs:288-309 |
| FND-018 | medium | Catchable signals outside the list, for example SIGABRT sent only to the started PID, kill the observer: the caller-started process exits 134 while the command keeps running. Reproduced. Fix: also forward ABRT, TRAP, BUS, SYS and the platform extras signal-hook permits. | tools/ix-build/src/observe.rs:378-398 |
| FND-019 | low | A caller with stdin closed gets a command with fd 0 opened on /dev/null, unlike a direct run (Rust runtime behaviour). Reproduced. Fix: close it again before spawn if it can be detected safely, or record the limit in the spec. | tools/ix-build/src/observe.rs:221 |
| FND-020 | low | Any parse error with an argument equal to observe goes to the fallback: observe --help and help observe print a warning and exit 2, and a bad run command whose arguments contain observe exits 2 with no message. Equals-form options are dropped there. Reproduced. Fix: pass help and version errors to clap and match observe only in subcommand position. | tools/ix-build/src/main.rs:222; tools/ix-build/src/observe.rs:70-94 |
| FND-021 | low | Every observed metrics or status call must rewrite and fsync the store, so on a read-only or full disk export fails entirely instead of publishing the readable state. Reproduced on a read-only directory. Fix: render the read state when the reclaim write fails. | tools/ix-build/src/observe.rs:566-578 |
| FND-022 | low | observe.rs returns String errors that main maps all to the state code, losing mode refusal, corruption and timeout distinctions, and observe.json has no schema version while denying unknown fields. Fix: use the crate Error and codes and add a version field. | tools/ix-build/src/observe.rs:27-62; tools/ix-build/src/main.rs:125 |
| FND-023 | low | Docs: the README example does not say the state directory must already exist with mode 0700, so following it records nothing; reference.md promises bounds and preservation the code does not meet; the admitted-mode migration guidance was removed. Fix: document the directory and restore the guidance. | tools/ix-build/README.md:43-53; tools/ix-build/reference.md:140-143 |
| FND-024 | low | When the pre-start budget expires after the write has landed, the warned fail-open run leaves a busy row with no command identity that this observer never removes. Reproduced. Fix: check the deadline only before the rename, or remove the row on that path. | tools/ix-build/src/observe.rs:458-460 |

## Examined Scope

- examined: tools/ix-build/src/observe.rs, main.rs, store.rs, runtime.rs (metrics dispatch), Cargo.toml, README.md, reference.md, tests/observe.rs.
- context_only: metrics.rs, process.rs, config.rs; dev-team FR-015; sysinfo 0.37.2 and signal-hook 0.3.18 sources.

## New findings (disposition pass 1)

Head bbb3ede8ccacfc8a594e77e622cc95d7b2463c2d. Findings about the fix diff only.

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-025 | high | A pre_exec closure sets SIGPIPE to ignored in every command, whatever the caller had. A build script loop piped into head hangs forever (macOS and Linux), pipefail status changes from 141 to 1, and kill -PIPE no longer terminates. The added test asserts this behaviour. Fix: run the closure only when the caller really ignored SIGPIPE, captured before the Rust runtime changes it, or delete the closure and document the ignored-caller limit. | tools/ix-build/src/observe.rs:218-233; tools/ix-build/tests/observe.rs:563-571 |
| FND-026 | medium | SIGPIPE is registered and forwarded, so when the observer writes a warning to a broken stderr it signals the command; a command using default SIGPIPE dies with 141 although it never touched stderr. Reproduced 3 of 3. Fix: remove SIGPIPE from the forwarded list; fix together with FND-025. | tools/ix-build/src/observe.rs:475; tools/ix-build/src/observe.rs:349-361 |
| FND-027 | medium | The busy row now stores observer_started null, so any process that reuses a dead observer PID counts as a live observer and the slot stays busy. FR-015-AC-16 says a reused process ID does not count. Reproduced. Fix: record process_start of the observer in preflight, which is safe after spawn. | tools/ix-build/src/observe.rs:534; tools/ix-build/src/observe.rs:743-746 |
| FND-028 | low | Preflight remainder and completion write have separate budgets after command exit, so exit and stdio were held 1.95 s after an instant command (900 ms preflight stall, 5 s completion stall). Fix: one exit deadline for both waits. | tools/ix-build/src/observe.rs:361-445 |
| FND-029 | low | Stop signals are no longer registered, so SIGTSTP sent only to the started PID stops the observer while the command keeps running. Reproduced. Fix: record the limit in FR-015, or forward then stop self. | tools/ix-build/src/observe.rs:465-487 |
| FND-030 | low | On Linux the SIGCHLD handler installed before spawn resets a caller-ignored SIGCHLD to default in the command (SigIgn 0x10000 direct, absent observed). Fix: capture the inherited disposition and restore it in pre_exec. | tools/ix-build/src/observe.rs:214-215; tools/ix-build/src/observe.rs:459-465 |
| FND-031 | low | The crate-wide forbid(unsafe_code) gate was removed and the whole run function allows unsafe. Fix: keep forbid if the closure is deleted, otherwise confine the allow to a small module. | tools/ix-build/src/main.rs:3; tools/ix-build/Cargo.toml:24; tools/ix-build/src/observe.rs:158 |
| FND-032 | low | Preflight now runs beside the command and takes the state lock while it runs; FR-015 says the lock is released before the command starts and not held while it runs. Fix: amend FR-015 for start-first observation. | tools/ix-build/src/observe.rs:281-296 |
| FND-033 | low | Observed export takes the export lock before validating the directory, so metrics --output on a rejected non-private directory leaves metrics-export.lock in it. Reproduced. Fix: validate the state directory before opening the lock. | tools/ix-build/src/main.rs:129-139 |
| FND-034 | low | thread::spawn panics if a thread cannot be created; after the command is spawned that would exit the observer while the command runs. Read only. Fix: use thread::Builder and treat failure as an observer failure. | tools/ix-build/src/observe.rs:283; tools/ix-build/src/observe.rs:396; tools/ix-build/src/observe.rs:724 |

## New findings (disposition pass 2)

Head bce5f4a7a20000a56275e3bc49b290fc1053732d.

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-035 | medium | The observer forwards its own SIGXFSZ. Under a file-size limit smaller than the state being written (always at 0; 4 KiB after 300 runs on Linux) the preflight write raises SIGXFSZ, the observer's flag handler catches it and the wait loop sends it to the command, which dies (rc 153) where a direct run succeeds. Reproduced on macOS and Linux. Present since c4ef503 and missed in earlier passes. Fix: stop forwarding SIGXFSZ but keep a handler so the write returns EFBIG. | tools/ix-build/src/observe.rs:521-545 |
| FND-036 | medium | Any empty request list now counts as unused, so an admitted store written before this PR and emptied by prune (no mode.json, next_sequence 2) accepts observe, which writes observe.json; every later run there is refused with a request error. Introduced by the FND-011 fix; FR-015 says prune does not reset the mode. Fix: unused only when requests is empty and next_sequence is 1. | tools/ix-build/src/observe.rs:669-680 |
| FND-037 | medium | Signal handlers are registered after spawn. A forwarded signal sent to the observer PID after the command starts and before registration kills the observer while the command keeps running, so a cancel is lost and the wrapper sees the build end: 6 of 1000 on Linux at load 28, 0 of 1000 with early registration, 0 of 400 on macOS. When a test loses this race its looping child is orphaned and hangs the test output. Introduced by the FND-002 fix. Fix: register before spawn and re-ignore the caller's ignored signals in pre_exec. | tools/ix-build/src/observe.rs:227-262 |
| FND-038 | medium | The fix adds an 87-line C interposer compiled with cc, against the ticket brief rule Rust only; its macOS branches are never built. Fix: replace it with a Rust cdylib fixture compiled with rustc (verified on Linux). | tools/ix-build/tests/fixtures/io_delay.c:1-87 |

## Dispositions

Round 1, head bbb3ede8ccacfc8a594e77e622cc95d7b2463c2d. macOS gates: fmt and clippy exit 0, tests 44 unit, 53 observe, 15 supervision passed. Linux (temporary source copy, since removed): 55 observe tests passed.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-001 | fixed | Handled TERM, INT, USR1, HUP exit 0; WINCH then TERM death reports signal 15. |
| FND-002 | fixed | Caller-ignored 1,2,3,13,15 preserved on macOS and Linux; nohup rc 0. The new default-caller SIGPIPE defect is FND-025. |
| FND-003 | fixed | Group TSTP, TTIN, TTOU stop both; stop and continue completes with rc 0. PID-only residue is FND-029. |
| FND-004 | fixed | Broken and full-disk stderr: command runs, status preserved. Forwarded SIGPIPE residue is FND-026. |
| FND-005 | fixed | All valueless hint forms run the command. |
| FND-006 | fixed | 3 s preflight stall did not delay start; PID-only TERM delivered in 2 ms; single completion stall returned at 1.05 s. Combined case is FND-028. |
| FND-007 | fixed | Linux: nofile 1024 soft, 524288 hard both direct and observed. |
| FND-008 | fixed | Bounded histogram persisted. |
| FND-009 | fixed | Buckets, histogram, escaping, seconds, atomic write and directory check reused. |
| FND-010 | fixed | 126 for non-executable and directory, 127 for missing. |
| FND-011 | still-open | Status, prune and recover still write state.json on an empty store and observe still rejects any state.json; reproduced on macOS and Linux. The added test only checks mode.json. |
| FND-012 | fixed | Identity is inserted with the row in one write; no row without it. |
| FND-013 | fixed | Newer export survives the stalled older one. |
| FND-014 | fixed | Export reads current policy capacity. |
| FND-015 | fixed | Snapshot has version, time, zero series and histogram. |
| FND-016 | fixed | Symlinked policy recorded. |
| FND-017 | fixed | Linux with SIGCHLD ignored by sigaction: new rc 7 and 0 over 50 runs, old rc 1. |
| FND-018 | fixed | ABRT, TRAP, BUS, SYS, VTALRM forwarded; EMT, FPE, ILL remain unforwarded. |
| FND-019 | accepted-no-change | Limit documented in README and reference. |
| FND-020 | still-open | Help is fixed, but equals-form options are still dropped in the fallback, so an unknown option with --state=X records nothing. |
| FND-021 | fixed | Stdout export works on read-only and full state directories. |
| FND-022 | fixed | Typed errors and schema_version added. |
| FND-023 | still-open | Reference.md still states a before-start bound that no longer describes the design, and the migration guidance was not restored. |
| FND-024 | fixed | Late preflight row is removed by completion; busy empty after run. |

Round 2, head bce5f4a7a20000a56275e3bc49b290fc1053732d. macOS: fmt and clippy exit 0, tests 44 unit, 66 observe, 15 supervision passed. Linux (temporary copy, since removed): 71 observe tests passed.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-011 | fixed | Status, metrics, prune, recover and a rejected run on an empty store now leave observe working. Legacy-store residue is FND-036. |
| FND-020 | fixed | Equals forms are recorded in the fallback. |
| FND-023 | accepted-no-change | The stale bound text is corrected. The dropped README migration lines concern admitted-mode adoption and no build result depends on them; restoring them is optional. |
| FND-025 | fixed | Direct and observed dispositions, mask, pipeline hang case and pipefail status match for default and ignored callers on macOS and Linux. |
| FND-026 | fixed | SIGPIPE is no longer forwarded; stalled preflight with broken stderr completes 3 of 3. |
| FND-027 | fixed | Observer start time is recorded; a reused PID reports busy 0. |
| FND-028 | fixed | Compound preflight and completion stall returns at 1.03 s. |
| FND-029 | accepted-no-change | Stop signals keep default job control. README and reference tell wrappers to target the process group or cgroup; the contract wording is dev-team#43. |
| FND-030 | fixed | Linux caller-ignored SIGCHLD is preserved and exit statuses match. |
| FND-031 | fixed | Unsafe code is denied except in the inherit module and the single pre_exec installer. |
| FND-032 | accepted-no-change | The code keeps start-first with an overlapping lock; the contract is changed to match in dev-team#43, which must merge with this PR. |
| FND-033 | fixed | A rejected 0755 directory receives no file. |
| FND-034 | fixed | All observer threads use thread::Builder and handle spawn errors. |
| FND-035 | still-open | Found in this pass; edit in swm242-round2-edits.patch. |
| FND-036 | still-open | Found in this pass; edit in swm242-round2-edits.patch. |
| FND-037 | still-open | Found in this pass; edit in swm242-round2-edits.patch. |
| FND-038 | still-open | Found in this pass; replacement fixture in swm242-round2-edits.patch. |

Round 3, head fd77413b7651a1a215bec613cef30349da8a45d4. Verified: the fd77413 source is the bce5f4a tree plus the round-two edit patch; only reference.md paragraphs are arranged differently, with the same content.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-035 | fixed | SIGXFSZ is handled and not forwarded; the delta is the verified edit. |
| FND-036 | fixed | A pruned legacy admitted store is refused. |
| FND-037 | fixed | Handlers register before spawn and the child restores ignored signals. |
| FND-038 | fixed | The C fixture is replaced by a Rust one. |
