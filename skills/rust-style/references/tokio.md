# Tokio and blocking boundaries

Read this when Rust code uses Tokio tasks or runtimes, blocks from async code,
or calls async code through a synchronous trait or FFI boundary.

## Blocking work from async code

- Do not introduce blocking work into an async path when an async operation
  can perform the same work. Send an unavoidable
  filesystem, CPU, or FFI blocking operation to `tokio::task::spawn_blocking`
  when an async caller can await its result. Bound the amount of CPU work
  spawned concurrently; Tokio's blocking pool is not a CPU-work limiter.
- `tokio::task::block_in_place` is a synchronous bridge for a multithread
  runtime. It cannot run inside a current-thread runtime; do not replace it
  with a direct blocking call on that runtime's sole worker. Use it only when
  the caller cannot become async and the behavior of suspending the current
  task is acceptable.
- If code may run with or without a runtime, inspect
  `Handle::try_current()` rather than call `Handle::current()` unconditionally.
  `Handle::current()` panics when no runtime context is entered.

## Sync-over-async code

- Do not call `Handle::block_on` from within an async context: it panics.
  Re-entry from `block_in_place` is possible only on a multithread runtime
  and needs an explicit contract for cancellation and task suspension.
- A `Handle` to a current-thread runtime does not itself drive that
  runtime's I/O and timers. Do not use it as a generic synchronous bridge.
  Keep an end-to-end async API, or move the entire synchronous bridge to a
  dedicated thread that owns and drives a separate runtime with
  `Runtime::block_on`. Do not synchronously wait on that bridge from the
  current-thread runtime's sole worker.
- A lazily created shared runtime may serve callers outside any Tokio
  runtime, but must define ownership, shutdown, and whether it is called
  recursively. Never call `Runtime::block_on` from an async context.
- Preserve `JoinHandle` results when task failure matters. Define what a
  dropped or cancelled future does during a write and how partial state is
  recovered or refused.
