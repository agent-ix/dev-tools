---
name: security-deepsec
description: Run a generic DeepSec-backed security review and remediation burndown for one or more target repositories. Use when asked to audit a repo, app area, service family, frontend/backend/client/SDK surface, infrastructure, or security-sensitive workflow with DeepSec; when asked to create local audit artifacts, GitHub security issues, checksite coverage, finding reports, spec/AC/test traceability, or a repeatable security burndown process.
---

# Security DeepSec

Run a DeepSec-assisted audit as an evidence-first security review. Treat DeepSec as a candidate generator and investigation accelerator, not as the whole audit. The required output is a durable, traceable burndown: local artifacts for complete coverage plus GitHub issues for promoted findings.

## Core Rules

- Read local project instructions first: `AGENTS.md`, `CLAUDE.md`, README, specs, Makefiles, and repo-specific docs.
- Prefer repo-local DeepSec docs and tooling when `.deepsec/node_modules/deepsec` exists.
- If DeepSec is not installed in the target workspace, locate an approved local DeepSec clone or ask before installing/downloading dependencies.
- Keep audit artifacts local unless the user explicitly asks otherwise. Default to `<target-repo>/.deepsec/findings/<run-id>/`.
- Record every reviewed checksite, including negative review results. A complete audit includes "checked and no finding" evidence, not only promoted findings.
- Promote a finding only when file evidence, reachable behavior, impact, and remediation are clear enough to survive code review.
- For every finding, decide whether it needs a spec update, acceptance criteria update, and new tests tagged with the criterion ids they assert (the test matrix is computed by `quire matrix`; do not hand-edit it).
- Do not close GitHub issues until resolution comments include commit SHA, tag if any, and exact verification output.

## Workflow

1. **Define scope**
   - Identify target repos/packages from the user's request instead of assuming names.
   - Record included and excluded repos, missing requested repos, and why.
   - Detect tech stacks from package manifests, lockfiles, framework config, service manifests, route/client directories, infra manifests, and deployment entry points.

2. **Prepare DeepSec**
   - Use an existing `.deepsec` workspace when present.
   - If initialization is needed, ask before installing or downloading dependencies.
   - Keep `INFO.md` short and project-specific: entry-point conventions, auth/session helpers, authorization and tenant/org scope, SDK/client primitives, data stores, background jobs, and deployment boundaries.
   - Run from `.deepsec/` when using the CLI: `pnpm deepsec scan`, `pnpm deepsec process`, optional `pnpm deepsec revalidate`, then `pnpm deepsec export --format md-dir --out ./findings`.

3. **Inspect checksite coverage**
   - Export or inspect DeepSec data to enumerate candidate sites by file, matcher slug, severity, and status.
   - Compare candidates against real entry points and sensitive flows. Add manual checksites for anything DeepSec does not cover.
   - For custom or missing patterns, read DeepSec `docs/writing-matchers.md` and decide whether a custom matcher is warranted.
   - Use [references/checksite-catalog.md](references/checksite-catalog.md) for built-in DeepSec areas, manual review categories, and domain-specific examples.

4. **Manually investigate**
   - Read code around each candidate and adjacent trust boundaries.
   - Trace source to sink: user input, identity/session state, redirect URL, tenant/org ID, auth header, cookie, cache key, telemetry/log, route guard, server call, queue message, file path, SQL/query builder, or cloud resource.
   - Confirm server-side assumptions. UI-only restrictions are never sufficient evidence of enforcement.
   - Mark false positives and no-finding checksites explicitly with reason.

5. **Create local artifact package**
   - Use the structure in [references/artifact-templates.md](references/artifact-templates.md).
   - Include `INDEX.md`, `CHECKSITES.json`, `BURNDOWN.md`, `GITHUB.md`, `reports/*.md`, and per-project checksite notes.
   - Keep local paths stable and include enough evidence that another agent can resume the audit.

6. **Create GitHub issues for promoted findings**
   - Use the GitHub app or `gh` if available.
   - Label every finding issue `security`, `ai-handled`, and `deepsec`.
   - Add issues to the security review project when possible.
   - Include summary, impact, evidence with file/line refs, recommended remediation, verification checklist, spec/AC/test impact, and local report path.
   - Create or update an index issue that links all finding issues and summarizes scope/checksite coverage.

7. **Remediate only when requested or clearly in scope**
   - Implement fixes in the owning repo, using existing patterns.
   - Add focused tests for the failing security invariant and any spec/AC updates.
   - Run repo-native lint/test commands, using `make` where available.
   - Use Docker for permission cleanup when generated files are owned by root or containers.
   - Commit each repo separately with clear messages. Push, merge, tag, and close issues only when requested or explicitly approved.

## Finding Standard

A promoted finding must have:

- **Reachability:** how the vulnerable path can execute.
- **Trust boundary:** what input, identity, tenant, browser state, service principal, job payload, file, network peer, or external actor is untrusted.
- **Broken invariant:** the exact security property that fails.
- **Impact:** plausible unauthorized access, token/secret exposure, injection, XSS, CSRF, account enumeration, tenant escape, cache collision, data corruption, SSRF, RCE, or policy bypass.
- **Evidence:** file/line references and relevant code behavior.
- **Fix:** concrete remediation with criterion-tagged tests and spec/AC changes if needed.

If any part is uncertain, keep the item as a candidate or research note instead of filing it as a finding.

## Commands

Use these from a DeepSec workspace when applicable:

```bash
pnpm deepsec status
pnpm deepsec scan
pnpm deepsec process
pnpm deepsec revalidate
pnpm deepsec export --format md-dir --out ./findings
pnpm deepsec metrics
```

Use matcher-specific scans for calibration:

```bash
pnpm deepsec scan --matchers <slug1>,<slug2>
```

If the user asks for a complete audit, do not stop at DeepSec output. Reconcile DeepSec candidates, manual code review, specs, tests, and GitHub issue coverage.
