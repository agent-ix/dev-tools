# API design

Read this when defining a public API, cross-crate interface, domain type,
constructor, trait, or documented module surface. In application code, use the
same principles without introducing abstraction solely for future callers.

## Inputs, outputs, and ownership

- Accept the most flexible immutable borrow that supplies the required data:
  `&T` instead of `&Box<T>`, `&[T]` instead of `&Vec<T>`, `&str` instead of
  `&String`, and `Option<&T>` instead of `&Option<T>`. Borrowing a wrapper is
  appropriate only when the wrapper itself is part of the operation.
- Avoid a `String` or `Vec<T>` input that is immediately borrowed. Prefer
  borrowing over a deep `clone()` introduced only to satisfy the borrow
  checker. Clone an `Rc` or `Arc` when shared ownership is deliberate; it is
  not a deep copy. Do not introduce shared ownership where borrowing or unique
  ownership remains clear.
- For reusable library functions that only traverse input, consider
  `IntoIterator`; for returned data, consider an iterator when callers can
  consume it without a forced allocation. Use slices or owned collections
  when indexing, natural construction, lifetime simplicity, or application
  usage makes them clearer.
- Use `impl Into<T>` where accepting several common owned forms materially
  improves a library API. Account for generic code size and inference costs;
  do not use it by default for application functions. Use `impl AsRef<Path>`
  when callers genuinely supply several path-bearing types.

## Types and public evolution

- Use newtypes for IDs, scopes, handles, and other values with distinct
  semantics or validation. Prefer named fields to tuple positions when the
  fields could be confused. Use `Option<T>` for true absence, not a sentinel;
  use `NonZeroUsize` or a validated type when it makes an invariant explicit.
- Use an enum rather than a `bool` parameter when a call such as `run(true)`
  does not reveal the choice. A boolean is fine when its purpose is obvious.
- Keep `pub` limited to the intended consumer surface and use `pub(crate)` for
  internal APIs. An externally intended public API need not have a caller in
  its defining crate. A root or parent module may re-export a deliberate API.
- For a public enum used across team boundaries and expected to grow, use
  `#[non_exhaustive]` unless the set is deliberately closed or callers can be
  updated together. For a public-field struct expected to grow, consider the
  same attribute; retain literal or struct-update construction when that is
  part of the intended API. Match closed enums exhaustively. When matching a
  non-exhaustive enum, spell out known variants whose behavior matters and
  make the future-variant fallback intentional.
- Use `#[must_use]` when ignoring a return value makes the call ineffective or
  loses critical information. Do not add a redundant function annotation
  when the result type already carries it. Publish a `const fn` only when its
  constness can remain part of the API or a migration is acceptable.
- If the type offers `len()`, offer `is_empty()`. Avoid operator overloads
  whose meaning is unrelated to the operator's mathematical or logical role.
  Treat another crate's `#[doc(hidden)]` item as private.

## Traits and construction

- Implement a standard trait such as `Display`, `FromStr`, or `Iterator` when
  an operation has that trait's meaning. An inherent method may coexist for
  discoverability. Derive a trait rather than hand-writing an equivalent impl.
- Add a custom trait for generic programming, dispatch, or a genuine extension
  seam, not merely to group methods. Prefer a generic library parameter over
  `dyn Trait` when dynamic selection or type erasure is not needed and code
  size is acceptable. Name an extension trait with an `Ext` suffix.
- Implement `Clone` for ordinary value types when copying the value makes
  sense. Implement `Copy` when the type can reasonably remain copyable. Add
  `PartialEq`, `Eq`, `Hash`, `PartialOrd`, or `Ord` only when their semantics
  hold; map keys commonly need `Eq` and `Hash`, and ordering requires a total
  order when implementing `Ord`.
- Implement `Default` if a type has `new()` with a meaningful default state;
  options structs with public fields should support struct update from
  `Default`. Keep `new()` or a named constructor where it aids discovery. Use
  `try_new()` for fallible construction. Pair an important `From` impl with an
  explicit constructor such as `from_foo()` when users need to find it.
- Use fluent consuming builders, `fn with_x(mut self, value: T) -> Self`, for
  envelopes and option builders where the repository uses that pattern.

## Names and documentation

- Follow `as_`, `to_`, and `into_` receiver conventions. Use `foo()` for a
  getter, `foo_mut()` for mutable access, `take_foo()` to clear and return,
  `set_foo()` to set, and `replace_foo()` to return the old value. A
  `with_foo()` closure accessor can contain access to an interior-mutability
  guard without exposing that guard; distinguish it by signature from a
  consuming `with_foo(value)` builder.
- Give type-level lifetimes descriptive names when their relationships are
  not obvious; `'_` and short local lifetimes are fine when clear. Name
  generic parameters for their role. Avoid unfamiliar abbreviations.
- Write a `///` comment on every public item and a `//!` module header.
  Document behavior and guarantees rather than repeating the identifier.
  Describe public panic conditions caused by malformed input and the
  invariants of unsafe APIs. Include enough examples to show how the public
  surface works; not every item needs its own example. Use Markdown,
  capitalization, and punctuation in doc comments. Keep implementation
  TODOs in ordinary `//` comments. Cite governing requirement IDs in
  module headers where the repository tracks them.
