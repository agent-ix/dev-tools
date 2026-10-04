---
name: backport-code
description: A skill for systematically backporting bug fixes and features across specs, templates, and downstream repositories.
---

# Backport Code

This skill provides a structured workflow for backporting fixes and features across the IX ecosystem, ensuring changes propagate correctly from specs to templates to downstream repos.

## Scope Definitions

Scopes define the breadth of a backport operation:

- all: Applies to the entire ecosystem
- language: Language-specific (e.g., `python`, `typescript`)
- repo-type: Repository type (e.g., `python-lib`, `react-app`)
- library: All languages using a specific library
- feature: A specific feature (e.g., `fastapi`, `postgres`, `postgres`)



## Workflow

1. Determine Target Scope
   Identify which scope(s) the fix applies to (all, language, repo-type, or library).

2. Determine Source Implementation
   Choose the source based on the type of work:
   - New features: Implement in reference implementations first.
   - Bug fixes: Fix in current project if currently iterating. Otherwise, implement in reference implementations first.
   The goal is choosing a golden example to prove a solution against, and then propagating that solution to all other projects.

3. Backport to Specs
   Update relevant specifications if the change affects defined behavior. Specs are the source of truth.

4. Backport to Reference Implementations
   Apply and fully test changes in the reference implementation. Ensure CI passes.
   - Crucial (Golden Path): Verify if the change deviates from or updates the `Golden Path` spec. If so, update the Golden Path documentation first or concurrently. Reference implementations must match the Golden Path.

5. Backport to Templates
   Update project templates (`*-cookiecutter`) to ensure new projects inherit the fix. Test template generation.

6. Forward Port to Downstream Repos
   Propagate to affected repositories based on the target matrix.

7. Skill Retrospective
   - Improve: Consider any corrections received during the task and propose improvements to this skill.
   - Document: Add the specific use case you just completed to the "Example Scenarios" section of this skill file as a reference.

## Planning Format

Plans should always list changes first, followed by target matrices.

### Example Plan Structure

```markdown
## Changes

### [Type]: [Title]
[Description of the change]

## Targets

### Specs

| Spec | Status | PR | Notes |
|------|--------|-----|-------|
| [spec-name] | [Status] | [Link] | [Notes] |

### Templates

| Template | Scope | Status | PR | Notes |
|----------|-------|--------|-----|-------|
| [template-name] | [scope] | [Status] | [Link] | [Notes] |

### Repos

| Repository | Type | Status | PR | Notes |
|------------|------|--------|-----|-------|
| [repo-name] | [type] | [Status] | [Link] | [Notes] |
```

### Status Legend
- ⏳ Pending (Work in progress)
- ✅ Complete (Merged and deployed)
- 🔜 Queued (Not yet started, waiting on dependencies)
- ❌ Blocked (Cannot proceed due to blocker)
- 🔍 Review (PR open, awaiting review)

## Best Practices

- Test First: Test thoroughly in reference implementations before propagating.
- Traceability: Link related PRs and document changes clearly.
- Planning: Update the plan matrix as work progresses.

## Example Scenario: Makefile Standardization

Context: A new pattern for secure package installation (`RUN_WITH_CREDS`) was developed in `py-deps`.

1. Target Scope: `language: python` (All Python projects).
2. Backport to Specs: Updated `golden-path/specs/languages/python/makefile-standards.md` to require `RUN_WITH_CREDS`.
3. Reference Implementation: Updated `auth-service` (the Python reference service) Makefile and verified CI passes with new credentials logic.
   - *Verification*: Confirmed `auth-service` matches the new `makefile-standards.md`.
4. Templates: Updated `fastapi-cookiecutter` Makefile. Generated a blank test project to verify `make build` works out-of-the-box.
5. Forward Port: Propagated changes to `cloud-manager` services, `identity`, and other downstream Python repositories.

## Example Scenario: Prettier Config Visualization

Context: Discovered `docker-compose.yml` was missing `.prettierrc` volume mounts, causing default config usage in `chat-input`.

1. Target Scope: `language: typescript` (Repos using explicit Docker volumes).
2. Source Implementation: Fixed in `chat-input` (Reference).
3. Backport to Templates: Updated `typescript-react-lib-cookiecutter` to include `.prettierrc` and `.prettierignore` mounts.
4. Forward Port: identified repos using explicit file mounts (`chat-window`, `login-box`, `mcp-gateway-ui`) and applied the fix. Skipped repos using directory mounts (`.:/app`) as they implicitly include config.

## Example Scenario: TypeScript Testing Patterns

Context: A new Jest testing pattern (typed mocks, async utilities) was developed and needs propagation to all TypeScript projects.

1. Target Scope: `language: typescript` (All TypeScript projects, both React and non-React).
2. Backport to Specs: Update `golden-path/specs/languages/typescript/testing.md` to document the pattern.
3. Reference Implementation: Apply to `nodejs-lib` (non-React reference) and `typescript-react-lib` (React reference). Verify CI passes on both.
4. Templates: Update **both** templates:
   - `typescript-lib-cookiecutter` for pure TypeScript/Node libraries
   - `typescript-react-lib-cookiecutter` for React component libraries
   - `web-app-cookiecutter` for React web apps
   Generate test projects from each to verify `make test` works.
5. Forward Port: Propagate to downstream repos based on their type:
   - React repos: `core-web-ui`, UI component libraries
   - Non-React repos: `nodejs-lib`, utility packages, MCP servers
