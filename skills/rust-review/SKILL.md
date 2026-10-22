---
name: rust-review
description: Review Rust changes against Agent IX Rust style, repository conventions, behavior, test evidence, safety and concurrency, wire contracts, resource bounds, and required gates. Load rust-style for every Rust review, including repositories with local idioms; use when code-review dispatches on Rust.
---

# Rust Review

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

This is the Rust lane of `code-review`. Python `pytest` and `mocker` checks
do not translate into Rust findings. Load [rust-style](../rust-style/SKILL.md)
for every Rust review and read its references relevant to the changed code.
Then read the repository's `AGENTS.md`, `CLAUDE.md`, Rust skill, `CONTRIBUTING.md`,
`clippy.toml`, `deny.toml`, `rustfmt.toml`, and root `Cargo.toml` lints where
they exist. Specific repository conventions outrank the published defaults.
An existing implementation that violates a documented rule does not make the
rule stale. Enforce it unless a newer explicit project authority supersedes
it; report conflicting authorities when their intended precedence is unclear.

Read the repository's idiom document as a checklist. Cite the applicable
rule when reporting a deviation; compiling and passing tests do not by
themselves excuse a rule the project adopted. Use the same checklist before
implementing a grammar, protocol, schema, state machine, or decision table:
choose a representation whose coverage can be audited directly.

## Review sequence

1. Record the changed crates and files, repository conventions, and CI
   workflow diff. Check whether the change narrows a gate before drawing
   conclusions from a green suite.
2. Apply the core checks below. Open the linked `rust-style` references and
   [detailed review checks](references/review-checks.md) for the parts of the
   change they cover. Inspect callers and tests needed to establish behavior.
3. Find concrete failure scenarios: malformed or untrusted input reaching a
   panic; a new enum variant silently mapped to a wrong value; a duplicate
   source of truth drifting; a test whose assertion cannot fail or execute;
   a blocking or lost task; a wire or persistence conversion changing data.
4. Run the repository's actual gates and report their results. Classify each
   finding using the contract and consequence below, then write the review
   artifact and dispositions as required by `code-review`.

## Core review checks

- **API and docs:** For changed public items, check intended consumers,
  visibility, documented behavior, and tests. An API needs no caller within
  its defining crate, but public exposure without a justified use or contract
  is a finding. Compare doc comments with implementation: a promise the code
  does not fulfill is a separate finding. Apply [API design](../rust-style/references/api-design.md).
- **Implementation:** Find unfinished behavior, no-op trait methods, placeholder
  returns, debug output, and `todo!()` or `unimplemented!()` on a live path. A
  `TODO` comment is a finding when work required by this change remains undone;
  a tracked future task alone is not. Check whether a new variant, field, or
  rule requires hand-editing several sites. Apply [implementation](../rust-style/references/implementation.md).
- **Tests:** Ask which source change would make each assertion fail. Flag
  tautological assertions, guarded assertions that may never run, tests that
  replace the unit under test, missing trace tags where the repository
  requires them, and tests that rely on an unavailable service without a
  defined lane. Apply [testing](../rust-style/references/testing.md).
- **Safety and lifecycle:** Trace caller-controlled input to panics, unsafe
  operations, unbounded work, shared state, cancellation, and shutdown. Check
  that a spawned task's failure is observed where it matters. Apply
  [safety and concurrency](../rust-style/references/safety-concurrency.md).
- **Boundaries:** Check error distinctions, deserialization, wire versions,
  paths, secret-bearing types, IPC shapes, and narrowing conversions through
  to the value actually stored or sent. Apply [wire and persistence](../rust-style/references/wire-and-persistence.md).
- **Tokio when used:** Check blocking work on async workers, runtime re-entry,
  locks held across `.await`, and sync-over-async bridges. Apply
  [Tokio](../rust-style/references/tokio.md).

## Findings and severity

- High: a broadly consequential wrong result, reachable serious failure, or
  weakened required gate. A catch-all codec arm that exports an unknown
  variant as another valid value belongs here; describe the input and wrong
  result. `high` is the shared `SpecReview` artifact's ceiling, even when the
  impact would be described as critical in prose.
- Medium or Low: a narrower defect or a violation of a documented Agent IX
  or repository rule, graded by consequence and reachability. Working code
  that violates an applicable style rule is still a finding; passing tests
  do not erase it.
- A reviewer's preference without an applicable rule or concrete concern is
  not a finding. If useful, put it in a separate style note; do not treat the
  availability of another idiom as a defect.

## Gates — run them, don't assume them

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p <touched crates>            # plus the repo's DB/integration lane
cargo deny check                           # if deny.toml exists
```

Diff CI workflow files before reviewing code. Removing a lane, narrowing
one (`--all-features` replacing a `--no-default-features` lane), adding
`continue-on-error`, or dropping a matrix entry weakens the gate even when
the remaining suite is green. Report that independently at the severity of
the coverage removed and compare it with the change's requirements.

Run the repository's scripts when they exist instead of approximating them.
Do not pipe test output through `tail` or `head` in a way that loses the test
process exit code.

## Output

Report findings in a table, most severe first. Each has `file:line`, a
`high`/`medium`/`low` severity, and the violated rule or concrete failure
scenario. Keep optional style notes outside the findings table.

When run as `code-review`'s Rust lane, fold findings into that skill's one
`SpecReview` artifact under `code-review`'s `SR-NNN` id. Do not create a
second artifact or id for the same analysis. Separate `gap-analysis` or
`spec-review` analyses still have their own artifacts and ids. When run
standalone, follow the `code-review` skill's Output section: allocate an id
after checking every existing `SR-` id, including quoted forms such as
`id: "SR-014"`; record full scope, including files with no findings; and
use at most one document-level `No findings (placeholder)`
row at `low` severity if the schema requires a nonempty table. State the
clean conclusion in `## Verdict`.

After a fix round, give every finding a separate `## Dispositions` entry:
`fixed`, `rejected: <reason>`, `deferred: <reason>`, or
`accepted-no-change`. Do not rewrite the original finding to describe its
later outcome. Use a host findings-reporting tool when one exists; otherwise
emit the table and gate results verbatim.
