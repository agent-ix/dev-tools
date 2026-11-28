---
id: SR-007
title: "Code review — SWM-242 read-only sampler"
type: SpecReview
analysis: code-review
scope: "agent-ix/dev-tools; tools/ix-build/src/sample.rs, src/main.rs, src/metrics.rs, tools/ix-build/tests and docs; PR #28 remediation at f65087b410b21e2f42211e9b34800e86d1e25885"
review_set: subset
---

## Summary

Re-reviewed PR #28 at `f65087b410b21e2f42211e9b34800e86d1e25885`, focusing on the three findings recorded against `ee13ee198eedfca82c5f5fc02fcb4f1e68cb8507`. The duplicate helper, broad Cargo subcommand match, and unbounded macOS process enumeration are corrected; ps reads now have byte caps and close the pipe before waiting.

## Verdict

**PASS** — the three recorded findings are resolved at this head, focused Rust checks pass, and no unbounded process-source reads remain in the reviewed sampler.

## Findings

| ID | Severity | Summary | Refs |
|---|---|---|---|
| FND-001 | high | `sample.rs` adds a second `escape_label` implementation that duplicates the existing `metrics::escape_label` behavior. This creates two sources to keep aligned for Prometheus escaping and violates the repository code-review duplication rule. Reuse the existing helper via an appropriate crate-visible API or move it to one shared location. | tools/ix-build/src/sample.rs:398-403; tools/ix-build/src/metrics.rs:530-536 |
| FND-002 | medium | Luna's `cargo_tree_busy` treats any argument exactly equal to `build`, `test`, or `check` as the Cargo subcommand. For example, `cargo help build` or `cargo new build` is counted as an active build despite not running a build/test/check. Parse the actual Cargo subcommand and add a negative fixture test; otherwise Luna busy metrics can be falsely attributed. | tools/ix-build/src/sample.rs:597-607; tools/ix-build/src/sample.rs:1172-1208 |
| FND-003 | medium | Draco's adapter refreshes the entire system process list and requests command/executable data for all processes through `ProcessesToUpdate::All`, but never applies `PROCESS_LIMIT` or a corresponding byte/count limit. The configured bound applies only to the Linux `/proc` adapter, so the macOS process-table read/enumeration is unbounded against the sampler decision's bounded-source requirement. Bound the macOS enumeration or report the source unavailable when its cap is exceeded. | tools/ix-build/src/sample.rs:476-503 |

## Dispositions

| ID | Outcome | Reason |
|---|---|---|
| FND-001 | fixed | Removed the sampler-local helper and made `metrics::escape_label` crate-visible; the sampler calls that single implementation (`tools/ix-build/src/metrics.rs:530-536`, `tools/ix-build/src/sample.rs:333-334`). |
| FND-002 | fixed | Added `cargo_subcommand` to parse the command position after Cargo global options, and tests show `cargo help build` and `cargo new build` are not counted (`tools/ix-build/src/sample.rs:767-800`, tests at 1457-1481). |
| FND-003 | fixed | Replaced full `sysinfo` enumeration with bounded `/bin/ps` reads. The summary output is capped at `PROCESS_LIST_BYTES_LIMIT`, parsed rows are capped at `PROCESS_LIMIT`, and lockf candidates are capped at `LOCKF_PROCESS_LIMIT`; detailed command output is separately capped. `bounded_command_output` reads at most `maximum + 1` bytes, drops stdout before waiting, rejects over-limit output, and checks the child status (`tools/ix-build/src/sample.rs:478-605`). The other process source uses bounded `/proc` entry counts and `read_bounded`; all `read_to_end` call sites in the sampler are behind `.take(maximum + 1)`. |

## Checks

- `cargo fmt --manifest-path tools/ix-build/Cargo.toml --check`: passed.
- `cargo clippy --locked --manifest-path tools/ix-build/Cargo.toml --all-targets -- -D warnings`: passed.
- `cargo test --locked --manifest-path tools/ix-build/Cargo.toml sample::tests`: 12 passed.
- `git diff --check ee13ee198eedfca82c5f5fc02fcb4f1e68cb8507..HEAD`: passed.
- Manual source-bound audit: both `read_to_end` sites are preceded by `take(maximum + 1)`; `/proc`, fdinfo and target-directory enumeration each has an entry cap. The macOS ps pipe is closed before `wait`, including the over-limit path.
- Manual lockf matcher audit: `ps` first identifies bounded candidate PIDs by `comm` basename, then reads bounded command rows only for those PIDs. Counting requires argv[0] `/usr/bin/lockf`, the exact `-t 0 <configured-path>` sequence, a configured path, and a non-zombie direct child. The parser checks candidate PID membership and rejects duplicate rows.
- The dev-tools repo has no `spec/` tree, so `quoin matrix` cannot derive the matrix here; the sampler behavior was checked manually against FR-015-AC-13..18. No AssuranceProfile applies.
