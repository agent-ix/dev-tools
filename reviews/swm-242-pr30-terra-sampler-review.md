---
id: SR-008
title: "Code review — SWM-242 Terra sampler source"
type: SpecReview
analysis: code-review
scope: "agent-ix/dev-tools; tools/ix-build/src/sample.rs, tools/ix-build/reference.md; PR #30 at 8970297f211ed646671cb83a2edd7850c426928d"
review_set: subset
---

## Summary

Reviewed PR #30 at `8970297f211ed646671cb83a2edd7850c426928d` against FR-015-AC-15 and AC-17, focusing on Terra's unprivileged wrapper-specific lock sampling. The adapter counts distinct configured lane paths only from matching wrapper Bash processes with a `FLOCK WRITE` fdinfo record, bounds its scans, and does not modify wrappers or lock files.

## Verdict

**PASS** — no findings; the focused checks pass and the observed metrics omit unavailable sources rather than reporting false zero values.

## Findings

| ID | Severity | Summary | Refs |
|---|---|---|---|
| FND-001 | low | No findings (placeholder) | - |

## Checks

- `cargo fmt --manifest-path tools/ix-build/Cargo.toml --check`: passed.
- `cargo clippy --locked --manifest-path tools/ix-build/Cargo.toml --all-targets -- -D warnings`: passed.
- `cargo test --locked --manifest-path tools/ix-build/Cargo.toml sample::tests`: 12 passed.
- `git diff --check origin/main...8970297f211ed646671cb83a2edd7850c426928d`: passed.
- Manual source audit: Terra filters bounded PID enumeration by `/proc/<pid>` directory UID, bounded `stat` comm `bash`, and bounded NUL-separated cmdline with exact `argv[1] == wrapper_path`. It scans only matching wrapper fd directories, bounds per-process and total fd counts, reads fdinfo only for configured lane symlink targets, bounds fdinfo bytes, and counts only `FLOCK WRITE` records. A source read error omits `ix_build_slots_busy` and emits `ix_build_probe_unavailable`; the configured-slot gauge is separately sourced.
- The wrapper-only source does not open or stat lane lock paths, use privileged process access, or change wrapper/lock files. Diff files are limited to `sample.rs` and `reference.md`.
- Documentation retains the host-timer invocation model and describes Terra's wrapper path and lane source. The repository has no `spec/` tree, so the change was checked manually against FR-015-AC-15 and AC-17; no AssuranceProfile applies.
