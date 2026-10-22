# Safety and concurrency

Read this when code can panic, uses unsafe or unstable features, shares
mutable state, starts work that can outlive its caller, or handles untrusted
input. Read [Tokio](tokio.md) as well when the runtime is Tokio.

## Unsafe and language features

- Put `#![forbid(unsafe_code)]` on safe crate roots and opt crates into the
  workspace lint policy with `[lints] workspace = true`. Define workspace
  safety and Clippy lints in the root manifest. A crate that genuinely needs
  unsafe code documents its exception in its own manifest and safety policy;
  do not weaken the workspace policy to silence a single use.
- Before adding unsafe code, identify why equivalent safe code cannot meet
  the need. Isolate unsafe operations behind a safe abstraction and away
  from business logic. Each unsafe block needs a `// SAFETY:` comment that
  proves its preconditions; a performance justification needs measurements.
  Audit the full unsafe surface and require `SAFETY:` comment coverage for
  every block, not only a sample.
- Do not use unstable language or library features without a documented
  project requirement and toolchain policy.

## Errors and panic

- Use `Option` for expected absence whose cause needs no explanation and
  `Result` for a recoverable failure the caller may need to distinguish.
  A panic indicates a bug or violated invariant, not malformed or
  caller-controlled input. Follow a repository's stronger no-panic policy.
- Pattern-match a value rather than check a condition and then `unwrap()`.
  When a panic for an invariant is genuinely necessary and permitted, use
  `expect` with the invariant rather than an unexplained `unwrap`. Document
  public panic conditions. In tests, panics may serve as assertions.
- Check indexing, slicing, arithmetic, and recursion reachable from
  untrusted input. Use `get`, checked arithmetic, bounds, or an explicit
  refusal when the input can violate a precondition. Bounded loops and
  recursion must have a stated outcome at the limit.
- Do not use `panic::catch_unwind` to make ordinary behavior correct or to
  replace error handling. A documented defense-in-depth boundary is possible
  only when the process uses `panic=unwind`; review what happens under
  `panic=abort` and after a panic leaves state partially changed.

## Locks, tasks, and lifecycle

- Decide whether lock poisoning propagates or violates an invariant. Do not
  recover by extracting the inner value of a poisoned `std::sync::Mutex`;
  that value may be inconsistent. If a lock failure is an invariant panic,
  use an `expect` message naming the invariant.
- Do not hold a blocking lock or a `RefCell` borrow across `.await`.
  Keep blocking filesystem or CPU work off async worker threads. Use the
  runtime-specific bridge described by the repository; for Tokio read
  [Tokio](tokio.md).
- Choose atomic ordering for the relationship it must establish. Publish a
  completion counter after the work it describes. Guard condition variables
  with a predicate loop; consider full-channel, lost-wakeup, and shutdown
  races. Do not assume `try_send` succeeded.
- Observe or deliberately detach spawned tasks; do not drop a join handle
  when its failure matters. Define the effect of cancellation during writes
  and of repeated shutdown. Avoid blocking or panicking in `Drop`.
- Claim a file or endpoint by opening or binding it instead of checking and
  then acting. Initialize shared state before readers can access it.
- Bound request-scoped tasks, retry loops, redirects, cursors, channels,
  buffers, and accumulators. Use `Instant` for elapsed duration rather than
  wall-clock time.
