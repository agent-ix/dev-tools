# Detailed Rust review checks

Open the sections relevant to the change after applying the core checklist in
[rust-review](../SKILL.md). The linked `rust-style` references own the rules;
these checks help establish whether code and tests actually satisfy them.

## API and source completeness

- For a new public item, identify its intended consumer and the contract it
  exposes. Look for a caller in other crates or a stated cross-crate use, not
  only a local caller. Check whether the documentation and tests support that
  contract. See [API design](../../rust-style/references/api-design.md).
- Compare every changed public doc comment with the implementation, including
  error conditions and guarantees. A comment claiming typed classification
  while code derives a kind from message text is misleading even if the
  current tests pass.
- Check whether callers distinguish errors by stable code or typed variant,
  or instead parse message prose. If several conditions share one variant,
  find the caller decision that loses information. See
  [wire and persistence](../../rust-style/references/wire-and-persistence.md).
- Count the places that must change together when adding a variant, version,
  grammar production, protocol field, or rule. Name the missed edit that
  would survive compilation and tests. A fallback that maps an unknown
  variant to a different valid value can silently corrupt a codec. See
  [implementation](../../rust-style/references/implementation.md).
- Inspect `TODO`, `FIXME`, and `XXX` comments against the current change's
  promised behavior. Flag unfinished required work, not a tracked future
  task. Inspect no-op trait methods and placeholder return values on a
  reachable path.

## Tests and evidence

- For each changed assertion, identify a source mutation that would make it
  fail. A test asserting a value it supplied or computed itself may prove
  nothing about production code. An assertion inside `if let` without a
  failing `else` may not execute at all. See
  [testing](../../rust-style/references/testing.md).
- Follow the seam into the real unit under test. A double of that exact unit
  can make a test pass while production behavior is absent. Check whether
  test-only branches or feature flags bypass production logic.
- Check that ignored and service-backed tests have the repository's intended
  lane, and that the relevant lane ran. For a traced repository, check each
  new test's criterion IDs and the computed matrix rather than inferring
  coverage from a test name.
- Inspect clock sleeps, wall-clock thresholds, fixed ports, ambient
  environment, and unordered iteration for tests that can pass or fail for
  reasons unrelated to the behavior under test.

## Safety, boundaries, and lifecycle

- Trace `unwrap`, `expect`, indexing, slicing, arithmetic, and recursion from
  caller-controlled input. State the input that reaches the panic or bound.
  Check whether `catch_unwind` hides an ordinary error and whether poisoned
  mutex state is reused. See
  [safety and concurrency](../../rust-style/references/safety-concurrency.md).
- Inspect each new unsafe block's preconditions and its surrounding safe
  abstraction. Check whether a new allowance, weakened lint, CI change, or
  advisory waiver narrows an enforced gate.
- Follow a narrowing numeric conversion to the persisted or transmitted
  value. Verify whether refusal or saturation matches the contract. Check
  malformed fields, absent-field defaults, unknown-field handling, version
  skew, path traversal, and symlink escape. See
  [wire and persistence](../../rust-style/references/wire-and-persistence.md).
- Check concurrent state at completion and shutdown: counters published
  before work finishes, full channels, lost wakeups, unobserved task failure,
  cancellation during writes, and double shutdown. Bound retries, cursors,
  redirects, buffers, and request-scoped tasks.
- For Tokio code, inspect blocking calls on async workers, a lock held across
  `.await`, nested runtime entry, and synchronous bridges on a current-thread
  runtime. See [Tokio](../../rust-style/references/tokio.md).
