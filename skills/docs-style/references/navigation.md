# Navigation

The sidebar is the table of contents. A reader finds a page by its label.

## Two levels deep

The sidebar nests at most two levels: a section and a group, then pages (rule:
sidebar-depth). A third level hides pages.

## Five to nine pages per group

A group has 5 to 9 pages (rule: group-size). A group of 29 pages is several topics.
Merge pages, or split the group by reader task. Merge a group of fewer than 5 into a
neighbor.

The limit is for hand-written groups. A group generated from a source list, such as
one page per module or per type, may be longer, because the reader looks an entry
up by name. Mark the group as generated in the sidebar source.

Bad: the Probability group lists 29 pages.

Good: Probability has five pages: models, claims, sampling, exact checking, results.

## Labels use reader words

A label names what the reader wants, using words the reader knows (rule:
reader-word-labels). A folder name is a poor label.

Bad: `model-checking`, `cli`, `adr`.

Good: Check a model, Command line, Decisions.

A library reference page is the exception. Its label is the library's own name, the
name the reader types into a package manifest, and its first sentence says what the
library is. A path section in `.vale.ini` turns the crate-name rule off for those
pages.

## One home per topic

A topic has one page (rule: one-home-per-topic). Outcomes, limits, counterexamples
and profiles are each explained once and linked from the others. Two pages with the
same title or headings are merged, and the removed URL redirects to the survivor
(rules: duplicate-title, near-duplicate-topic). Error codes follow the same rule:
each reference page has its own errors section, and one generated page lists every
code. A hand-written per-group outcomes page duplicates both, and the copies drift.

When a merge or a deletion removes pages, the facts on them go to a new home or are
listed as dropped (rule: dropped-facts-listed). A redirect points only at a page
that exists.

## Order

Order a section for a newcomer: concepts, then tutorials, then how-to guides, then
reference. Order the concept and tutorial pages as a learning track, so each page
uses only what earlier pages introduced (rule: learning-track-order). Internal
repository documents such as decision records, audits and acceptance packets stay
out of the public sidebar.
