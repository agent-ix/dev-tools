---
name: rust-review
description: Review Rust code — idiomatic-Rust conformance and repo idioms, test conventions and tracking tags, trait-seam boundaries, stub and tautology detection, panic and unsafe surface, async/blocking and lock discipline, integer conversions at wire and persistence boundaries, resource bounds, and the gates that must actually be run. Use when reviewing a Rust change or crate, or when `code-review` dispatches on a Cargo workspace.
---

# Rust Review

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

The Rust half of `code-review`. Everything here is stated in Rust terms —
`pytest`/`mocker`/`pass` checks do not apply and must not be transliterated
into false findings ("this file has no test classes" is not a Rust finding).

## 0. Load the project's own conventions first

Project conventions outrank this skill. Before reviewing, read whichever of
these exist and treat them as the authority on style, layout, and gates:

- `.claude/skills/rust-style/SKILL.md` (or any repo skill describing Rust idioms)
- `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`
- `clippy.toml`, `deny.toml`, `rustfmt.toml`, `[workspace.lints]` in the root `Cargo.toml`

If the repo documents no idioms of its own, invoke the `rust-style` skill for
the portable default set. A repo's own idiom doc always wins over it.

A deviation from a documented repo idiom is a finding. A deviation from this
skill that the repo documents differently is not.

Read the repo's idiom doc as a **checklist**, not as background. Those idioms
are usually scar tissue — each one is there because something went wrong
without it — so "it compiles and passes" is not a defense for writing around
one. Cite the specific idiom when reporting.

Use this review checklist pre-implementation too. Before writing a mechanism
derived from a normative grammar, schema, protocol, state machine or decision
table, choose a representation whose correspondence to that authority can be
audited directly; do not wait for PR review to discover structural drift.

## 0b. Idiomatic Rust

Non-idiomatic code that works today is a finding: it is the shape that breaks
under the next change. Check the repo's own idioms first (§0), then these.

- **Errors.** One error type per crate boundary, `thiserror`-derived, with
  stable codes where the repo has a catalog. Not `Box<dyn Error>` on a public
  API, not `String` errors, not `anyhow` in a library. Errors carry context
  rather than being stringified and re-parsed.
- **The variant set is the API, not the message.** An error variant whose
  payload is a human-readable string, constructed with a different literal at
  each site, has moved the discriminant into prose: `Selection(&'static str)`
  standing for five distinct refusals is one variant doing five jobs. Symptom
  to grep for: tests that can only assert `matches!(.., Variant(_))`, and
  callers that compare messages to decide what happened. Each condition a
  caller must distinguish is its own variant with typed fields. A module doc
  claiming the kind is not message-derived, above code that derives it from a
  message, is a second finding.
- **`Result` over panic.** Recoverable conditions return `Result`; `unwrap` on
  a value the caller supplied is a bug, not a shortcut (§6).
- **Ownership at the signature.** Take `&str`/`&Path`/`&[T]` and return owned;
  accept `impl Into<String>` / `impl AsRef<Path>` where the repo does. A
  `String` parameter that is immediately borrowed, or a `clone()` to satisfy
  the borrow checker rather than to express intent, is a finding.
- **Newtypes over primitives.** Ids, scopes, and handles as newtypes rather
  than bare `String`/`Uuid`, so two ids cannot be swapped at a call site.
- **Iterators over index loops.** `for i in 0..v.len()` with `v[i]`, manual
  `push` loops that are a `map`/`collect`, and `.iter().count() > 0` instead of
  `!is_empty()`.
- **Matching.** Exhaustive `match` over a closed enum rather than a catch-all
  `_` arm that silently absorbs a new variant. `if let`/`let else` over nested
  `match` on `Option`. `?` over `match ... return Err`. A catch-all that maps
  to a *value* rather than to an error is the worst case: `_ => Operator::Add`
  in a lowering or codec table means the next variant added elsewhere is
  exported as the wrong value, accepted downstream, with no panic and no failing
  test. Grade these Critical, not style.
