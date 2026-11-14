---
title: "Chance"
description: "Chance is the section for probabilistic rules; this page maps the declarations, what the repository runs and what stays specified only."
status: preview
---

Chance is the part of the language that states claims about probability. The behaviour of a model is drawn by a sampler, and a claim can say "the delay is at most 5 ms in 95 percent of runs".

These are real language features with little command-line reach. The exact engine is mostly specified and not yet built. Each page states which part is built, which is specified only, and what a reader can run. The section badge is `preview` for that reason.

## What runs and what does not

The table lists each layer and whether it is built.

| Layer | Built | Reached from the command line |
| --- | --- | --- |
| Syntax of the declarations | Yes | `acme-plan format` |
| Sampling engine in the `qsl-analyze` crate | Yes | No |
| Exact engine solvers | Specified only | No |

The exact engine is specified but not built, and the sequential test (SPRT) stops sampling early. It was the honest choice, and the checker is only as good as its model, so the API is not stable yet. The next paragraph is long on purpose, to show the length rule, because it goes on and on about the sampler and the engine and the model and the claim and the scheduler and the budget without ever stopping for a full stop.

## The pages

- [Random parameters](/random/): the `random` declaration.
