# Wire and persistence boundaries

Read this when defining errors, serializing or deserializing data, exposing
IPC commands, accepting paths, or converting values stored in a database or
sent over a wire. These are Agent IX defaults across crates, not instructions
for a named repository.

## Error envelopes

- Each crate that surfaces errors defines a `Copy` code enum and a
  `thiserror` envelope. The enum derives `Clone, Copy` and provides
  `as_str(self) -> &'static str`, `all() -> &'static [Self]`, and
  `from_code(&str) -> Option<Self>`. Keep its stable spellings in a catalog
  owned by the crate; never rename or reuse a code.
- Use an envelope with a typed code, immutable `Box<str>` message,
  deterministic `BTreeMap<String, String>` context, and typed provenance
  when needed. Use a consuming `with_context(mut self, ...) -> Self` builder
  where that is the crate's construction pattern. Keep a generated type-export
  derive when clients consume the wire shape.
- Give each condition a caller must distinguish a distinct code or error
  variant with typed fields. Never turn message literals into hidden
  discriminants or branch on the displayed text. Carry causes and context
  through the error rather than stringify and re-parse them.
- Do not expose `Box<dyn Error>`, `String` errors, or `anyhow` as the public
  error contract of a library. Use `Option` only for expected absence whose
  cause the caller does not need to distinguish.

## Input and output contracts

- Add `#[serde(deny_unknown_fields)]` to deserialized payloads, together
  with the wire's declared naming convention such as
  `#[serde(rename_all = "snake_case")]`. A caller's misspelled field must
  not silently disappear. Test acceptance and malformed-type refusal for
  each field of a hand-written decoder.
- Store a reference to secret material in input types, not the material
  itself, so an untrusted payload cannot deserialize a secret into that
  type. Validate paths against traversal and symlink escape before use.
- Give validated composite references constructors that identify the trust
  source, such as `trusted(...)` and `from_untrusted_client(...)`. The
  client-origin constructor validates and returns `Result`; a caller must
  not reach a trusted constructor with unvalidated input.
- Emit a named `Serialize` struct or enum for a wire payload. Do not build
  the schema by inserting string keys into `serde_json::Value` in separate
  branches. Obtain protocol codes and tags from their typed catalog rather
  than literals invented at a call site.
- When adding a wire field, provide the established absent-field behavior
  with `#[serde(default)]` where appropriate and assert that behavior in a
  test. Refuse incompatible protocol or version skew explicitly rather
  than coercing it to a different valid value.

## IPC, when present

- Return `Result<_, AppError>` from IPC commands, with one serde-derived
  `{ code, message }` envelope for the command surface. Do not introduce
  per-command `String` errors or ad-hoc per-view error objects.
- Decode IPC success and failure through one shared client helper. New
  client views use that helper rather than their own error branching.

## Persistence and numeric conversion

- Never use a bare narrowing `as` cast across a persistence, wire, or FFI
  boundary. Use `TryFrom` and a documented refusal or saturation policy;
  widen before arithmetic that could overflow. A saturation must be a
  deliberate contract, not an accidental way to hide invalid input.
- Check `usize` conversions on 32-bit targets and signed conversions of
  large counts, timestamps, offsets, and budgets. A large unsigned value
  cast to a signed storage type must not wrap negative.

```rust
let count = i32::try_from(len).unwrap_or(i32::MAX); // Only when clamping is the contract.
let budget = i128::from(used) + i128::from(requested);
let stored = i64::try_from(budget).unwrap_or(i64::MAX);
```
