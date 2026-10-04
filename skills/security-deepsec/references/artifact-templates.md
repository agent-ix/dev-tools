# Artifact Templates

Use a stable local run directory:

```text
<audit-root>/.deepsec/findings/<run-id>/
├── INDEX.md
├── CHECKSITES.json
├── BURNDOWN.md
├── GITHUB.md
├── reports/
│   └── F-001-short-slug.md
└── projects/
    └── <project-id>/
        ├── INDEX.md
        └── checksites/
            └── <slug>.md
```

Default `<audit-root>` to the target repository root unless the user specifies a separate audit workspace:

```text
<target-repo>/.deepsec/findings/<run-id>/
```

## INDEX.md

Include:

- Run ID and date.
- User-requested scope.
- Repos/packages actually reviewed.
- Explicit exclusions and missing requested repos.
- DeepSec commands run and data/export locations.
- Manual supplemental review areas.
- Finding table with ID, title, severity, repo, issue link, status.
- Verification summary after remediation, if any.

## CHECKSITES.json

Use one object per reviewed item:

```json
{
  "id": "CS-001",
  "project": "target-project",
  "source": "deepsec|manual|spec|test|github",
  "matcher": "open-redirect",
  "file": "src/routes/callback.ts",
  "line": 42,
  "area": "redirect-validation",
  "status": "finding",
  "finding_id": "F-001",
  "issue": "https://github.com/org/repo/issues/123",
  "notes": "Untrusted return URL is used directly after callback handling"
}
```

For negative checks, set `status` to `no-finding` or `false-positive` and explain why.

## BURNDOWN.md

Group by finding and include:

- Owner repo/package.
- Required code changes.
- Required spec/AC updates.
- Required tests, and the criterion ids each is tagged with.
- Verification commands.
- GitHub issue link.
- Status: `open`, `in-progress`, `fixed`, `verified`, `closed`.

## GITHUB.md

Include:

- Project/tracker link.
- Labels used.
- Index issue link.
- Finding issue map.
- Closure criteria.
- Final resolution notes after fixes.

## Per-Finding Report

Use this structure for `reports/F-###-slug.md`:

```markdown
# F-###: Title

Severity: Critical|High|Medium|Low
Status: Candidate|Open|Fixed|Verified|Closed
Repos: owner/repo
GitHub Issue: <url or TBD>

## Finding

What is wrong, stated as a broken security invariant.

## Impact

What an attacker or unauthorized user can do.

## Evidence

- `path/to/file.ts:123` - code behavior
- `path/to/spec.md:45` - expected behavior, if applicable

## Reachability

How the path executes and what actor/input controls it.

## Recommended Remediation

Concrete implementation approach.

## Spec, AC, And Trace Impact

- Spec update: yes/no and where.
- Acceptance criteria update: yes/no and IDs.
- Test trace tags: the criterion ids each new test asserts (the matrix is computed by `quire matrix`).

## Verification

- Test or lint command to run.
- Exact behavior to assert.
- Manual verification, if needed.
```

## GitHub Finding Issue

Include:

- Summary.
- Impact.
- Evidence with file/line refs.
- Recommended remediation.
- Verification checklist.
- Spec/AC impact and criterion ids for new tests.
- Local report path.
- Audit metadata.

Required labels: `security`, `ai-handled`, `deepsec`.

## Resolution Comment

Before closing an issue, add:

- Repo and branch.
- Commit SHA.
- Stable tag, if pushed.
- Summary of fix.
- Tests/lint commands with exact pass output.
- Spec/AC updates completed, and `quire matrix` showing the new tests bound to their criteria, or explicit reason not needed.
