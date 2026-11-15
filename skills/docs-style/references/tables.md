# Tables

A table is for data that has rows and columns, both meaningful. Everything else is
a list, a paragraph or a section. The rules follow the Google developer
documentation style guide: tables only for two-dimensional data, a sentence that
introduces each table, long tables split. Reference tables may be longer than the
limits suggest for teaching pages; the length rule skips them.

## Two-dimensional data only

If each row has two columns, a name and a description, use a list (rule:
table-two-dimensional).

Bad:

```text
| Page | Describes |
| --- | --- |
| Random parameters | The `random` declaration |
```

Good:

> - [Random parameters](/random): the `random` declaration.

## At most 4 columns

A table with 5 or more columns does not fit a phone (rule: table-too-wide). Split
it by theme, or give each row its own section. Tables written without outer pipes
count too.

Bad: a table with the columns Profile id, Badge, A tick is, Past the end of the
trace, Intervals and Defined by.

Good: one table of Profile and Tick, and a second of Profile and End of trace.

## Short cells

A cell is at most 12 words (rule: table-cell-too-long). A longer cell is a
paragraph; move it under a heading or into a definition list.

Bad: "One admitted semantic-event position of the authoritative sequence"

Good: "One event position"

## Long identifiers get their own section

Code of 30 or more characters in a cell breaks the layout and the reader's eye
(rule: table-code-cell-long). Use the identifier as a heading, or a definition
list:

```markdown
### quire.temporal.event-position.false-extension/v1

A tick is one event position.
```

## Short names in the table, full identifiers above

When rows are named by long identifiers, put the full identifiers once in a code
block or a definition list above the table. Use a short name for each in the
table. The reader reads the full id once and the table stays narrow.

The code block gives each short name its full identifier:

```text
event   quire.temporal.event-position.false-extension/v1
sample  quire.temporal.fixed-rate-sample.false-extension/v1
```

The table then uses the short names:

| Profile | Tick |
| --- | --- |
| event | One event position |
| sample | One sample |

A cell that holds a sentence is a list item or a definition-list entry instead
(rule: table-cell-too-long).

## One sentence introduces each table

A sentence that says what the table shows comes directly before it (rule:
table-without-intro). A heading directly above a table is not an introduction.

Bad:

```markdown
## Profiles

| Name | Tick |
```

Good:

```markdown
## Profiles

The table lists each profile and what one tick means in it.

| Name | Tick |
```

## Split long tables

A table of more than 12 rows in a concept, tutorial, how-to or README page is split
by theme (rule: table-too-long). Give each part its own heading and introduction.

## Say whether the table is complete

A table introduced with "The keys are:" reads as the full list (rule:
complete-table-claim). Give every key, or say "The main keys are name and version;
the others are in the schema."

## Generate tables from output

A table copied by hand from program output goes stale and hides mistakes (rule:
no-hand-copied-output). Where the site can generate the table from the tool, it
does. Where no generator exists, show one real run in a fenced block with the
command that made it, say the rest in a sentence, and ask for a generator. A table
of facts the author knows, such as a truth table, is not program output.

## Check the table at phone width

The author opens every new page at phone and desktop width, in light and dark, and
looks at the tables (rule: rendered-page-checked). A table that clips at phone width
becomes a list.

## Do not restate

A table and the paragraph after it do not say the same thing (rule:
restates-figure-or-table). Remove the paragraph.