- **Construction.** Consuming builders (`fn with_x(mut self) -> Self`) where
  the repo uses them; `Default` implemented rather than a `new()` full of
  zeros; `From`/`TryFrom` rather than ad-hoc `to_x`/`parse_x` helpers.
- **Types that state the invariant.** `Option<T>` for genuinely absent, not a
  sentinel; `NonZeroUsize`, `Box<str>`, `Cow<'_, str>`, `BTreeMap` for
  deterministic order where it matters.
- **Traits.** Implement the standard trait (`Display`, `FromStr`, `Iterator`)
  instead of an inherent method with the same job. Don't add a trait with one
  implementation and no seam purpose.
- **Docs.** `///` on every public item and a `//!` module header saying what
  the module owns, where the repo requires it; a doc comment that describes an
  intention the code does not implement is a finding on its own.
- **Visibility.** `pub` only for what the crate's consumers use; prefer
  `pub(crate)`. A `pub` item with no external caller is either API debt or a
  missing test.
- **One fact, one place.** For each fact the change introduces — a variant's
  data, a supported version, a rule, a name — count the sites that must be
  hand-edited to add the next one, and how many of those the compiler checks.
  Five parallel `match` arms plus a `CASES: [Case; 13]` array plus a table in
  the tests is six edit sites and two enforced; a version pin written into two
  files stays correct only by memory; a rule implemented verbatim in two
  modules will diverge. Name the missing single source of truth (a struct, a
  const, an exhaustive `match` on one enum) rather than only the duplication.
- **Keep declarative authorities declarative.** Compare code shape with the
  authority it implements. A grammar, schema, protocol/state transition table,
  capability matrix or wire field catalog transcribed into a long ordered chain
  of `expect`/`if`/`match` calls is a finding when reviewing additions or
  detecting omissions requires mentally replaying control flow. Prefer one
  typed table, exhaustive enum mapping, generated representation or bounded
  interpreter that makes authority-to-code coverage inspectable. Named semantic
  nodes/handles beat opaque indices whose meaning exists only in statement
  order. Function length alone is not the issue, and genuinely sequential
  application logic need not be forced into data. Report the concrete drift
  scenario: for example, adding a grammar production to the standard can leave
  one imperative parser branch, recovery path or CST child layout unchanged
  while tests still cover only the old forms.

## 1. Test standards

- **Naming and tracing.** If the repo traces tests to requirements (a
  `/// Trace: FR-XXX-AC-N` doc line or `#[trace("FR-XXX-AC-N")]`), every new
  test carries its criterion ids and every cited id resolves. Untagged new
  tests in a traced repo are a finding, and so is a `Tracing:` line — it binds
  nothing. Test names describe behaviour; a missing `tc_NNN_` prefix is not a
  finding. Verify with `quire matrix --strict` (quire-cli 0.34.0 or later), not
  hand-written matrix rows.
- **Placement.** Unit tests in `#[cfg(test)] mod tests` beside the code;
  integration tests in `tests/` reaching only the public API. A `tests/` file
  reaching internals through `#[path]` includes is acceptable only where the
  repo already does it deliberately.
- **External dependencies.** Tests needing a database, network, or daemon must
  skip cleanly when the dependency is absent (`DATABASE_URL` unset → return,
  not panic), or be `#[ignore]`d into a named lane.
- **No clock-based assertions.** `thread::sleep` followed by an assertion is a
  finding: it passes vacuously when it arrives early and fails under parallel
  load when it arrives late. Require a positive, observable signal (a counter,
  a channel, a state flag) and a condition wait. Wall-clock *thresholds*
  (`elapsed < 50ms`) belong in an `#[ignore]`d benchmark lane, never in the
  default test run.
- **Determinism.** Flag tests depending on iteration order of `HashMap`,
  system time, ambient `$HOME`/env, or a fixed port. Per-worktree or
  per-process resource assignment beats a constant.

## 2. Seam compliance (the "mock boundary" check)

Rust has no `mocker`. The equivalent question is *where the seam is*:

- **pass** — a trait injected at construction (`Arc<dyn Repository>`), an
  in-memory implementation of a port, a temp dir, a scratch database.
- **pass** — a decorator implementing the same trait to observe traffic
  (counting, recording, failing) — this is how you *measure* rather than
  assume, see §4.
