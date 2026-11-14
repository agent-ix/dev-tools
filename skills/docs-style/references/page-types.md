# Page types

Every page has one type. The type decides who the page is for, which sections it
has and what it leaves out. The types follow Diátaxis: a page is a tutorial, a
how-to, a reference or an explanation (called a concept page here). A fifth type,
status, holds generated state. A README is a sixth page with its own layout in
[readme.md](readme.md).

Declare the type in frontmatter (`type: concept`). A page that needs two types is
split into two pages (rules: one-type-per-page, page-type-declared). The structure
checker reads the type, enforces the required sections (rule: required-sections)
and applies a length budget (rule: page-length-budget).

The table lists each type, the question its reader asks and the word budget.

| Type | Reader asks | Budget in words |
| --- | --- | --- |
| concept | What is this, and why does it exist? | 900 |
| tutorial | Can you walk me to a first working result? | 1800 |
| how-to | How do I do this one task? | 700 |
| reference | What exactly is the syntax, option or rule? | 2500 |
| status | What runs today? | 3000 |

Every page except status and reference states what the reader should know first,
in a line near the top or in a `Before you start` section (rule: prerequisites-stated).
Name the earlier pages: "You should know what a trace is" links to the page that
defines a trace.

## Concept

The reader wants to understand an idea before using it. Use this type for "What a
specification language is", "How time works" and "How a rule is checked".

Required, in order:

1. A plain definition in the first sentence: `<name> is <what it is>.`
2. The problem it solves, in the reader's terms.
3. One figure that shows the mechanism.
4. One small running example, under a heading that contains "Example".
5. Next steps: links to the how-to, the tutorial and the reference.

Between the figure and the example, add one step that names the likeliest wrong
reading and corrects it (rule: common-misreading). A newcomer to formal methods
reads "always" as "at some point" or "within 30" as "within 30 seconds". Say so
once, in one short paragraph.

Banned: syntax tables, option lists, error codes, requirement ids, install steps,
numbered procedures. Those belong on other pages (rule: one-type-per-page).

Bad opening, a definition buried in an inventory:

```text
A temporal profile is a registered identity, such as
`quire.temporal.event-position.false-extension/v1`, that fixes what a position, a
tick and the end of a trace mean for a temporal clause. The capability family
`V1-TEMP` holds 23 capabilities of the complete language.
```

Good opening, the definition and then the reader's problem:

> A temporal profile fixes what a position and a tick mean in a rule about time.
> "Within 30" can mean 30 events, 30 samples or 30 seconds. The profile says which.

### Worked skeleton: concept

This skeleton is an example of applying the guide to one subject, time in a
temporal specification language. Replace the subject and keep the shape.

````markdown
---
title: How time works
type: concept
---

Time in a rule is the order of events in a trace, together with a way to
count between them. A trace is the list of things a system did.

You can use this to write a rule such as "every request is answered within 30
events" and to know what the checker means by "within".

## The problem

"Every request is answered quickly" cannot be checked. "Within 30 events"
can, but only if the checker knows what counts as one step.

![A trace with positions 0 to 5 and the interval 0 to 3 marked.](trace.svg)

## Clocks

A clock gives each position in a trace a number. An interval such as 0 to 3
selects the positions numbered 0 to 3, counted from where the rule starts.

## A common misreading

"Within 30" does not mean 30 seconds. It means 30 clock ticks, and the clock
decides what one tick is.

## Example: version updates

The system keeps configuration versions. The rule says an update arrives
within 2 events of a request. (Three lines of rule, one trace of five
positions, the result: holds at position 0, fails at position 3.)

## Next steps

- [Tutorial: check a rule against a trace](../tutorials/check-a-trace.md)
- [How to read a counterexample](../how-to/read-a-counterexample.md)
- [Reference: time](../reference/time.md)
````

Each section teaches one idea (rule: one-idea-per-section). The example is small
enough to read in a minute.

## Tutorial

The reader wants a working result and does not yet know the vocabulary.

