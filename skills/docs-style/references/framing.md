# Framing

Framing decides what the reader learns first. The reader is a software engineer
who is new to formal methods. They know tests, types and CI. They do not know
what a trace, a model or a temporal operator is.

## Reader and task first

Start from what the reader wants to do, in their words. Write "check that every
request is answered", not "evaluate a temporal clause". The first sentence says
what the thing is (rule: lede-definition). The second paragraph says what the
reader can now do (rule: lede-reader-outcome).

Bad:

> This section documents Quoin 0.29.1. The facts on these pages come from the
> Quoin repository and from running the installed `quoin` 0.29.1 executable.

Good:

> Quoin is a set of tools that help a coding agent write a specification, review
> it and turn it into a plan. You can start from a requirement in plain English
> and end with a validated document.

The second paragraph never states sourcing, versions or release state.

## Define each term once

Define a term the first time a reader needs it, in one sentence, and link to that
definition from every later use (rule: define-term-once). Do not define it again
on each page. Spell out an acronym once (rule: undefined-acronym): "the
sequential probability ratio test (SPRT)".

## One new idea per section

A section teaches one idea and uses only ideas from earlier sections (rule:
one-idea-per-section). If a section needs two ideas, split it. A reader new to the
field cannot hold three new words at once.

## Teach in order of rising expressive power

Start with the smallest language that shows the idea and add power in steps.
Forge teaches this way with language levels. For time in FSL, the order is event
positions, then samples, then timestamps, then dense time (rule:
rising-expressive-power). Do not begin with the most general form.

## One running example

Pick one small system and use it on every concept and tutorial page (rule:
running-example). For FSL the system is the history of configuration versions
with a version number and an optional parent. A reader who already knows the
system spends attention on the new idea. Show an example by changing the previous
one, not by starting a new one. Quint teaches a small example with a deliberate
bug from install to first check; learntla shows a spec as a series of changes
from a base spec.

## One figure, one claim

A figure shows one mechanism and its caption states the claim in a sentence (rule:
figure-one-claim). If a sentence says it faster, write the sentence. Draw
figures with the `docs-figures` skill.

## Where the reader goes next

End a concept page with links to the how-to that does the task, the tutorial
that builds it and the reference that lists it. Link to a page; do not list its
contents in the paragraph before the link.