- **fail** — `#[cfg(test)]` branches inside production functions that change
  behavior under test.
- **fail** — a feature flag whose only purpose is to bypass real logic in tests.
- **fail** — a test that replaces the exact unit under test with a double and
  therefore asserts on its own stub.
- **Source-inspection tests** (`include_str!` + assertions on the text) are a
  legitimate last resort for properties no runtime path can reach. They are a
  finding when a behavioral test was possible.

## 3. Completeness (source)

- `todo!()`, `unimplemented!()`, `TODO`, `FIXME`, `XXX`, `dbg!`, stray
  `eprintln!` debugging.
- Trait impls that silently no-op where the trait promises work.
- Functions returning `Ok(Default::default())`, empty `Vec`, or `None` as a
  placeholder rather than a modeled answer.
- Modules that only re-export, added without a reason.
- A public API added with no caller and no test — either wire it or don't ship it.
- New `pub` items with no doc comment in a repo that documents its public surface.

## 4. Completeness (tests)

- Tests with no assertion, or only `assert!(result.is_ok())` when the criterion
  names a value.
- **Tautological assertions** — the test asserts a value it set itself, or a
  constant it computed rather than measured. If the assertion cannot fail when
  the implementation is wrong, it is not a gate. Ask of every new assertion:
  *what change to the source makes this fail?* If nothing does, it is a finding.
- **Assertions that never run.** An assertion inside `if let ... { assert!(..)
  }` or `if value.is_object() { assert_eq!(..) }` with no `else` failing the
  test is skipped, not passed, when the guard goes false — and deleting the
  feature it checks turns it green. Distinct from a tautological assertion:
  that one cannot fail, this one does not execute. Every guarded assertion
  needs an `else` that fails, or the guard's condition asserted first.
- `#[ignore]` without a named lane or tracked reason.
- `#[should_panic]` used to accept a panic that should have been an error.
- Tests whose dependencies are all doubles, so no real code path runs.

## 5. Integrity

- No new `#[allow(...)]` without a comment giving the reason, and never
  crate-wide to silence one site.
- No weakening of `-D warnings`, `[workspace.lints]`, `deny.toml`, or a
  coverage threshold to make a change pass.
- No advisory waiver without an expiry and a stated review date, where the repo
  requires it.
- No `unsafe` added to a crate carrying `#![forbid(unsafe_code)]`; where
  `unsafe` is permitted, every block carries a `// SAFETY:` comment justifying
  each invariant.

## 6. Panic surface

- `unwrap()`, `expect()`, `panic!`, slicing `&v[i..j]`, and indexing `v[i]` in
  library or daemon code. In tests they are fine; on a request path they turn a
  bad input into a downed worker.
- `unwrap()` on a lock — decide deliberately between propagating poisoning and
  `expect("...")` with a message naming the invariant.
- Integer arithmetic that can overflow in release (`+`, `*` on sizes/counts)
  where `checked_`/`saturating_` was meant.
- Recursion or unbounded loops reachable from untrusted input.

## 7. Numeric and boundary conversions

- Bare `as` casts crossing a wire, database, or FFI boundary — require
  `try_from` with an explicit fallback, or a widening type for the arithmetic.
- `usize`↔`u32`/`i32`/`i64` conversions on 32-bit targets.
- Truncation in timestamps, counts, and offsets that persist.

## 8. Async and blocking

- Blocking calls (filesystem, `std::sync::Mutex` under contention, CPU loops,
  `block_on`) on an async worker thread.
- A lock or `RefCell` borrow held across `.await`.
- `block_on` inside a runtime — check the sanctioned bridge the repo uses.
- Spawned tasks whose `JoinHandle` is dropped, so failures vanish.
- Cancellation: what happens if the future is dropped mid-write?

## 9. State, concurrency, and lifecycle

- Atomic `Ordering` chosen deliberately; a counter published *after* the work
  it describes, not before.
- Lost wakeups: `try_send` on a full one-slot channel, `Condvar` without a
  predicate loop, notify-then-check races.
