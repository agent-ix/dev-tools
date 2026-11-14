# README layout

A README is one page with a fixed layout. Check it with
`docs_lint.py --type readme README.md` (or by file name).

1. Title and badges.
2. Two paragraphs: `<name> is <what it is>.` Then what it does for the reader,
   with the user's verbs.
3. The hero figure (`<picture>`, alt text equal to the figure label verbatim).
4. Getting started: the dependency line, install for any CLI, build from a clone.
5. Usage: the smallest real example, with one sentence saying what it shows.
6. Reference tables.
7. The shared ecosystem figure, kept as is.
8. Building and testing: the two or three commands a user runs.
9. Contributing, License.

Deeper material goes in `docs/`: `index.md`, `how-it-works.md`, `examples.md`,
`api-reference.md`, and a worked scenario when the tool is used to debug
something.

A README keeps badges, the hero figure, install and build, one example taken from
the repository's own tests or examples, reference tables, and a one-line license.
It leaves out lists of what the thing does not do, unconnected systems,
disclaimers, status notes, maintainer process, pointers to tests and ticket ids
(see [phrasing.md](phrasing.md) and [status-and-internals.md](status-and-internals.md)).

A README lede does not roll-call sibling projects (rule: unconnected-systems). One
shared ecosystem figure at the bottom is enough.

A worked-example section opens with the task it solves: "A deadline requirement is
false on a recorded trace. The steps below find out why."

Check fenced code blocks are unchanged, relative links resolve, and doctest READMEs
(`include_str!`) still pass `cargo test --doc`.
