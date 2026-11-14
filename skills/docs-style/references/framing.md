# Framing

Framing decides what the reader learns first. The reader is a software engineer
who is new to the subject. They know tests, types and CI. They do not know the
subject's vocabulary.

The FSL material below is an example of applying the guide to one subject, a
specification language. Replace the subject and keep the method.

## Reader and task first

Start from what the reader wants to do, in their words. Write "check that every
request is answered" where the subject's term would be "evaluate a temporal
clause". The first sentence of a concept, reference or README page says what the
thing is (rule: lede-definition). The second paragraph says what the reader can do
(rule: lede-reader-outcome). How-to and tutorial pages open with the task or the
result instead; status pages open with what they list.

Bad second paragraph:

```text
This section documents Quoin 0.29.1. The facts on these pages come from the
Quoin repository and from running the installed executable.
```

Good second paragraph:

> Quoin is a set of tools that help a coding agent write a specification, review
> it and turn it into a plan. You can start from a requirement in plain English
> and end with a validated document.

The second paragraph never states sourcing, versions or release state.

## State what the reader needs first

A page says what the reader should already know and links to the page that teaches
it (rule: prerequisites-stated). Put it in a `Before you start` section or in one
sentence near the top.

## Define each term once

Define a term the first time a reader needs it, in one sentence, and link to that
definition from every later use (rule: define-term-once). Spell out an acronym
once (rule: undefined-acronym): "the sequential probability ratio test (SPRT)".

## Introduce notation in three moves

A symbol, operator or keyword that the page's running example uses gets three
things in this order (rule: notation-introduced):

1. Its name, as a word.
2. How to say it aloud: `always p` is read "p holds at every position".
3. One use on a concrete example, before the general form. A figure of the
   running example may be that use.

Only then show the general syntax. A reader who can say a formula aloud can
remember it. Notation the running example does not use stays off the page; the
reference page lists it.

## One vocabulary for results

Pick one set of words for results and outcomes, define it once, and use it on
every page of the set (rule: one-term-per-thing). A reader who meets "holds" on one
page and "pass" on the next asks whether they differ.

## One new idea per section

A section teaches one idea and uses only ideas from earlier sections (rule:
one-idea-per-section). If a section needs two ideas, split it.

## Teach in order of rising expressive power

Start with the smallest language that shows the idea and add power in steps.
Forge teaches this way with language levels (rule: rising-expressive-power).
Start from the form the reader can run today, then add one capability per step.
Each step is a form that runs as well. Order the forms by what the reader can
run, and teach a form that does not run yet on no page.

## Order a learning track across pages

A learning track is a list of concept and tutorial pages read in order. Each page
uses only what an earlier page in the track introduced (rule: learning-track-order).
Check a track by listing, for each page, the terms it uses and the page that
defines each one. A term with no earlier definition is a gap: add it earlier, or
define it on the page. Order the track by rising expressive power, with one page
per idea.

## One running example

Pick one small system and use it on every concept and tutorial page (rule:
running-example). The specification-language example uses the history of
configuration versions, each with a version number and an optional parent. A reader
who already knows the system spends attention on the new idea. Build each example
from the previous one. Quint teaches a small example with a deliberate bug from
install to first check. learntla shows a spec as a series of changes from a base
spec.

## Examples run

An example in the docs is a program the reader can run. Take it from the
repository's tests or examples folder, run it in CI, and generate the shown output
with a script. Never paste output by hand; regenerate it when the tool changes
(rule: examples-runnable).

A teaching page teaches with examples that run. A form the tool cannot run yet is
an implementation gap. A gap is never a rule of the thing the page documents.
Leave the form out and let the status page carry it (rule: gap-not-rule).

## Show the command and a trimmed result

A teaching page shows the command a reader types and a trimmed result that
contains only fields the page has defined (rules: concept-no-harness,
output-terms-defined). Define each field or value in the result, or cut it. Pipes into `jq`, redirects
to files, exit-status captures and multi-line filters are plumbing from a test
harness. Keep them out of concept and tutorial pages. State the exit code in a sentence: "The command
exits with status 1".

Bad:

```bash
check rule.native trace.json > out.json; echo "exit $?"
cat out.json | jq '.result'
```

Good:

```bash
check rule.native trace.json
```

```text
violated at position 3
```

The command exits with status 1.

## One figure, one claim

A figure shows one mechanism and its caption states the claim in a sentence (rule:
figure-one-claim). If a sentence says it faster, write the sentence. Draw figures
with the `docs-figures` skill.

## Where the reader goes next

End a concept page with links to the how-to that does the task, the tutorial that
builds it and the reference that lists it. Link to a page; do not list its contents
in the paragraph before the link.
