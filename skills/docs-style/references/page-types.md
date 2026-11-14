# Page types

Every page has one type. The type decides who the page is for, which sections it
has and what it leaves out. The types follow Diátaxis: a page is a tutorial, a
how-to, a reference or an explanation (called a concept page here). A fifth type,
status, holds generated state. A README is a sixth, special page with its own
layout in [readme.md](readme.md).

Declare the type in frontmatter (`type: concept`). A page that needs two types is
split into two pages (rule: one-type-per-page, page-type-declared). The structure
checker reads the type and enforces the sections below (rule: required-sections)
and a length budget (rule: page-length-budget).

| Type | Reader asks | Budget in words |
| --- | --- | --- |
| concept | What is this, and why does it exist? | 900 |
| tutorial | Can you walk me to a first working result? | 1800 |
| how-to | How do I do this one task? | 700 |
| reference | What exactly is the syntax, option or rule? | 2500 |
| status | What runs today? | 3000 |

## Concept

The reader wants to understand an idea before using it. Use this type for "What
FSL is", "How time works" and "How a rule is checked".

Required, in order:

1. A plain definition in the first sentence: `<name> is <what it is>.`
2. The problem it solves, in the reader's terms.
3. One figure that shows the mechanism.
4. One small running example, under a heading that contains "Example".
5. Next steps: links to the how-to, the tutorial and the reference.

Banned: syntax tables, option lists, error codes, requirement ids, install steps,
numbered procedures. Those belong on other pages (rule: one-type-per-page).

Bad (opens with the page, then the inventory):

> A temporal profile is a registered identity, such as
> `quire.temporal.event-position.false-extension/v1`, that fixes what a position, a
> tick and the end of a trace mean for a temporal clause. The capability family
> `V1-TEMP` holds 23 capabilities of the complete language.

Good (definition, then the reader's problem):

> A temporal profile fixes what a position and a tick mean in a rule about time.
> "Within 30" can mean 30 events, 30 samples or 30 seconds. The profile says which.

### Worked skeleton: "How time works"

    ---
    title: How time works
    type: concept
    ---

    Time in a rule is the order of events in a trace, together with a way to
    count between them. A trace is the list of things a system did.

    You use this to write rules such as "every request is answered within 30
    events" and to know what the checker means by "within".

    ## The problem

    "Every request is answered quickly" cannot be checked. "Within 30 events"
    can, but only if the checker knows what counts as one step.

    <figure of a trace with positions 0 to 5 and an interval [0,3] marked>

    ## What a trace and a clock are

    A trace is a list of positions. A clock gives each position a number.
    An interval such as [0,3] selects the positions whose number is 0 to 3
    counted from the position where the rule starts.

    ## Example: version updates

    The system keeps configuration versions. The rule says an update arrives
    within 2 events of a request. (Three lines of rule, one trace of five
    positions, the result: holds at position 0, fails at position 3.)

    ## Next steps

    - Tutorial: Check a rule against a trace
    - How to read a counterexample
    - Reference: Time

Each section has one idea. The example is small enough to read in a minute.

## Tutorial

The reader wants a working result and does not yet know the vocabulary.

Required: `Before you start`, two or more `Step N` sections, `Next steps`.

Each step changes the result of the previous step by one thing, and ends with
what the reader sees (rule: tutorial-builds-up). Start from a base example that
runs, then add one feature per step, as learntla does with a spec and as Quint
does with an example that has a deliberate bug.

Banned: choices between alternatives, explanations longer than a paragraph, and
reference tables. Link to the concept page instead.

## How-to

The reader knows what they want to do and wants the shortest path.

Required: numbered steps and an `Expected output` (or `Result`) section. A
how-to does one task and states the starting condition in its first sentence
(rule: how-to-single-task).

Banned: background, history, definitions, alternatives. Link to the concept page.

Bad:

> Before you check a trace, it helps to understand what a trace is and why FSL
> uses profiles. A trace is...

Good:

> To check a trace against a rule, run `quire-spec check rule.native trace.json`.

## Reference

The reader looks something up and wants it complete and scannable.

Required: a Syntax, Usage, Options or Fields section, and an Examples section
with a minimal example for each form (rule: reference-example-per-form). Add an
errors section when the thing can fail.

Banned: teaching prose, motivation, running examples. Link to the concept page.

## Status

Generated, one per section, titled "What runs today". Lists what is available, in
preview and planned, from a data file. It holds the state that other pages must
not mention (see [status-and-internals.md](status-and-internals.md)). Do not edit
it by hand.

## Splitting a mixed page

A page with a definition, a syntax table, setup steps and a status note is four
pages. Keep the part that matches the page's main reader question and move the
rest. If a part has no home, delete it. A topic has one home page; two pages
with the same title or the same section headings are merged (rules:
duplicate-title, near-duplicate-topic). Headings go down one level at a time
(rule: heading-level-skip).
