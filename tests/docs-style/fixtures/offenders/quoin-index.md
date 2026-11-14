---
title: "Quoin"
description: "Quoin is a bundle of Quire modules, agent skills and workflows for authoring ISO/IEC/IEEE 29148-aligned specifications; this section covers install for each coding agent, configuration, the CLI and the skills."
status: shipped
availability: public
sidebar:
  order: 1
---

Quoin is a bundle of Quire modules, agent skills and workflows for authoring ISO/IEC/IEEE 29148-aligned software specifications and other technical documents. The bundle lets a coding agent write a validated, traceable specification as Markdown, review it, turn it into a plan, and later audit the code against it.

This section documents Quoin 0.29.1. The facts on these pages come from the Quoin repository and from running the installed `quoin` 0.29.1 executable.

## What is in the bundle

| Part | What it is | Where it lives |
| --- | --- | --- |
| `quoin` CLI | A native executable that assembles the module catalog, builds authoring packs, computes the evidence-backed test matrix, launches the review and planning workflows, and manages modules. | Installed with `npm install -g @agent-ix/quoin` or from a GitHub Release archive. |
| Agent plugin | 21 skills that an agent loads: `specify`, `spec-review`, `spec-to-plan`, `gap-analysis`, the analysis lenses and others. | The `skills/` directory of the repository, shipped to Claude Code, Codex, opencode and Copilot. |
| Default module set | Ten Quire modules that define the artifact types and domain objects. | Installed on first catalog access under `~/.ix/filament/modules`. |
| Workflow definitions | The `review` and `to-plan` workflows, run by `ix-flow`. | `workflows/` inside the `spec-review` and `spec-to-plan` skill directories. |
| Module template | A cookiecutter template for a new semantic module repository. | `templates/semantic-module` in the repository. |

The [skills index](/tools/quoin/skills/) lists every skill. The [CLI guide](/tools/quoin/cli-guide/) covers the command groups.

## How Quoin relates to Quire, ix-flow and FSL

| Component | Relationship to Quoin |