- Use-after-close: sending on a channel whose receiver has shut down; double
  shutdown; `Drop` impls that block or panic.
- TOCTOU on files and endpoints — bind or open to claim, don't probe then act.
- Shared state initialized after something can already read it.

## 10. Untrusted input and wire contracts

- `#[serde(deny_unknown_fields)]` on every externally supplied payload.
- Hand-written `Deserialize`/decoders: every field they parse needs a test,
  including the malformed-type refusal. A hand-written decoder is where a
  new field silently goes untested.
- New wire fields are backward compatible (`#[serde(default)]`, absent =
  previous behavior) **and** that default is asserted by a test.
- Protocol/version skew handled as a refusal, not a coercion.
- Path inputs validated against traversal and symlink escape before use.
- **The type is the schema — on the way out, too.** A payload the code emits
  must be a `Serialize` struct or enum, not a `serde_json::Value` built by
  inserting string keys across several branches. An untyped emitted format has
  no field list to review, no compiler check that a branch populated a required
  field, and its only contract is whichever hand-written test happened to
  assert a key. Same rule for the code/tag fields inside it: they come from the
  repo's catalogued enum, not from literals invented at the call site and
  documented nowhere.

## 11. Resource bounds

- Every loop that follows a server-supplied cursor, retry, or redirect has a
  ceiling and a stated error at the ceiling — especially one running inside a
  connection worker, where a non-terminating loop hangs a connection instead of
  failing it.
- Channels, buffers, and accumulators bounded; whole-result collectors
  (`collect()` over a stream) sized against the largest real input.
- `Instant` for durations, never `SystemTime`/wall clock.
- Anything spawned per request has a limit.

## 12. Gates — run them, don't assume them

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p <touched crates>            # plus the repo's DB/integration lane
cargo deny check                           # if deny.toml exists
```

**Diff the CI workflow files before reviewing the code.** A change that
removes a lane, narrows one (`--all-features` replacing a
`--no-default-features` clippy or test lane), adds `continue-on-error`, or
drops a matrix entry has weakened the gate for everything already merged — and
it does that while its own suite reports green. Report it at the severity of
the coverage it removed, independently of the code change, and check it against
what the change's own requirements promise: a PR whose acceptance criterion
says minimal builds retain behaviour cannot be the PR that stops testing them.

Run the repo's own scripts (`scripts/*.sh`) where they exist rather than
approximating them. Never pipe a test run through `tail`/`head` in a way that
discards the exit code — a failed suite reads as green.

## Output

Findings as a table, most severe first, each with `file:line`, a severity
(Critical/High/Medium/Low), and a concrete failure scenario — inputs or a
sequence that produces the wrong result. A finding with no scenario is a
style note; label it as such. Every `file:line` is against the exact commit
sha you reviewed; if the branch moves before the finding is resolved, that
`file:line` needs re-verifying against the new sha rather than being reused.

When run as `code-review`'s Rust lane, fold this table into that skill's one
`SpecReview` artifact under `code-review`'s own `SR-NNN` id — Rust findings do
not get a second id or a second file for the same PR; `code-review` and
`rust-review` are one artifact, one id (`gap-analysis` and any `spec-review`
sub-analysis run for the same PR still get their own id each — one `SpecReview`
document per analysis skill). When run standalone, follow
the `code-review` skill's Output section for the artifact itself: allocate the id
by checking every existing `SR-` id in the repo first (including quoted ids,
`id: "SR-014"`, which a naive grep misses), record full scope (crates and
files examined, including ones with no findings, and the reviewed sha), and
never write a placeholder row per clean check — one document-level
`No findings (placeholder)` row at most (`low` severity; there is no `info`
level in the schema), with the "clean" conclusion stated in `## Verdict`.

Each finding eventually gets an explicit outcome — `fixed <sha>`,
`rejected: <reason>`, `deferred: <reason>`, or `accepted-no-change` — recorded
in a separate `## Dispositions` section once the fix round lands, never by
editing the original finding row. The finding's wording is what was found;
the disposition is a separate, later fact about what happened to it.

When the host provides a findings-reporting tool, use it. Otherwise emit the
table, plus the gate results verbatim.
