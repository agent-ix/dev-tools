---
name: rust-style
description: Agent IX Rust conventions for every Rust coding and review task. Load this baseline even when the repository has additional Rust guidance; read the linked API, implementation, safety, boundary, testing, and Tokio references for the work at hand.
---

# Rust Style

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

Load this skill before designing, writing, or reviewing Rust. These are Agent IX
defaults for Rust code, not instructions for a particular repository. Read
`AGENTS.md`, `CLAUDE.md`, a repository Rust skill, and its lint and formatter
configuration too. More specific repository instructions take precedence. A
documented rule remains authoritative even if existing code violates it. Treat
the rule as superseded only when a newer explicit project authority changes it;
report the conflict when those authorities disagree.

## Core rules to keep in context

- Borrow immutable inputs as `&str`, `&Path`, `&[T]`, or `&T` where possible.
  Prefer unique ownership and borrowing over unnecessary deep clones or shared
  ownership. Use identity newtypes when two IDs could be confused.
- Use `Option` for genuine absence, `Result` for recoverable failures, and a
  distinct typed condition for each failure a caller must distinguish. Each
  crate that surfaces errors uses a stable `Copy` code enum and a `thiserror`
  envelope; never branch on human-readable error text.
- Keep public APIs deliberate: document every `pub` item, avoid accidental
  visibility, use standard traits when they fit, and consider compatibility
  before exposing enum variants, struct fields, or a `const fn`.
- Prefer direct, auditable control flow. Match closed enums exhaustively; do
  not map an unknown variant to a valid but wrong value. Use an iterator
  operation or a `for` loop according to which makes the behavior clearer.
- Return an error for bad caller-controlled data. Do not let malformed input
  trigger `unwrap`, indexing, slicing, or arithmetic panics. An invariant
  panic is only appropriate when the repository permits it and the invariant
  is explicit. Prefer pattern matching to check-then-`unwrap`.
- Default to `#![forbid(unsafe_code)]` and workspace safety lints. An allowed
  unsafe operation requires a `SAFETY:` comment, an explanation of why safe
  code is insufficient, and an audit of the surrounding abstraction.
- Use typed wire payloads and reject unknown fields on every deserialized
  payload. Keep secrets out of input types. At wire and persistence
  boundaries, use checked integer conversions rather than a bare narrowing
  `as` cast.
- Write deterministic tests that exercise real behavior. In a repository that
  traces tests to requirements, bind each new test with `/// Trace:` or its
  declared trace attribute; do not hand-edit a Test Matrix. Use the
  repository's required gates and license header.

## Read the applicable detailed rules

- [API design](references/api-design.md): public or cross-crate APIs, types,
  constructors, traits, ownership, and documentation.
- [Implementation](references/implementation.md): modules, imports, naming,
  control flow, formatting, macros, and ordinary Rust code.
- [Safety and concurrency](references/safety-concurrency.md): panic paths,
  unsafe, locks, cancellation, resource bounds, and concurrent state.
- [Wire and persistence](references/wire-and-persistence.md): errors,
  deserialization, IPC, external input, persisted values, and conversions.
- [Testing](references/testing.md): unit and integration tests, doubles,
  trace tags, external services, and deterministic assertions.
- [Tokio](references/tokio.md): Tokio tasks, blocking work, runtime context,
  and sync-over-async bridges. Read this only when Tokio is used.

These rules describe code and review decisions. Enforceable gates such as
Clippy, cargo-deny, and unsafe audits belong in the repository's CI and lint
configuration; follow that configuration when it is stricter.
