---
id: SR-006
title: "Gap analysis of locked-build observe mode (SWM-242)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/dev-tools; tools/locked-build/tests/observe.rs, src/observe.rs; PR dev-tools#27 at c4ef5039f28fa2004af3ec3730a878681717d42c; dev-team FR-015 AC-13..22"
review_set: subset
---

## Summary

Ticket: SWM-242. Checked FR-015-AC-13..22 against the 29 tests in tests/observe.rs. Each of the ten criteria has at least one Trace tag, but several clauses are unasserted, which is how defects in SR-005 pass a green suite.

## Verdict

**CONDITIONAL**. Medium and low findings only in this analysis; the high defects are in SR-005.

## Findings

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-001 | medium | No AC-15 test compares dispositions, mask, inherited descriptors or process group with a direct run, covers a command that handles a signal and exits normally, or runs the signal checks on a fail-open path. Fix: add a probe command asserting each against a direct run. | tools/locked-build/tests/observe.rs:127-167; tools/locked-build/tests/observe.rs:274-293; tools/locked-build/tests/observe.rs:444-471 |
| FND-002 | medium | AC-14 bound tests allow 1.5 s for the held lock, the FIFO policy is rejected by the file-type check so the bounded config read never runs, and no test stalls a state write. Fix: assert the bound with an injected slow write and read. | tools/locked-build/tests/observe.rs:297-325; tools/locked-build/tests/observe.rs:373-396 |
| FND-003 | medium | next_observe_reclaims_dead_observer_and_command_slot asserts through metrics, which reclaims by itself, so removing reclaim from observe preflight still passes. Fix: assert on observe.json before any export. | tools/locked-build/tests/observe.rs:536-562 |
| FND-004 | medium | AC-17 never asserts a build counted as failed for a nonzero exit or signal, and AC-19 asserts only series presence, never the wait value, bucket, sum or count, so a unit error passes. | tools/locked-build/tests/observe.rs:187-213; tools/locked-build/tests/observe.rs:685-704 |
| FND-005 | medium | AC-21 is tested only for the held lock; unwritable and corrupted state after start are not tested. | tools/locked-build/tests/observe.rs:796-834 |
| FND-006 | low | Partial clauses: AC-13 breached thermal or memory limit and hold --pause and AC-20 governor endpoint are not exercised (no producer in this build), AC-20 JSON build and test series are unasserted, AC-18 --queued-at-ms -1 is untested. | tools/locked-build/tests/observe.rs:706-735; tools/locked-build/tests/observe.rs:751-794 |
| FND-007 | low | Behaviour with no owning requirement: raw statuses capped at 10000 with silent drop, 32 MiB store limit, 1 MiB and symlink policy refusal. | tools/locked-build/src/observe.rs:24; tools/locked-build/src/observe.rs:347-349; tools/locked-build/src/observe.rs:415-418 |

## Coverage

- Computed matrix unavailable: quoin matrix and quire matrix both refuse because dev-tools has no spec directory (QQ-1002). Fallback: Trace comment grep index, declared as such.
- Tagged: FR-015-AC-13, 14, 15, 16, 17, 18, 19, 20, 21, 22. Criteria AC-1..12 belong to admitted mode, are outside this PR and were not reassessed.
- Reverse gap and stubs: no stub, placeholder or skipped observe test found; unowned behaviour is FND-007.
- Semantic review: not opted in; only a bounded intent check of AC-13..22.
- Plan completion: not assessed

## New findings (disposition pass 1)

Head bbb3ede8ccacfc8a594e77e622cc95d7b2463c2d. The new test file run against the c4ef503 source on Linux gave 36 passed, 19 failed.

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-008 | medium | Several added tests do not detect their defect. The ignored SIGCHLD, PID-only abort and both held-lock tests pass 5 of 5 on the old source (sh trap does not ignore SIGCHLD; the abort test checks only the observer; thresholds are 2 s). The empty-store test checks mode.json and never runs observe. The broken-stderr test fails on old source in isolation but passed in the parallel suite. | tools/locked-build/tests/observe.rs:338-390; tools/locked-build/tests/observe.rs:636-646; tools/locked-build/tests/observe.rs:961-1032 |

## Dispositions

Round 1, head bbb3ede8ccacfc8a594e77e622cc95d7b2463c2d.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-001 | still-open | Handled-signal, nohup, descriptor and mask tests were added, but the SIGPIPE test asserts the defect SR-005 FND-025 and process group is not compared. |
| FND-002 | still-open | No slow read or write is injected, the new held-lock tests pass on the old source, and the post-exit bound still allows 1.5 s. |
| FND-003 | still-open | Next_observe_reclaims_dead_observer_and_command_slot is unchanged. |
| FND-004 | still-open | Wait value, buckets and count are now asserted; no build is asserted counted as failed. |
| FND-005 | still-open | No test makes state unwritable or corrupt after start; the read-only test covers export. |
| FND-006 | accepted-no-change | AC-18 and AC-20 parts are now tested; the AC-13 governor and hold clause has no producer in this build. |
| FND-007 | still-open | No requirement or spec note added for the limits. |

Round 2, head bce5f4a7a20000a56275e3bc49b290fc1053732d.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-001 | fixed | Default and ignored disposition probes and a process-group comparison with a direct run were added; the old SIGPIPE assertion is gone. |
| FND-002 | still-open | I/O injection and an 800 ms held-lock start bound were added. slow_config_read (1500 ms) still passes on c4ef503, and preflight_and_completion_share_one_post_exit_deadline passes on bbb3ede. Test-only; verified edits are in the patch. |
| FND-003 | fixed | The test reads observe.json before any export. |
| FND-004 | fixed | A failed build is asserted. |
| FND-005 | fixed | Unwritable and corrupted state after start are tested. |
| FND-007 | accepted-no-change | The limits are documented in README and reference; they are implementation bounds and do not change a build. |
| FND-008 | still-open | The SIGCHLD, empty-store, abort and held-lock start gaps are closed. broken_stderr now passes on every version because the closed descriptor 2 is reopened as /dev/null, and the held-lock signal test passes on c4ef503. Test-only; verified edits are in the patch. |

Round 3, head fd77413b7651a1a215bec613cef30349da8a45d4. Verified: the fd77413 source is the bce5f4a tree plus the round-two edit patch; only reference.md paragraphs are arranged differently, with the same content.

| FND | outcome | reason |
| --- | ------- | ------ |
| FND-002 | fixed | 800 ms start bounds and a combined late-preflight and stalled-completion test fail on the old heads and pass on the edit. |
| FND-008 | fixed | Broken stderr uses a closed pipe; it and the held-lock signal test fail on c4ef503. |
