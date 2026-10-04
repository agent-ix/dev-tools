---
name: rust-style
description: Default Rust code-style idioms — error envelopes with stable codes, safety lints, async/blocking bridges, persistence-safe integer conversions, untrusted-input hardening, typed IPC, domain-identity newtypes, and the `Trace:` criterion-binding test convention. Use when writing or reviewing Rust in a repo that documents no idioms of its own.
---

# Rust Style

Default idioms for a Rust codebase. This skill teaches the idioms; the *enforced*
gates (forbid-unsafe, clippy `-D warnings`, cargo-deny/audit, unsafe-comment audit,
a PR tier that finishes in minutes) belong in the repo's own CI config.

## Error Envelopes

Each crate that surfaces errors defines a `Copy` code enum plus an envelope struct:

- Code enum derives `Clone, Copy` and exposes `as_str(self) -> &'static str`,
  `all() -> &'static [Self]`, and `from_code(&str) -> Option<Self>`.
- Envelope struct derives `thiserror::Error` with `#[error("{code}: {message}")]` and holds:

```rust
pub struct CoreError {
    code: CoreErrorCode,
    message: Box<str>,
    context: BTreeMap<String, String>,
    provenance: Option<Box<Provenance>>,
}
```

- Codes are **stable — never renamed or reused**. Keep them in a catalog file the crate owns.
- Derive `thiserror` for the error, and keep whatever type-export derive the repo uses
  (e.g. specta's `Type`) so the wire shape stays generated rather than hand-written.
- Not `Box<dyn Error>` on a public API, not `String` errors, not `anyhow` in a library.

## Safety Lints

- `#![forbid(unsafe_code)]` on crate roots.
- Crates opt into the shared policy with `[lints] workspace = true`; the workspace root
  defines `[workspace.lints.rust] unsafe_code = "forbid"` and `[workspace.lints.clippy]`.
- If one crate genuinely needs `unsafe` (an FFI or GUI-toolkit shim), it overrides
  `[lints.rust] unsafe_code = "warn"` in its own `Cargo.toml`, and a script audits for
  100% `// SAFETY:` comment coverage. That exception is documented and singular — do not
  add a second one.

## Async / Blocking Boundary

Never block a tokio worker. Two sanctioned bridges:

- **Flavor-aware `block_in_place`** for CPU- or FFI-blocking calls. `block_in_place`
  panics on a current-thread runtime, so check the flavor first:

```rust
match Handle::current().runtime_flavor() {
    RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| blocking_call()),
    _ => blocking_call(),
}
```

- **A shared `OnceLock` runtime** for sync-over-async trait impls: a `shared_executor()`
  that reuses a live `Handle` when one is present and otherwise falls back to a lazily
  created `Runtime`.

## Persistence-Boundary Integer Conversions

Never a bare `as` cast across a persistence or wire boundary. Use `try_from` with a
saturating fallback, or widen for arithmetic that could wrap:

```rust
let count = i32::try_from(len).unwrap_or(i32::MAX);      // saturate, don't wrap
let budget = i128::from(used) + i128::from(requested);   // widen before the math
let stored = i64::try_from(budget).unwrap_or(i64::MAX);
```

A large `u64` cast with `as i64` goes negative silently and persists that way.

## Untrusted Input

- Every deserialized payload carries `#[serde(deny_unknown_fields)]`, combined with
  `#[serde(rename_all = "snake_case")]` (or the repo's wire convention) for shaping.
- **Structural secret exclusion**: store a key *reference* only, never key material, so
  secrets cannot deserialize into a payload in the first place.

## Typed IPC

- Every IPC command returns `Result<_, AppError>`, where `AppError { code: String,
  message: String }` is the single serde-derived envelope for the whole command surface.
  Do not reintroduce per-command `String` errors or per-view ad-hoc error shapes.
- The client unwraps through one shared helper (`unwrapIpc` / `IpcError` / `IpcResult<T>`).
  New views consume that helper, not their own error branching.

## Domain Identity

- Validated composite-ref types get **named trust-boundary constructors**:

```rust
impl ActorRef {
    pub fn trusted(/* … */) -> Self { /* … */ }
    pub fn from_untrusted_client(/* … */) -> Result<Self, CoreError> { /* … */ }
}
```

  The constructor name states the trust source; client-origin refs are always the
  fallible path.
- Prefer id newtypes for leaf ids over raw `String`/`Uuid`.

## Testing

- Name tests for the behaviour they check; do not add a `tc_NNN_` prefix to new tests
  (existing ones stay as they are). A test binds to acceptance criteria only through a
  `/// Trace:` doc line listing the criterion ids it asserts, comma separated
  (`/// Trace: FR-001-AC-2, NFR-003-AC-1`), or `#[trace("FR-001-AC-2")]` where the repo
  uses the attribute. The keyword is `Trace:` —
  a `Tracing:` line binds nothing. Only criterion ids go on it; issue, `Task-` and
  `Plan-` ids go on a sibling `/// Provenance:` line. The Test Matrix is computed from
  these tags by `quire matrix`; do not hand-edit matrix rows.
- Source-inspection tests compile fixtures in via `include_str!` rather than reading
  from disk at runtime.
- Postgres integration tests use a scratch-database fixture and **skip cleanly when
  `DATABASE_URL` is unset**, so the default `cargo test` needs no server. Run them
  against a throwaway server when you want them.
- In-memory repositories used by tests are unconditionally `pub` — no `test-support`
  feature, no `#[cfg(test)]` gate on them.

## Ergonomics & Headers

- Flexible params: `impl Into<String>`, `impl AsRef<Path>`.
- Fluent **consuming** builders that take `mut self` and return `Self` — `with_context`,
  `with_provenance`, `with_target`.
- `Box<str>` for immutable messages; `BTreeMap` for deterministic context ordering;
  `#[serde(rename_all = …)]` for wire shaping.
- A `///` doc on every `pub` item, and a module `//!` header citing the governing
  requirement id where the repo tracks one.
- The repo's license header on **every** file, e.g.:

```rust
// SPDX-License-Identifier: <repository-license>
// Copyright (C) <year> <copyright-holder>
```
