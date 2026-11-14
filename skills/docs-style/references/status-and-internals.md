# Status and internals

Docs describe the language and the tool. They do not describe the build, the
repository or the process that made them. The reader cannot use any of that.

## Where status goes

Release state, build state and "does not run yet" belong in one place: the
generated status page for each section ([page-types.md](page-types.md)). A page
for a feature that does not run yet carries a single small badge. Prose does
not carry status (rule: status-in-prose).

Bad: "The exact engine is mostly specified and not yet built. Each page states
which part is built, which is specified only, and what a reader can run. The
section badge is `preview` for that reason."
Good: "The exact engine computes the probability over the whole model." (The
status page says it is in preview.)

Bad: "No `quire-spec` command evaluates a clause over a trace, so the page badge
is `preview`." (rules: no-command-runs, page-self-reference)
Good: (Delete. The status page lists what runs.)

## Internals that stay out of prose

- Requirement and decision ids: FR-, NFR-, StR-, TC-, AC-, ADR-, SR-,
  Plan- and Task- ids (rule: requirement-id-in-prose). They live in the spec.
- Issue and PR references such as `repo#123` (rule: issue-ref-in-prose).
- Rust paths such as `TemporalProfile::ALL` (rule: rust-path-in-prose).
- Crate and module names such as `qsl-semantics` (rule: crate-name-in-prose).
  Edit the prefix list in the Vale script for your repository.
- Source paths such as `qsl-semantics/src/check/profile.rs` (rule:
  source-path-in-prose).
- Test names and pointers to tests (rule: test-name-in-prose). The example
  stands on its own.
- Branch state: "on main", "unreleased" (rule: on-main-branch).
- Version pins: "documents Quoin 0.29.1" (rule: version-pin). State a version in
  the install line, once.
- Maintainer process: CLAUDE.md, AGENTS.md, "Hosted CI is manual-only", gate
  tooling (rule: maintainer-process).

Bad: "The compiled-artifact evaluator of FR-043 reads a trace of positions."
Good: "The evaluator reads a trace of positions."

Bad: "The catalog in `qsl-semantics/src/check/profile.rs` has five, and FR-234
admits the fifth."
Good: "There are five profiles."

## Banned headings

These headings announce bookkeeping (rule: banned-heading): Gaps, Tests, Status
of this page, What was verified, What runs, Related pages. A status page may use
"What runs today"; turn the Vale rule off for those paths in `.vale.ini`.

Replace "Related pages" with "Next steps". Delete "Tests", "Gaps" and "What was
verified"; the reader needs one example that works (rule: no-test-section).

## Where internals belong instead

| Item | Home |
| --- | --- |
| Requirement and test ids | The spec and its matrix |
| Crate and module names | The repository README and API docs |
| Issue numbers | The tracker |
| Release state | The status page and the changelog |
| Process | CONTRIBUTING |
