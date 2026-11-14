# Navigation

The sidebar is the table of contents. A reader finds a page by its label.

## Two levels deep

The sidebar nests at most two levels: a section and a group, then pages (rule:
sidebar-depth). A third level hides pages.

## Five to nine pages per group

A group has 5 to 9 pages (rule: group-size). A group of 29 pages is several
topics. Merge pages, or split the group by reader task.

Bad: the Probability group listing 29 pages.
Good: Probability with five pages: models, claims, sampling, exact checking,
results.

## Labels are reader words

A label names what the reader wants, not a folder (rule: reader-word-labels).

Bad: `model-checking`, `cli`, `adr`.
Good: Check a model, Command line, Decisions.

## One home per topic

A topic has one page (rule: one-home-per-topic). Outcomes, limits, counterexamples
and profiles are each explained once and linked from the others. Two pages with
the same title or headings are merged and the removed URL redirects to the
survivor (rules: duplicate-title, near-duplicate-topic).

## Order

Order a section for a newcomer: concepts, then tutorials, then how-to guides,
then reference. Internal repository documents (decision records, audits,
acceptance packets) are not in the public sidebar.
