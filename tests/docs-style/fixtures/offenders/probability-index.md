---
title: "Probability"
description: "Probability is the section for probabilistic rules, which attach chance and cost to a state model; this page maps the random, reward, workload and probabilistic declarations, the sampling and exact engines, what each page documents, what the repository runs and what stays specified only."
status: preview
sidebar:
  order: 1
---

Probability is the part of FSL that states and checks claims about chance. A state model says what can happen. The probabilistic declarations say how often, how long and how much, and the `probabilistic` clause states claims such as "the 95th percentile of the latency is at most 5 ms", "the server is up 99.9% of the time" and "a message is delivered with probability at least 0.95 whatever the scheduler does".

A user writes four kinds of declaration. A `random` declaration gives an operation parameter a distribution. A `workload` weights the operations of a model. A `reward` gives each step of an operation a number, such as its duration. A `probabilistic` clause compares a probability, a quantile, a mean, a long-run fraction or an expected total with a threshold. Two engines decide such a claim. The sampling engine draws behaviours and reports a measurement with an error bound. The exact engine computes the value over the whole finite model and reports a proof or a refutation.

These are real language features with little command-line reach. The commands `format`, `compile` and `run` of `quire-spec` read the declarations and check them, and `compile` then refuses the checked package at emission. The engines are library code, and the exact engine is mostly specified and not yet built. Each page states which part is built, which is specified only, and what a reader can run. The section badge is `preview` for that reason.

## What runs and what does not

| Layer | Built | Reached from the command line |
| --- | --- | --- |
| Syntax of the four declarations | Yes | `quire-spec format` |
| Forms and stage-3 checks, with every `invalid_probabilistic_model` cause | Yes | `quire-spec compile` checks them and then refuses emission with exit 21 |
| Step probabilities, the weighted sampler and sampled behaviours | Yes, in the `qsl-eval` crate | No |
| Sampling engine EN-4 with the Okamoto, SPRT and regenerative methods | Yes, in the `qsl-analyze` crate | No |
| Settlement of a measurement, sampled witnesses and their replay | Yes | No |
| Window aggregates in monitors | Evaluator built, with no source syntax | No |
| Exact engine EN-5: the finite product | Yes | No |
| Exact engine EN-5: solvers, certificates, witnesses, limits, settlement | Specified only | No |
| Clopper–Pearson method, timed sampling, timed exact checking | Specified only | No |

One capability limit applies to the whole section. FR-057 states that no current declaration form belongs to the capability family `probabilistic`, and test case TC-971 verifies that a request for the capability `probabilistic-satisfaction` against any current declaration is inapplicable. The checks and the engines are not capability requests and work as the pages describe. The [evidence and routing](/fsl/analysis/probability/evidence-and-routing/) page states the limit.

