---
name: docs-style
description: Write and review READMEs and docs pages in the Agent IX house style. Use when writing a README or docs page, when asked to review docs for filler, "Claude-ish" wording or extraneous information, or when a repository's README must be brought to the agent-ix standard. Covers what to say, what to cut, structure, and grep checks. Figures are made with docs-figures.
---

# Docs Style

One rule covers most of this: **say what the thing does and what it helps the
reader do.** Everything else needs a reason to be there.

Figures (diagrams, charts, light/dark SVG pairs) are made with the
`docs-figures` skill. This skill covers the words and the structure around them.

## Voice

1. **First sentence: `<name> is <what it is>.`** Nothing before it: no example,
   no problem story, no scene-setting.
2. **Second paragraph: what it does for the reader.** Lead with the user's
   verbs: "evaluates a trace to true or false", "returns the first trace where
   they differ". Two or three plain sentences.
3. **One fact per sentence.** Cut any sentence that repeats a figure, a table or
   the previous sentence. Cut lead-ins ("Here is…", "Read it top to bottom").
4. **Literal headings:** Usage, Reference, Getting started, Building and testing,
   Contributing, License. No clever or question headings. US spelling.
5. **No figurative or dramatic wording.** No "honest", "trust", "lie", "only as
   good as", "is where", "the point is".
6. **No contrast framing.** No "X, not Y", "rather than", "instead of", "isn't X,
   it's Y". Keep a contrast only when the reader must choose between two real
   options.

## Cut these (the usual filler)

- **Lists of what the thing does not do.** "It does not parse text, does not
  rewrite formulas, does not run R2U2." The reader did not expect it to. Say
  what it does.
- **Crates, products or systems it is not connected to.** "Does not depend on
  Quire", "has no dependency on tl-mltl". If a system is out of context for the
  repo, it does not appear in that README.
- **Disclaimers and compliance boilerplate** in the README: "does not qualify,
  validate, accredit or certify…", dependency-graph license statements. These
  belong in a dedicated docs page, if anywhere.
- **Status notes** ("Status: prerelease, API not stable, publish = false"). State
  how to install instead.
- **Maintainer and process detail:** "Hosted CI is manual-only", toolchain pin
  repeated three times, `CLAUDE.md` / `AGENTS.md` workflow pointers, "work is
  specified before it is implemented", "agent-assisted contributions go through
  the same review", internal gate tooling and its flags.
- **Pointers to tests, fixtures and test names** in Usage ("mirrors
  `tests/reference.rs`", "see `tc_084_…`", "runs as a doc test"). The example
  stands on its own.
- **A sibling-project roll call in the lede.** One shared ecosystem or stack
  figure at the bottom is enough; do not also list every neighbor in paragraph
  one.
- **History:** ticket, spec, task and plan IDs, SHAs, changelog-style narration.

## Keep

Badges, the hero/mechanism figure, an install and build path (Getting started),
one real runnable example taken from the repo's own tests or examples, reference
tables, the shared ecosystem figure, a one-line license.

## Structure

1. Title, badges.
2. Two paragraphs: what it is, what it does for the reader.
3. Hero figure (`<picture>`, alt text equal to the figure label verbatim).
4. Getting started: dependency line (git or registry), install for any CLI, build
   from a clone. Not "status".
5. Usage: the smallest real example, with one sentence saying what it shows.
6. Reference tables.
7. Ecosystem / stack figure (shared across sibling repos, kept as is).
8. Building and testing: the two or three commands a user runs.
9. Contributing (CONTRIBUTING, CLA, CHANGELOG), License.

Deeper material goes in `docs/`: `index.md`, `how-it-works.md`, `examples.md`,
`api-reference.md`, and a worked scenario when the tool is used to debug
something (what you are looking for at each step).

## Review procedure

1. Read the README as someone who has never heard of the project or its
   neighbors. For each sentence ask: does this help me use it? If not, cut it.
2. Run the checks:

       grep -nE 'does not|do not|not depend|no dependency|rather than|instead of|, not |honest|Hosted CI|CLAUDE.md|AGENTS.md|tests/|Agent-assisted|Status:' README.md

   Keep a hit only where "not" states a plain fact the reader needs (for example,
   "does not allocate").
3. Check that fenced code blocks are unchanged, every relative link resolves, and
   doctest READMEs (`include_str!`) still pass `cargo test --doc`.
4. Propose edits as before/after for each cut and a rewritten lede. Do not apply
   them until asked, unless the task is to apply them.
