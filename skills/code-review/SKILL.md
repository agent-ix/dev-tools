---
name: code-review
description: Review code for Agent-IX Golden Path compliance, focusing on test style, mock boundaries, implementation gaps, and code-test alignment. Dispatches by language — Python inline, Rust via `rust-review`, React via `review-react`.
---

# Code Review

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

Perform the following checks on the codebase.

## Language Dispatch (do this first)

The checks below are written for Python. Detect what the reviewed code
actually is and run the matching lane — transliterating a Python check into
another language produces false findings ("no test classes", "no `pass`
placeholders") that bury the real ones.

| Detected | Lane |
|---|---|
| `Cargo.toml` / `.rs` files in the change | invoke `rust-review` and run it **instead of** Test Standards, Mock Compliance, Completeness, and Edge Case & Logic Review |
| `package.json` with `react` | additionally run the React lane below |
| `pyproject.toml` / `setup.py` / `.py` files | the Python checks below, as written |

Language-independent sections — **Duplication and Vendoring**, **Integrity**, **Spec-Code Faithfulness**,
**Code-Test Alignment**, **Gap Analysis**, **Output** — always apply. Read
"docstring" as "doc comment". Tests bind to acceptance criteria by criterion id through
the tag forms quire declares:

- a `Trace: FR-XXX-AC-N, …` line — a docstring or `#` comment line in Python, a `//`
  comment or JSDoc line in TS/React, `///` in Rust
- the Rust `#[trace("FR-XXX-AC-N")]` attribute
- the Python `@pytest.mark.trace("FR-XXX-AC-N")` marker
- the TS `trace("FR-XXX-AC-N")` marker

Ids in a `Criteria:` list, a Description, or a `Tracing:` line bind nothing. Only test,
bench and fuzz functions bind; a tag on a Storybook story or other non-test code is
documentation only.

A mixed repo runs several lanes; scope each to the files it applies to.

Also load the repo's own conventions before judging style: `CLAUDE.md`,
`AGENTS.md`, and any `.claude/skills/*` describing the language's idioms in
this codebase. A documented repo idiom outranks this skill.

## Applicable assurance context

Before applying the ordinary rubric, look under the reviewed repository's `spec/` tree
for an `AssuranceProfile` whose stated scope applies to the changed code or behavior.
Do not create a profile and do not apply an unrelated one. If no applicable profile
exists, continue with the ordinary review below unchanged.

When a profile applies, read
[references/assurance-context.md](references/assurance-context.md). Use that context to
focus the existing checks and findings; it is not a second checklist. Name the profile
and evaluated source revision/baseline in the review artifact, and state explicitly when
selected architecture, impact, measurement, exception, independence, structural-gap, or
producer-reliance evidence is unavailable. The durable artifact MUST contain an exact
`## Assurance Context` heading whenever a profile applies; discussing the profile only in
the summary, findings, or chat does not satisfy this requirement.

## Duplication and Vendoring (P0 — run this first)

**Vendoring is a CRITICAL P0 finding.** It is an antipattern that will almost always be
rejected in favour of a cleaner design. Report it at `high` severity, which forces a
**FAIL** verdict. Do not soften it to `medium` because the copy is small, well-documented,
provenanced, or temporary — those are the properties of every copy that ever shipped.

No vendoring. No duplicating. No copying code or specs. Find one home for a thing and
depend on it.

**The defect is duplication.** One question, asked of every change: *does this
repo now hold a second statement of something that is defined somewhere else?*
If yes, it is a finding. The thing now exists in two places, so it has two
owners and one of them will drift. That is the whole of it.

**Nothing launders a duplicate.** Not retyping it, translating it to another
language, reformatting it, renaming its identifiers, regenerating it from local
types, splitting it across files, taking only part of it, or paraphrasing a
spec into new prose. Every one of those still leaves two places that must be
edited when the thing changes. Effort spent reworking a duplicate is evidence
of vendoring, not a defence against it.

**Apply it mechanically, not as a judgement.** No case analysis, no weighing of
how small, how documented, or how temporary. The weighing step is where new
justifications get invented.

**Read the lists below as examples, never as a definition.** They are shapes
already seen, written down so they are recognisable on sight. They are not the
boundary of the rule and they are not exhaustive. A copy whose form appears
nowhere in this skill is still a finding, and "it isn't any of the listed
forms" is not a finding's refutation — the next copy is always spelled a way
nobody wrote down yet.

**Two things that are themselves findings:**

- Composing a sentence explaining why *this* copy is acceptable. If a reviewer
  or author is writing that sentence, the finding is already located.
- Reasoning about whether a case falls inside the rule's wording. The rule is
  not a boundary to sit near. Arguing the edge of the definition — rather than
  asking whether the thing now exists in two places — is the tell.

### What counts as vendoring

A copy is a copy however it is spelled. Examples, not an exhaustive list:

- `vendor/`, `third_party/`, `schemas/vendored/`, `corpus/<upstream>-v1/`
- a snapshotted `schema.json`, however its comment describes it
- `include_str!` / `include_bytes!` of another repo's artifact
- a fixture tree copied from another repo, with or without `VENDOR.json` / `PROVENANCE.json`
- a script that refreshes a copy by shelling into a sibling checkout
- anything that reads a sibling checkout by absolute path
- the same requirement, type, or constant re-stated in a second repo's spec
- another repo's values re-stated as a **native const, enum, or type** in code —
  `const TARGETS: [&str; 14]` holding an upstream enum's members is that enum in
  a different syntax
- a duplicate **generated** from local types (schemars, TypeSpec, codegen). "We
  own it because we generate it" describes the mechanism, not the ownership; if
  upstream defines the vocabulary, generating it here makes a second source
- a duplicate paired with a **conformance or drift gate** watching it. That is
  duplication with a detector bolted on — the finding is the duplication
- a **pin that does not pin the artifact**: a submodule tag or lockfile that
  rebuilds to something other than what it names
- a plan that **phases** removal behind a producer shipping something ("the
  copies go once upstream publishes"). Reads as sequencing, works as a reprieve

**Paperwork does not make a copy acceptable.** A `VENDOR.json` recording the upstream
revision, a SHA-256 snapshot guard, and a provenance note documenting the source are how
a copy gets *described* — not how it gets *justified*. A well-provenanced copy still
drifts; it just drifts with a paper trail.

**Visibility is irrelevant.** "The source repo is public, so this is drift rather than a
leak" is not a reason to downgrade the finding. Public or private, the file does not
belong in that repo.

### The tell: a raised limit

A size limit raised to accommodate content is the signature of vendoring. `BINARY_BYTES =
16 * 1_048_576` existed because a fixture embedded a 5.2 MB executable and someone lifted
the ceiling instead of asking why. If a change raises a limit to fit copied content, the
finding is the content, not the limit.

### Duplication inside one repo

Before accepting a new helper, utility, constant, type, or test fixture: **search for an
existing one.** A second implementation of the same idea is the same defect at smaller
scale, and it drifts the same way.

- **fail**: a new helper that duplicates an existing function's behavior under a new name
- **fail**: the same constant, regex, schema fragment, or error string defined twice
- **fail**: a type re-declared in a second module rather than imported
- **fail**: copy-pasted test setup that an existing fixture already provides
- **fail**: two implementations of one rule, where fixing a bug requires editing both

When reviewing a new helper, state explicitly in the finding what you searched for and
where, so the author can see the existing function rather than being told one exists.

### Examples

| Seen in review | Why it is a finding | What to require instead |
|---|---|---|
| `tests/fixtures/modules/` (70 files) copied from another repo, with `VENDOR.json` pinning a commit | A fixture tree that drifts silently from its source; the pin goes stale and nothing checks it | Resolve the fixtures through the existing dependency |
| `schemas/vendored/semantic-core/0.1.0/` **and** `0.2.0/` side by side | Two snapshots of one upstream, nothing reconciling them — drift in its clearest form | One dependency on the published artifact |
| `scripts/vendor-schemas.sh` refreshing a copy from a sibling checkout | The build depends on another repo's working tree; results differ per machine | Depend on the published artifact |
| A schema re-published by a second repo as package data "so consumers can find it" | Creates a second source of truth; consumers then depend on whichever they found first | One publisher; consumers depend on it |
| `quality_lints` reading `../other-repo/file` by absolute path | Vendoring spelled as a path; the result depends on an unrelated checkout | An owned fixture or the published artifact |
| A vendored JS bundle that inlines `zod@4.4.3` | Hides a transitive dependency from `pnpm audit`; advisories never surface | Depend on the package so the audit tooling can see it |
| A new `format_duration()` beside an existing `humanize_duration()` | Two implementations of one rule; a fix to one silently misses the other | Reuse the existing function, or replace it — not both |

Related: **a gate is never deleted to make a diff pass.** When a copy is
removed, a gate that existed only to watch that copy evaporates *because its
subject is gone* — state that explicitly and show nothing is left for it to
assert. Deleting the gate while keeping the copy is the opposite move and is
strictly worse than the copy alone, because it makes the duplication silent.

### A guard over a copy is not evaluated

A duplicate may arrive with a checksum, a provenance manifest, a refresh script
or a conformance gate. **Do not assess whether the guard works.** Its quality is
irrelevant: a guard that cannot detect upstream drift does not make the copy
worse, and a guard that can does not make the copy acceptable. The copy is the
finding either way.

Assessing the guard is itself the trap — it turns the review into a debate
about guard design, which ends in "strengthen the check" rather than "remove
the copy". If a guard is present, note it as evidence that somebody already
reasoned their way to keeping the copy, and report the copy.

### How to find it

Do not rely on reading paths — the last duplicate is always the one nobody
listed. These locate some of them:

```bash
# the same file tracked in two repos
( cd repo-a && git ls-files -z | xargs -0 sha256sum ) > /tmp/a.sums
( cd repo-b && git ls-files -z | xargs -0 sha256sum ) > /tmp/b.sums
join <(sort /tmp/a.sums) <(sort /tmp/b.sums)      # any output is a finding

# one $id claimed by more than one document
grep -rho '"\$id": *"[^"]*"' --include='*.json' . | sort | uniq -c | sort -rn
```

Dependencies are untracked (`node_modules/`, cargo registry and git checkouts,
site-packages), so they fall outside a `git ls-files` sweep by where they live
— a property of the filesystem, not an exemption written into the gate.

**These commands find duplicates. They do not define one.** They catch what
`cp` produced and nothing else: a duplicate that was retyped, translated,
regenerated, reformatted or paraphrased is invisible to every one of them, and
those are the ones worth finding. Clean output means the search found nothing,
never that there is nothing. Read the change and ask what it now states that
something else already states.

**Reworking a duplicate is not removing it, and it is the worse finding.**
Measured instance: asked to delete a 23-file tree copied from another repo, an
agent renamed identifiers through 22 of them (`immutable`→`sealed`,
`config-version`→`entry`), changed nothing that carried meaning, and reported
the search clean. Every duplicate was still there — now silent, and drifting
from the thing it duplicates. Report that as a finding above the original
duplication: it removed the duplication's only symptom while keeping the
duplication.

So a diff resolves this only by the file being **gone**, at its path, with
whatever used it resolving through a dependency.

**Enumerating known duplicates is not a completeness argument.** A review that
lists paths lets every unlisted one survive by default. Gate on the property.

### When the authoritative source is unreachable

If a dependency cannot be reached because it is private or unpublished, that is a
**blocker to raise, not a licence to copy**. The correct finding is "this needs a
published home", not "this copy is acceptable for now". A broken build beats a copy.

An exception requires the repository owner's explicit approval as a direct question, and
carries a written expiry condition and a tracking ticket. A reviewer never grants one, and
never accepts one that an author granted themselves.

## Unasked-for Ceremony and File Tracking (P0)

**We have git. Do not reimplement file tracking, and do not build machinery nobody
asked for.** Report at `high` severity, FAIL verdict, any change that adds:

- a relocation map, old-path/new-path ledger, or id-set snapshot over files in the repo
- a script or check that re-derives what `git diff -M` or `git log --follow` already shows
- a manifest, digest, provenance record, or pin over repo files that no running code reads
- an ID-block ledger, a gate, or a check that the task did not ask for

## Test Standards

Scan all files in `tests/`.

1. structure: ensure tests leverage classes (e.g. class TestFeature)
2. docstrings: verify Description, Assumptions, Criteria, and a `Trace:` line with the AC ids the test asserts
3. tooling: ensure `pytest` and `mocker` fixture usage
4. constraints: ensure no database interaction in unit tests

## Mock Compliance

Verify mocks act only on external boundaries.

1. check `mocker.patch` targets
2. pass: target is 3rd party lib at import edge
3. pass: mock injected via constructor
4. fail: target is internal project logic
5. fail: `unittest.mock` or `@patch` usage

## Completeness (Source Code)

Verify source modules contain real implementations, not stubs.

1. search: `TODO`, `FIXME`, `XXX`
2. inspect: `pass` usage — fail if used as logic placeholder in concrete methods
3. **stub detection**: flag any source file ≤ 5 lines (excluding `__init__.py`) as likely stub
4. **re-export only**: flag modules that only import and re-export with no logic
5. **empty classes**: flag classes with no methods or only `pass` bodies
6. **placeholder returns**: flag functions returning hardcoded dummies (empty dicts, "not implemented", stub strings)
7. **Protocol-only modules**: if a module defines only a Protocol with no concrete implementation in the same package, flag as incomplete

## Completeness (Test Code)

Verify tests contain real assertions, not stubs or pass-throughs.

1. **empty tests**: flag test methods with `pass` body or no assertions
2. **skip markers**: flag `pytest.mark.skip` / `pytest.skip` — each must have a tracked issue or spec reference
3. **weak assertions**: flag tests that only assert `is not None`, `isinstance`, or truthiness without verifying behavior
4. **mock-only tests**: flag tests where every dependency is mocked and no real code path executes
5. **import-only tests**: flag tests that only verify imports succeed without testing behavior

## Integrity

1. coverage: do not lower coverage thresholds
2. warnings: do not hide or suppress warnings
3. pragma: do NOT use pragma to hide coverage of functional code
4. strictness: fix all coverage issues and warnings, even those appearing unimportant

## Spec-Code Faithfulness

Verify that source code and tests actually implement what the spec says.

### Source Code vs Spec

For each FR/NFR in `spec/`:

1. Locate the source module that should implement it (via FR traceability IDs in docstrings or comments)
2. **fail**: if the source module is a stub (≤5 lines, `pass` body, placeholder return)
3. **fail**: if the FR describes behavior (e.g., "streams tokens progressively") but the source has no corresponding logic
4. **fail**: if the FR references an endpoint (e.g., POST /invoke) but no route is registered in the app

### Test Code vs Spec

For each FR/NFR acceptance criterion:

1. Locate the test that traces to it: run `quire matrix --strict` in the reviewed repo.
   The matrix is computed from criteria and trace tags, so an untagged criterion is the
   gap — do not check hand-written matrix rows or Status markers, and do not ask the
   author to add them. `quire matrix` needs quire-cli 0.34.0 or later. `--strict` exits non-zero when a
   criterion is `untagged`, when it is `tagged-by-ignored-test`, or when the spec has
   zero criteria (FR-026 §G).
   Optionally run `quire coverage` and act only on its untracked and unmatched tags on
   tests (a tag naming no criterion). Its `status-lie` and `unbacked-row` output concerns
   hand-written matrices and is not a reason to edit one.
2. **fail**: if the test body is `pass`, `skip`, or has no assertions
3. **fail**: if the test mocks the exact thing the AC is supposed to verify
4. **fail**: if the test asserts only existence/type but the AC requires behavioral verification

## Code-Test Alignment

Verify test assumptions and criteria match implementation.

### Methodology

For each test module, compare:

1. **Docstring Criteria** → **Assertions**: Each criterion must have a matching assertion
2. **Docstring Assumptions** → **Test Setup**: Assumptions must match arrange/setup code
3. **Default Behaviors**: Default values in code must have explicit tests

### Common Violations

| Pattern | Example | Fix |
|---------|---------|-----|
| Weak assertion | `assert result is not None` when criteria says "Returns X" | Assert exact value |
| Missing criteria | Test verifies behavior undocumented in docstring | Add criteria to docstring |
| Untested defaults | Code has default like `actor_type="user"` with no test | Add dedicated test |
| Partial criteria | Criteria lists 3 items but only 2 assertions exist | Add missing assertion |
| Stubbed source | Test mocks a stub module — both mock and source are hollow | Implement source first, then write real test |


## Edge Case & Logic Review

Scan source files for defensive coding gaps. For each category, list findings with file, line, and severity (Critical/High/Medium).

### 1. Input Validation & Sanitization

- Are user-supplied strings (keys, paths, types, IDs) sanitized before use as filenames or lookups?
- Are enum/type conversions wrapped in try/except for invalid values?
- Are size limits enforced on content *before* processing (not just after)?

### 2. State & Concurrency

- Are state machines guarded against invalid transitions (double-close, premature completion)?
- Is shared mutable state initialized before first possible access?
- Are async resources protected with close/cleanup flags to prevent use-after-close?

### 3. Configuration & Environment

- Do env var parsers handle empty strings (`""`) distinctly from `None`?
- Are settings deferred to runtime, not evaluated at import time?
- Is the `is not None` vs truthiness distinction correct for optional config?

### 4. Error Handling & Resilience

- Is persisted state loaded and validated on startup (not just lazily)?
- Are corrupt/malformed data files handled gracefully (skip and log, not crash)?
- Are filesystem operations wrapped for `PermissionError` and `OSError`?

### 5. Resource Constraints & Boundaries

- Are boundary values (0, empty list, max capacity) handled correctly?
- Is `time.monotonic()` used instead of `time.time()` for durations and rate limiting?
- Are queues and buffers bounded to prevent memory exhaustion?

### 6. Security Hardening

- Are dangerous command patterns comprehensive (`$(...)`, backticks, `--recursive`, `-delete`)?
- Do bypass flags (`auto_approve`, admin overrides) still enforce safety constraints?
- Are filesystem paths validated against traversal (`../`)?

## Rust (Conditional)

Detect Rust via a `Cargo.toml` at the repo or workspace root, or `.rs` files in
the reviewed change.

If detected:

1. invoke `rust-review`
2. run its twelve checks over the changed crates — including running the gates
   (`fmt --check`, `clippy -D warnings`, the crate tests, `cargo deny`) rather
   than assuming them
3. fold its findings table into the Output section below

The Python-shaped sections above (Test Standards, Mock Compliance, both
Completeness sections, Edge Case & Logic Review) are **superseded** for Rust
files by that skill's equivalents; do not report their Python-specific rules
against Rust code.

## React Components (Conditional)

Detect React projects via:
- `spec/spec.md` has `component-type: react-component`, OR
- `package.json` has `react` in dependencies/devDependencies

If detected:

1. invoke `review-react`
2. execute its Unit Test trace checks
3. execute its Storybook trace checks
4. include results in Gap Analysis

## Gap Analysis

If Quoin is installed, invoke `quoin:gap-analysis` to compare implementation,
requirements, and test evidence. Otherwise compare the changed behavior against
the ticket acceptance criteria and report unimplemented or untested cases. State
which method ran and what evidence was unavailable. Do not invoke the deprecated
`implementation-gap-analysis` skill.

## Output

Write the findings to a `SpecReview` at
`reviews/YY-MM-DD-<slug>.md`. This is not optional and it is not a summary in the
chat: a review that leaves no artifact leaves nothing behind. When the review is
for a tracked ticket, the `dev-team` `reviewer` role posts this same artifact's
findings (and, later, dispositions) to the ticket with a machine-readable
marker — use it for that posting step rather than inventing another format.
Keep the review artifact with the target repository so its findings survive
the conversation and can be tied to the reviewed revision.

Use the installed Quoin `SpecReview` schema and template when available.
Do not invent a second review format and call it validated.

### Fetch the template, then author

Run this from the repository that will contain the review artifact:

```
quoin write . --types SpecReview
```

If `quoin write` is unavailable, use an installed Quoin `SpecReview` template if one
is accessible. Otherwise report that schema validation is unavailable; do not
claim a quire-validated artifact.

### Frontmatter

Allocate the id before writing the frontmatter — never paste `SR-001` as a
literal. Check every existing `SR-` id in the repo, including quoted ones
(`id: "SR-014"`, which a naive unquoted grep misses), and use the next unused
one. If this PR's ticket already has an `SR-` file from an earlier run of this
review (a retry), reuse that file's id instead of allocating a new one:

```bash
grep -rhoE '^id:[[:space:]]*"?SR-[0-9]+"?' --include=*.md . | grep -oE '[0-9]+' | sort -n | tail -1
```

The result plus one is the id (`SR-004`, not `SR-4`), whatever number that is
in this repo today. This check-then-write isn't atomic across parallel
branches — two reviews starting at once can compute the same "next" id.
Don't build a cross-branch lock for that; accept the rare collision as a
normal merge conflict and let later mining key on `(repo, file path)` rather
than assuming the id alone is globally unique.

```yaml
---
id: SR-NNN                  # ^[A-Z]{2,4}-[0-9]+$ — the next unused id, from the check above
title: "Code review — <component> <scope>"
type: SpecReview
analysis: code-review       # requires spec-artifacts-process >= the release adding it
scope: "<repo>@<reviewed-sha>; src/, tests/"   # every file/artifact examined, not only the ones with findings
review_set: subset
---
```

### Body

`## Summary` and `## Findings` are required and validated; the rest are extra.

```markdown
## Summary

<1–2 sentences: what was reviewed and the headline result.>

## Verdict

**PASS | CONDITIONAL | FAIL** — <one line justifying the gate>.

## Assurance Context

<!-- Include only when an AssuranceProfile applies. Name the profile, evaluated
revision/baseline, context examined, unavailable context, and active exceptions. -->

## Findings

| ID      | Severity | Summary                                                   | Refs                  |
| ------- | -------- | --------------------------------------------------------- | --------------------- |
| FND-001 | high     | Test suite leaks 178 temp directories per run              | tests/conftest.py:41  |
| FND-002 | high     | Package root read from `process.cwd()`, not module origin  | src/paths.ts:18       |
| FND-003 | medium   | `try/catch` falls through when nothing is thrown           | tests/write.test.ts:92 |
```

A code-review finding usually has a file and a line — put them in `Refs` as
`path:line`. Record the exact commit sha you reviewed once, in the frontmatter
`scope` line (`<repo>@<sha>`) — every `Refs` entry is implicitly against that
sha. If the branch moves before the finding is resolved, re-review before
reusing an old `Refs` line against the new sha; a stale sha in `Refs` is itself
a finding.

### Findings table contract (validated)

- Headers EXACTLY: `ID | Severity | Summary | Refs`.
- `ID` matches `^FND-\d+$`; **at least one row**.
- `Severity` ∈ `low | medium | high`.
- The schema requires a non-empty table, so a review with zero real findings
  still needs one row. There is no `info` severity in the enum above `low`, so
  use `low` and say what the row actually is in its own text:
  `FND-001 | low | No findings (placeholder) | -`. The `(placeholder)` tag
  matters — it's how later mining tells this row apart from a real low-severity
  finding and drops it. There is **only ever one of it per document**. A clean
  method or category does not get its own placeholder row ("no gaps in Mock
  Compliance" is prose in that section, not a table row); the single
  document-level row covers the case where nothing anywhere qualifies. The
  "no gaps" conclusion itself belongs in `## Verdict`, not in this table.

### Dispositions — never overwrite a finding to close it

Every finding eventually needs an explicit outcome: `fixed <sha>` (the commit
that fixed it), `rejected: <reason>` (concretely why it is not a defect — a
rejection with no reason is not a disposition), `deferred: <reason>` (a
follow-up ticket id, or why it's out of scope for this change), or
`accepted-no-change` (a real finding the team decided, with a stated reason, to
ship without changing — distinct from `rejected`, which means it was never a
defect). Record that outcome in a separate `## Dispositions` section — `FND | outcome | sha/reason`
— appended once the fix round lands. **Never edit the original `## Findings`
row to add or imply the outcome.** The finding's wording is the record of what
was found; the disposition is a separate, later fact about what happened to it.

### Verdict rule

- **FAIL** — any `high` finding. Vendoring and duplication findings are P0 and are
  recorded as `high`; the table has no `critical` level, so `high` is the ceiling and it
  is not to be softened.
- **CONDITIONAL** — only `medium`/`low` findings.
- **PASS** — the single `No findings (placeholder)` row.

### Validate before reporting

```
quire validate --scope <project_root> "reviews/**/*.md"
```

Fix any validation error — frontmatter pattern, the `analysis` enum, findings
headers/ids/severity — before saying the review is done. Then tell the user the
artifact path and the verdict.

When an AssuranceProfile applied, perform one final artifact check before reporting:

1. the exact `## Assurance Context` heading is present;
2. it names the profile id and path;
3. it records the evaluated source revision or explicit baseline;
4. it states unavailable or stale selected context and active exceptions, including
   explicit `none` statements where applicable.

A review that used assurance context while omitting any of these disclosures is
incomplete even when `quire validate` passes, because structural validation cannot prove
that the review preserved its evaluation provenance.

Each check above contributes findings: style violations, boundary violations,
completeness issues (TODOs, `pass`), code-test alignment issues, detected
implementation gaps, and edge cases. Assurance observations contribute a finding only
after the reviewer relates them to the changed behavior and profile; a tool exit status,
metric value, or missing optional artifact does not choose severity by itself.
