---
name: docs-style
description: Write and review READMEs and docs pages in the Agent IX house style. Use when writing a README or docs page, when asked to review docs for filler, "Claude-ish" wording, sprawl, indirect framing or implementation status, or when a README or docs folder must be brought to the agent-ix standard. Gives a page-type guide, a rule catalog, a Vale style and a structure checker. Figures are made with docs-figures.
compatibility: Python 3.9+ for scripts/docs_lint.py. Vale 3 for the prose rules (MDX also needs mdx2vast).
---

# Docs Style

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md).

One rule covers most of this: **say what the thing does and what it helps the
reader do.** Everything else needs a reason to be there.

The reader is a software engineer who is new to the field. Pages are short,
direct and literal. Figures are made with the `docs-figures` skill; this skill
covers the words, the page types and the structure.

`<skill>` below is this skill's base directory. Every rule has a stable id in
[rules/catalog.yaml](rules/catalog.yaml); reviews and lint output cite those ids.

## Workflow

1. **Pick the page type.** A page is one of concept, tutorial, how-to, reference
   or status. A README has its own layout. Put `type:` in the frontmatter.
   [page-types.md](references/page-types.md), [readme.md](references/readme.md).
2. **Write to the guide.** Framing first (reader, task, one idea, one running
   example), then phrasing, tables, and what stays out.
   [framing.md](references/framing.md), [phrasing.md](references/phrasing.md),
   [tables.md](references/tables.md),
   [status-and-internals.md](references/status-and-internals.md),
   [navigation.md](references/navigation.md).
3. **Run the checks.**
   - Prose: `vale --config=<ini> <paths>`. Copy both `AgentIX/` and `config/` from
     `<skill>/vale/` into your Vale `StylesPath` (the script rules need `config/`),
     and start from [vale/.vale.ini](vale/.vale.ini). MDX needs `mdx2vast` on `PATH`.
     Vale applies a rule to every page; the sample ini shows per-path sections for
     `applies_to`, for the `trust-words` and contrast rules, and for status pages.
   - Structure: `python3 <skill>/scripts/docs_lint.py --check <paths>`. It reads the
     page type from frontmatter. `--type T` fills it in for files without one.
     `--level suggestion` shows everything; `--ignore ids` skips rules.
4. **Judge the `review` rules** in the catalog (`check: review`). No tool checks
   them. Read the page as someone who has never heard of the project.
5. **Report** findings, then fix them.

## Review procedure

1. Read the page as the target reader. For each sentence ask: does this help me
   understand or use the thing? If not, cut it.
2. Run Vale and `docs_lint.py`. Keep a hit unless the sentence states a plain
   fact the reader needs (for example, "does not allocate"). To suppress a rule for
   a block, write `<!-- vale AgentIX.LeadIns = NO -->` before it and
   `<!-- vale AgentIX.LeadIns = YES -->` after it (MDX: `{/* vale AgentIX.LeadIns = NO */}`).
3. Go through the `review` rules for the page's type: lede, one idea per
   section, running example, repetition, two-dimensional tables, navigation.
4. Check that fenced code blocks are unchanged, every relative link resolves,
   and doctest READMEs (`include_str!`) still pass `cargo test --doc`.
5. Report one line per finding, keyed by rule id:

       path:line: rule-id: what is wrong -> the rewrite

   Give a before and after for each cut and a rewritten lede. Do not apply edits
   until asked, unless the task is to apply them.

## The rules in short

- Concept, reference and README pages open with `<name> is <what it is>.` A
  how-to opens with the task, a tutorial with the result, a status page with what
  it lists. The second paragraph says what the reader can do, with no sourcing,
  version or release state.
- Examples run: taken from tests, executed in CI, output generated.
- A learning track introduces terms in order; each page says what to know first.
- One fact per sentence, under 30 words. Literal headings. US spelling.
- No figurative words, no contrast framing, no lead-ins, no page self-reference.
- Tables only for two-dimensional data: 4 columns at most, cells of 12 words at
  most, one introducing sentence.
- No requirement ids, crate or module names, test names, issue numbers, branch
  or version pins in prose. Status lives on the generated status page.
- No lists of what the thing does not do, unconnected systems, disclaimers,
  maintainer process or history.

Full text and examples are in the references. Every rule is in the catalog.

## Layout

- `references/`: the guide (page types, framing, phrasing, tables, status and
  internals, navigation, README layout).
- `rules/catalog.yaml`: every rule, with severity, check kind, examples.
- `vale/`: the `AgentIX` Vale style, `config/scripts/` (rules that skip fenced
  code), and a sample `.vale.ini`.
- `scripts/docs_lint.py`: the structure checker. `scripts/gen_vale_scripts.py`
  generates the Tengo scripts from one template (`--check` finds stale ones).

Run `python3 -m pytest tests/docs-style` for the tests.
