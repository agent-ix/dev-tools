# Phrasing

Write short, literal sentences. One fact per sentence. A sentence over 30 words
carries two facts (rule: sentence-too-long). US spelling (rule: us-spelling).
Every rule below has a Vale check unless marked review.

## First sentences

The first sentence is `<name> is <what it is>.` Nothing comes before it: no
example, no problem story, no scene-setting. The second paragraph says what the
reader can do, with the reader's verbs: "evaluates a trace to true or false".

## No contrast framing

Say what the thing is. A contrast makes the reader hold the wrong idea first
(rule: contrast-framing). Patterns: "X, not Y", "rather than", "instead of",
"isn't X, it's Y". Keep a contrast only where the reader must choose between two
real options.

Bad: "A clock is the rule that orders positions, not a stopwatch."
Good: "A clock gives each position in a trace a number."

## No figurative or dramatic words

Use the literal word (rule: figurative-words). Words flagged: honest, trust, lie,
"only as good as", "is where", "the point is", "under the hood".

Bad: "A checker is only as good as its model."
Good: "A checker reports results for the model you give it."

## No lead-ins

Start with the fact (rule: lead-ins). Cut "Here is", "Below is", "It is worth
noting", "Note that", "Let's", and "For example," at the start of a sentence.
Introduce a code block with a sentence that says what it shows, or with nothing.

Bad: "Here is an example. Note that the interval starts at zero."
Good: "The interval starts at zero."

## No page self-reference

A page does not describe itself (rule: page-self-reference). "This page covers",
"this section lists", "on this page" and "the page badge is" say nothing about the
subject. Write the subject.

Bad: "This page covers positions, the clock of each profile, the anchor and
watermarks."
Good: "A position is one observation in a trace."

## No stacked hedges

One hedge at most per sentence (rule: hedging-stack). If the condition is known,
state it: "A rule is checked once per trace if the trace is finite."

Bad: "A rule may generally be checked, and could often be reused."

## Acronyms and headings

Literal headings: Usage, Syntax, Getting started, Example, Next steps. No
questions or clever headings (rule: question-heading). Spell out an acronym
before using it (rule: undefined-acronym).

## Review rules

These need a reader's judgment.

- Lists of what the thing does not do (rule: negation-lists). "It does not parse
  text, does not rewrite formulas." The reader did not expect it to. Say what it
  does.
- Systems the thing is unconnected to (rule: unconnected-systems). "Does not
  depend on Quire." If a system is out of context, it does not appear.
- Disclaimers and compliance boilerplate in pages a reader uses to learn or act
  (rule: disclaimer-boilerplate). Put it on a dedicated page if needed.
- Repetition (rule: restates-figure-or-table). Cut any sentence that repeats a
  figure, a table or the previous sentence.
- Rule-of-three padding (rule: rule-of-three). "Fast, flexible and robust" adds
  rhythm. A measured fact adds information.

## Quick before and after

Bad: "Time, in FSL, is where the honest model meets the trace. Here is how it
works."
Good: "A trace is the list of things a system did. A clock numbers each one."
