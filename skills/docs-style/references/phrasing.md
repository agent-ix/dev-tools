# Phrasing

Write short, literal sentences. One fact per sentence. A sentence over 30 words
carries two facts (rule: sentence-too-long). Use US spelling (rule: us-spelling).
Examples marked "Bad" appear in fenced blocks so the linter skips them; the guide
lints clean.

## First sentences

A concept, reference or README page opens with `<name> is <what it is>.` Nothing
comes before it: no example, no problem story, no scene-setting. A how-to opens
with the task ("To check a trace, run ...") and a tutorial with the result. A
status page opens with what it lists.

The second paragraph says what the reader can do, with the reader's verbs:
"evaluates a trace to true or false".

## Contrast framing

Say what the thing is. A contrast makes the reader hold the wrong idea first (rule:
contrast-framing). The flagged patterns are `X, not Y`, `rather than`,
`instead of` and `isn't X, it's Y`. Keep a contrast only where the reader must
choose between two real options. The next section describes one deliberate case,
the common misreading.

Bad:

```text
A clock is the rule that orders positions, not a stopwatch.
```

Good:

> A clock gives each position in a trace a number.

## Figurative or dramatic words

Use the literal word. Two rules apply, and a page may switch off the second per
path in `.vale.ini`.

- Rule figurative-words flags `only as good as`, `is where`, `the point is`,
  `under the hood`, `sits`, `stands` and `foundation`. (rule: figurative-words)
- Rule trust-words flags `honest`, `trust` and `lie to`. (rule: trust-words)

Bad:

```text
A checker is only as good as its model.
```

Good:

> A checker reports results for the model you give it.

## Lead-ins

Start with the fact (rule: lead-ins). Cut `Here is`, `Below is`,
`It is worth noting`, `Note that`, `Let's`, and `For example,` at the start of a
sentence. Introduce a code block with a sentence that says what it shows, or with
nothing.

Bad:

```text
Here is an example. Note that the interval starts at zero.
```

Good:

> The interval starts at zero.

## Page self-reference

A page does not describe itself (rule: page-self-reference). `This page covers`,
`this section lists`, `on this page` and `the page badge is` say nothing about the
subject. Write the subject.

Bad:

```text
This page covers positions, the clock of each profile, the anchor and watermarks.
```

Good:

> A position is one observation in a trace.

## Stacked hedges

One hedge at most per sentence (rule: hedging-stack). If the condition is known,
state it: "A rule is checked once per trace if the trace is finite."

## Unconnected systems

A system the reader did not ask about stays out (rule: unconnected-systems).
`Does not depend on Quire` and `does not certify` are flagged. If a system is out
of context, it does not appear.

## Headings and acronyms

Use literal headings: Usage, Syntax, Getting started, Example, Next steps. Do not
use questions or clever headings (rule: question-heading). Spell out an acronym
before using it (rule: undefined-acronym). A definition in the form `the full name (ABC)` counts.

## Review rules

These need a reader's judgment.

- Lists of what the thing does not do (rule: negation-lists). The reader did not
  expect it to parse text or run a tool it never mentioned. Say what it does.
- Disclaimers and compliance boilerplate in pages a reader uses to learn or act
  (rule: disclaimer-boilerplate). Put it on a dedicated page if it is needed.
- Repetition (rule: restates-figure-or-table). Cut any sentence that repeats a
  figure, a table or the previous sentence.
- Rule-of-three padding (rule: rule-of-three). "Fast, flexible and robust" adds
  rhythm. A measured fact adds information.
- A common misreading is allowed once per concept page, as its own short step
  with its own heading. It states the wrong reading and the right one.

## Suppress a finding

Keep a hit unless the sentence states a plain fact the reader needs, such as
"does not allocate". To suppress a rule for one block, use `<!-- vale AgentIX.LeadIns = NO -->`
before it and `<!-- vale AgentIX.LeadIns = YES -->` after it in Markdown. In MDX,
use `{/* vale AgentIX.LeadIns = NO */}`. `<!-- vale off -->` and `<!-- vale on -->`
switch off every rule.

## Quick before and after

Bad:

```text
Time, in FSL, is where the honest model meets the trace. Here is how it works.
```

Good:

> A trace is the list of things a system did. A clock numbers each one.
