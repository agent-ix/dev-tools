---
id: SR-004
title: "Gap analysis of locked-build metrics export (SWM-242)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/dev-tools; tools/locked-build/src/metrics.rs, runtime.rs, model.rs and their tests; PR dev-tools#25; dev-team FR-015"
review_set: subset
---

## Summary

Ticket: SWM-242. Checked head 97ba21775eaaa6c794fb1acfa3c69fff3c7a450c against dev-team FR-015 at ea0d2e6024900686658c5feb88b15fcbeb776b31. The three new tests carry FR-015 tags wider than what they assert, the runtime wiring of both new measurements is untested, and one exported series has no owning requirement.

## Verdict

**CONDITIONAL**. Medium and low findings only in this analysis; the high code defects are in SR-003. This is a bounded manual criterion-to-test check, not a computed matrix PASS.

## Findings

| ID      | Severity | Summary | Refs |
| ------- | -------- | ------- | ---- |
| FND-001 | medium | The test tagged FR-015-AC-8 does not iterate any termination reason and only checks that selected lines read 1; it never asserts that no other outcome series changed, so a double increment elsewhere passes. | tools/locked-build/src/metrics.rs:391-509 |
| FND-002 | medium | The test tagged FR-015-AC-1 checks only blocked-seconds arithmetic through direct observe_block calls; it covers no scripted request sequence, no other series and no promtool check. | tools/locked-build/src/metrics.rs:511-524 |
| FND-003 | medium | The FR-015-AC-10 test never captures values before restart, so equality across restart, second recover and prune is not asserted; it checks three series after the fact. | tools/locked-build/src/runtime/tests.rs:201-240 |
| FND-004 | medium | Runtime wiring is untested: with the record_queue_wait and observe_block calls removed from admit_worker, all 31 tests still pass. No test asserts queue_depth, slots_busy, histogram bucket values, multiple waiters through the runtime, or the textfile mode. | tools/locked-build/src/runtime.rs:528-544 |
| FND-005 | medium | ix_build_slots_configured has no owning requirement: FR-015 lists slots_busy, slots_sustainable and slots_max, no dev-team or ix-board branch names the new series, and ix-board FR-041 reads slots_sustainable. The required spec amendment is missing. | tools/locked-build/src/metrics.rs:237-246 |
| FND-006 | low | The test tagged FR-015-AC-9 asserts the all-zero start but never ends a check request aborted_thermal, which the finish path can be scripted to do, so the second half of the criterion is unverified. | tools/locked-build/src/metrics.rs:391-496 |
| FND-007 | low | No test is tagged to or asserts the FR-015-AC-4 exclusion of ticket ids, request ids, command text and environment from the new exposition. | tools/locked-build/src/metrics.rs:176-282 |

## Coverage

Plan completion: not assessed.

`quire matrix --scope <dev-tools worktree> --format json` exits 3 because dev-tools has no spec root; the criteria live in dev-team. No matrix was invented. FR-015 criteria checked by hand against the changed tests:

| Criterion | Result |
| --------- | ------ |
| FR-015-AC-1 | tagged, not verified (FND-002) |
| FR-015-AC-2 | busy-slot half applies; untested (FND-004); sustainable and pause halves out of slice |
| FR-015-AC-3 | partial: terminal counters across prune; histogram only by hand serialization; blocked counter not covered |
| FR-015-AC-4 | label exclusion untested (FND-007); probe_unavailable out of slice |
| FR-015-AC-5 to AC-7 | out of slice (governor, lane labels, live scrape) |
| FR-015-AC-8 | partial, non-abort outcomes without exclusivity (FND-001) |
| FR-015-AC-9 | partial (FND-006) |
| FR-015-AC-10 | partial (FND-003) |

Semantic review beyond the FR-015 tags added by this diff was not run. The mutation used for FND-004 was reverted; the worktree is clean at the reviewed head. No stubs were found in the changed code.
