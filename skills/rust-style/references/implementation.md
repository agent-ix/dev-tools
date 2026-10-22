# Implementation and organization

Read this when implementing Rust modules, loops, matches, imports, macros,
formatting, or naming. Prefer readable code whose relationship to its source
of truth can be checked directly.

## Control flow and data

- Use exhaustive `match` on a closed enum when a new variant must force a
  decision. Never use `_ => ExistingValue` to convert an unknown domain or
  wire value into a valid but wrong value. For non-exhaustive enums, handle
  known variants that matter and make the fallback deliberate.
- Use `if let` or `let else` when only one `Option` or `Result` case matters,
  and `?` for ordinary error propagation. Pattern-match the value instead of
  checking `is_some()` and then calling `unwrap()`.
- Prefer direct iteration over `for i in 0..values.len()` plus indexing when
  no index is needed. Use a short operation such as `any()` or `map()` when it
  names the behavior clearly. Prefer a `for` loop for branching, side effects,
  or error handling that a combinator chain would obscure. Assign a complex
  iterator pipeline to a named variable before using it in a `for` loop;
  avoid `for_each` when an ordinary loop is clearer.
- Use `!collection.is_empty()` rather than `.iter().count() > 0`. Add an
  explicit variable type when `.into()`, `.parse()`, or a complex iterator
  pipeline makes the result hard to infer from the code. Prefer the type on
  the binding over an unnecessary turbofish.
- Keep a fact in one place: use a typed table, an exhaustive mapping, or a
  generated representation when a grammar, schema, protocol, state machine,
  or catalog would otherwise be duplicated in hand-maintained branches.
  Preserve genuinely sequential application logic as code; report the
  concrete drift risk when proposing a declarative representation.

## Modules, imports, and visibility

- Give a leaf module with one primary domain item a name that points to that
  item. Use parent modules mainly to declare and re-export the public
  surface; a small amount of glue code is fine. A module that only
  re-exports an intentional API is useful, not inherently incomplete.
- Avoid glob imports by default. `use super::*` in a local test module,
  an enum's variants in an enum-focused scope, and an intentional public
  re-export are reasonable exceptions. Keep a `use` group to one module when
  practical. Import a trait in the smallest scope that needs its methods
  unless use is widespread through the file.
- Qualify a free function from another module when doing so makes its origin
  clear. Import type names for ordinary unqualified use; retain a module path
  when names conflict or grouping clarifies the domain. Follow the
  repository's rustfmt and import configuration when it chooses differently.
- Do not access another crate's `#[doc(hidden)]` items. Do not introduce
  pre-`main()` execution through constructors or similar hooks without an
  explicit project requirement.

## Formatting, macros, and names

- Run the repository's formatter. Use `#[rustfmt::skip]` only for generated
  code or a specific alignment that improves readability, with a reason.
  Keep invisible characters escaped in source literals where possible.
- Prefer functions, types, traits, and modules to code generation. When
  generation is justified, prefer `macro_rules!` for a simple Rust AST
  transformation before a procedural macro. Check how a macro affects
  search, IDE navigation, refactoring, and comprehension across the codebase.
- Use `format!` or `write!` for multi-part string construction and
  substitutions on a literal template rather than repeated concatenation
  or an ad-hoc templating syntax.
- Follow conventional `as_`, `to_`, `into_`, `new`, and `try_new` names.
  Give a long-lived lifetime or generic parameter a meaningful name;
  avoid project-private abbreviations in a public API.
- Put the repository's SPDX and copyright header on every source file,
  using its own license and holder rather than a hard-coded example.
