# Status and internals

Docs describe the language and the tool. They do not describe the build, the
repository or the process that made them. The reader cannot use any of that.

## Where status goes

Release state, build state and "does not run yet" belong in one place: the
generated status page for each section ([page-types.md](page-types.md)). A page for
a feature that does not run yet carries a single small badge. Prose does not
carry status (rule: status-in-prose).

Bad:

```text
The exact engine is mostly specified and not yet built. Each page states which
part is built, which is specified only, and what a reader can run. The section
badge is `preview` for that reason.
```

Good:

> The exact engine computes the probability over the whole model.

The status page says the engine is in preview.

Bad:

```text
No `quire-spec` command evaluates a clause over a trace, so the page badge is `preview`.
```

Good: delete the sentence. The status page lists what runs (rules:
no-command-runs, page-self-reference).

## Internals that stay out of prose

<!-- vale AgentIX.CrateNameInProse = NO -->
<!-- vale AgentIX.SourcePathInProse = NO -->
- Requirement, ticket and decision ids such as `FR-43`, `TC-971`, `STD-123`,
  `V1-TEMP-021` or `ADR-7` (rule: requirement-id-in-prose). The pattern is any
  uppercase key with a number. Names such as `SHA-256` and `UTF-8` are exceptions;
  extend the exception list in `RequirementIdInProse.yml` for your repository.
- Issue and pull request references such as `repo#123` (rule: issue-ref-in-prose).
- Rust paths such as `TemporalProfile::ALL` (rule: rust-path-in-prose).
- Crate and module names such as `qsl-semantics` (rule: crate-name-in-prose). Edit
  the prefix list in `scripts/gen_vale_scripts.py` and regenerate the scripts.
- Source paths such as `qsl-semantics/src/check/profile.rs` (rule:
  source-path-in-prose).
- Test names and pointers to tests (rule: test-name-in-prose). An example needs no
  pointer to a test.
- Branch state such as `on main` or `unreleased` (rule: on-main-branch).
- Version pins such as `documents Quoin 0.29.1` (rule: version-pin). State a
  version in the install line, once.
- Maintainer process: `CLAUDE.md`, `AGENTS.md`, `Hosted CI is manual-only` and gate
  tooling (rule: maintainer-process).

<!-- vale AgentIX.CrateNameInProse = YES -->
<!-- vale AgentIX.SourcePathInProse = YES -->

Bad:

```text
The compiled-artifact evaluator of FR-043 reads a trace of positions.
```

Good:

> The evaluator reads a trace of positions.

Bad:

```text
The catalog in `qsl-semantics/src/check/profile.rs` has five, and FR-234 admits the fifth.
```

Good:

> There are five profiles.

## Banned headings

These headings announce bookkeeping (rule: banned-heading): `Gaps`, `Known gaps`,
`Tests`, `Status of this page`, `What was verified`, `What runs`, `Related pages` and
`Open questions`. A status page uses the heading "What runs today", which the rule
does not match; it only needs `AgentIX.StatusInProse = NO` for its path in
`.vale.ini`.

Replace "Related pages" with "Next steps". Delete "Tests", "Gaps" and "What was
verified"; the reader needs one example that works (rule: no-test-section).

## Where internals belong instead

The table lists each kind of internal detail and where it lives.

| Item | Home |
| --- | --- |
| Requirement and test ids | The spec and its matrix |
| Crate and module names | The repository README and API docs |
| Issue numbers | The tracker |
| Release state | The status page and the changelog |
| Process | CONTRIBUTING |
