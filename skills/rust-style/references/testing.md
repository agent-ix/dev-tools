# Testing

Read this when writing or reviewing Rust unit tests, integration tests,
test doubles, requirements trace tags, or tests that need an external service.

## Placement and traceability

- Place unit tests beside the code in `#[cfg(test)] mod tests`. Put
  integration tests in `tests/` and exercise the public API. A test that
  reaches internals through `#[path]` needs an established repository reason.
- Name tests for behavior; do not add a `tc_NNN_` prefix merely to satisfy a
  general convention. Keep an existing repository naming convention where
  its test selection depends on it.
- In a repository that traces tests to requirements, bind every new test to
  the criteria it asserts using `/// Trace: FR-001-AC-2, NFR-003-AC-1` or
  the repository's declared `#[trace("FR-001-AC-2")]` attribute. `Tracing:`
  binds nothing. Put issue, task, and plan IDs on a separate
  `/// Provenance:` line. Use `quire matrix --strict` when available to
  inspect the computed matrix; do not hand-edit matrix rows.

## Evidence that can fail

- Assert the behavior named by the test, not only `result.is_ok()` when a
  value or effect is at issue. Every assertion must be able to fail when the
  implementation is wrong. An assertion on a value the test itself supplied
  does not measure production behavior.
- Make guarded assertions execute on the intended path. Assert the guard
  first or make the other branch fail; `if let Some(x) = result { assert!(...)
  }` can silently pass when `result` becomes `None`.
- Add assertion context when the expression alone will not explain a
  failure, especially for a protocol invariant or a value useful in logs.
  Do not merely repeat the assertion in the context string.
- An ignored test needs a named lane or tracked reason. Use `#[should_panic]`
  only when panic is the intended contract, not to accept a recoverable
  failure as a panic.

## Real seams and deterministic resources

- Test production behavior through a real code path. Use an injected trait,
  an in-memory implementation of a port, a temp directory, or a scratch
  service as a seam. A decorator of the same trait can count, record, or
  fail calls. Do not replace the exact unit under test with its own double.
- Do not put `#[cfg(test)]` branches inside production functions to bypass
  behavior. An in-memory implementation used only by its crate's unit tests
  stays private and test-only. Make it `pub` only when integration or downstream
  tests need to import it; document it as a testing API. Use a default-off
  `test-support` feature when downstream tests need the implementation without
  making it part of the ordinary production API.
- Prefer a behavioral test. A source-inspection test using `include_str!`
  is a last resort for a property with no behavioral path; compile its
  fixture into the test rather than depend on a runtime working directory.
- Do not use `thread::sleep` as a synchronization oracle. Wait for a
  positive observable condition through a channel, flag, counter, or
  condition variable. Keep wall-clock performance thresholds in a named
  benchmark lane rather than default tests.
- Avoid dependence on `HashMap` iteration order, system time, ambient home
  directories, inherited environment, or a fixed port. Assign resources
  per test, process, or worktree as appropriate.

## External services, when present

- Tests needing a database, daemon, or network service skip cleanly when
  that dependency is absent or live in a named `#[ignore]` lane. Run the
  service-backed lane when verifying changes to that integration.
- For Postgres integration tests, use a scratch database fixture and skip
  cleanly when `DATABASE_URL` is unset. Run the same tests against a
  throwaway server in the named integration lane.