Required: `Before you start`, two or more `Step N` sections, `Next steps`.

Each step changes the result of the previous step by one thing and ends with what
the reader sees (rule: tutorial-builds-up). Start from a base example that runs,
then add one feature per step. learntla does this with a spec. Quint does it with
an example that has a deliberate bug.

Banned: choices between alternatives, explanations longer than a paragraph and
reference tables. Link to the concept page.

### Worked skeleton: tutorial

````markdown
---
title: Check a rule against a trace
type: tutorial
---

In this tutorial you write one rule and check it against a recorded trace. You
end with a counterexample you can read.

## Before you start

You should know what a [trace](../concepts/how-time-works.md) is. Install the
tool with the [install guide](../how-to/install.md).

## Step 1: Run the base rule

Save the base rule as `rule.native` and run `check rule.native base.json`.
The output is `holds`.

## Step 2: Add a deadline

Change the rule to require an answer within 2 events. Run the check again.
The output is `violated at position 3`.

## Step 3: Read the counterexample

Run `explain` on the result. It prints the positions that broke the rule.

## Next steps

- [How to check a trace in CI](../how-to/check-in-ci.md)
````

## How-to

The reader knows what they want to do and wants the shortest path.

Required: numbered steps and an `Expected output` (or `Result`) section. A how-to
does one task and states the starting condition in its first sentence (rule:
how-to-single-task). The first sentence is the task, such as "To check a trace
against a rule, run ...". It is not a definition.

Banned: background, history, definitions, alternatives. Link to the concept page.

Bad opening:

```text
Before you check a trace, it helps to understand what a trace is and why
profiles exist. A trace is the list of events a system produced.
```

Good opening:

> To check a trace against a rule, run `check rule.native trace.json`.

### Worked skeleton: how-to

````markdown
---
title: Check a trace in CI
type: how-to
---

To fail a build when a recorded trace breaks a rule, add the check to your
pipeline. You need the rule file and a trace in JSON.

1. Add `check rule.native trace.json` to the test job.
2. Set the job to fail on a nonzero exit status.
3. Commit the trace under `tests/traces/`.

## Expected output

The job prints `holds` and exits with status 0. A broken rule prints
`violated at position N` and exits with status 1.
````

## Reference

The reader looks something up and wants it complete and scannable.

Required: a Syntax, Usage, Options or Fields section, and an Examples section
with a minimal example for each form (rule: reference-example-per-form). Add an
errors section when the thing can fail.

Banned: teaching prose, motivation and running examples. Link to the concept page.
A reference page opens with `<name> is <what it is>.` and then goes to the forms.

### Worked skeleton: reference

````markdown
---
title: Interval
type: reference
---

An interval selects the positions of a trace between two offsets from the
position where a rule starts.

## Syntax

The table lists each form of an interval and what it selects.

| Form | Selects |
| --- | --- |
| `[a,b]` | Positions a to b, both included |
| `[a,*]` | Position a and every later position |

## Examples

`eventually[0,2] holds(done)` requires `done` within 2 positions.

`always[1,*] holds(ok)` requires `ok` from position 1 onward.

## Errors

An interval with `a` greater than `b` is refused with `invalid_interval`.
````

## Status

Generated, one per section, titled "What runs today". It lists what is available,
in preview and planned, from a data file. It holds the state that other pages must
not mention (see [status-and-internals.md](status-and-internals.md)). Do not edit
it by hand.

## Splitting a mixed page

A page with a definition, a syntax table, setup steps and a status note is four
pages. Keep the part that matches the page's main reader question and move the
rest. If a part has no home, delete it.

Split a page when it answers two reader questions, even when it is under its length
budget (rule: split-by-reader-question). "What is a profile?" and "How do I pick
one?" are two questions, so they are a concept page and a how-to. If two questions
need two page types, they are two pages.

A topic has one home page. Two pages with the same title or the same section
headings are merged (rules: duplicate-title, near-duplicate-topic). Headings go
down one level at a time (rule: heading-level-skip).
