---
title: "Toolkit"
status: shipped
---

Toolkit is a bundle of modules and agent skills for authoring specifications. The bundle lets an agent write a validated document, review it, and plan work.

This section documents Toolkit 0.29.1. The facts on these pages come from the repository and from running the installed executable.

| Part | What it is | Where it lives |
| --- | --- | --- |
| `toolkit` CLI | A native executable that assembles the module catalog, builds authoring packs, computes the evidence-backed test matrix, launches the review and planning workflows, and manages modules. | Installed with `npm install -g @example/toolkit`. |

The skill is only as good as the module, and it runs on main. Here is the layout. Note that the CLI does not depend on the engine, rather than the other way round. See the [gaps](#gaps-in-the-source-documents).
