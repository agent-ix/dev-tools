---
name: new-project
description: Scaffold a new repository from an Agent IX template, verify it locally, establish AGPL and content-rights controls, and create its GitHub repository privately. Use for new project or repository initialization; do not use to change an existing repository's visibility or license.
---

# New Project Initialization

Create a working local project before creating any remote repository. Treat the first
push as a publication boundary even when the GitHub repository is private.

## Choose and render the template

The public scaffold available with this plugin is
`https://github.com/agent-ix/rust-lib-cookiecutter.git` for Rust libraries.
Render it with Cookiecutter over HTTPS or from an existing local clone. Do not
assume that an unauthenticated user can access the other Agent IX templates.

If the user has authenticated access to additional templates, select the
narrowest suitable local cookiecutter:

- `fastapi-cookiecutter` for stateless Python services;
- `pg-data-service-cookiecutter` for PostgreSQL-backed systems of record;
- `faststream-worker-cookiecutter` for asynchronous event workers;
- `python-lib-cookiecutter` for Python libraries;
- `typescript-lib-cookiecutter` for TypeScript libraries;
- `typescript-react-lib-cookiecutter` for React component libraries;
- `web-app-cookiecutter` for web applications;
- `rust-lib-cookiecutter` for Rust libraries.

If none is accessible for the requested project type, report that limitation
and ask for an accessible template or create a tailored project plan. Do not
claim the unavailable template was rendered.

Render locally, inspect the generated tree, and replace template-specific placeholders.
Do not create the GitHub repository yet.

## Establish the publication boundary

Before the first commit or push:

1. Replace any template license with the canonical GNU Affero General Public License
   version 3 text. Record `AGPL-3.0-or-later` in package manifests that declare a
   license. Licensing is necessary but does not authorize publication.
2. Copy [assets/CONTENT_RIGHTS.md](assets/CONTENT_RIGHTS.md) into the repository root and
   fill in the project owner. Keep the default prohibition on external source content
   unless a human has approved a specific, attributable exception.
3. Wire up the org CLA so it is in place before any outside contribution, not added
   after one shows up:
   - Copy [assets/CLA.md](assets/CLA.md) into the repository root byte-identical to the
     canonical copy in `agent-ix/.github` -- do not reword it.
   - If the rendered template already has a `CONTRIBUTING.md`, splice the CLA-signing
     section from [assets/CONTRIBUTING.md](assets/CONTRIBUTING.md) in just below its
     top-level heading, preserving the rest of the file. Otherwise copy
     [assets/CONTRIBUTING.md](assets/CONTRIBUTING.md) in as-is.
   - Copy [assets/cla-workflow.yml](assets/cla-workflow.yml) to
     `.github/workflows/cla.yml`. This is a thin caller into the reusable CLA Assistant
     Lite workflow that lives once in `agent-ix/.github`; do not inline its logic.
   - `rust-lib-cookiecutter` already renders all three of these; for it, skip this step
     rather than overwriting the rendered files.
4. Keep source material with unresolved distribution rights entirely outside the Git
   working tree. Do not put it in ignored directories, Git LFS, issues, PR text, commit
   messages, CI artifacts, or a private GitHub repository.
5. Run the deterministic preflight:

   ```bash
   python <skill-directory>/scripts/preflight.py <generated-project>
   ```

   Stop if it reports a missing license, inconsistent manifest license, missing rights
   policy, protected or unreviewed file type, workstation path, or unresolved-rights marker.
   Inspect any hit without echoing protected text into logs.

Run the generated project's formatter, tests, and build. Initialize Git only after the
tree passes the rights preflight. Make the initial commit include the AGPL license and
content-rights policy.

## Create the private remote

Create the repository as private and push only the preflighted commit:

```bash
gh repo create agent-ix/<project-slug> --private --source=. --remote=origin --push
gh repo view agent-ix/<project-slug> --json visibility --jq .visibility
```

The verification result must be `PRIVATE`. If creation or verification is ambiguous,
stop before retrying a push and inspect the actual remote state.

Do not change visibility as part of this workflow. A later public release requires a
separate, explicit human decision after an AGPL and content-rights audit; the absence of
known protected material is not itself permission to publish.

Record the overall verification outcome and every check in execution order with its exact
command, exit code, and concise result summary. A failed or incomplete verification skips
provider discovery and invocation, but still records the separate deferred handoff result
described below.

## Post-verification assurance handoff

After scaffold verification finishes, follow the generic
[assurance onboarding handoff contract](references/assurance-onboarding-handoff.md).
For failed or incomplete verification, record `deferred` with
`scaffold_verification_failed` without discovery or invocation. After passed
verification, offer the creator an optional handoff to the installed
Engineering Assurance plugin.

Preserve an explicit `invoke`, `deferred`, `skipped`, or `not_applicable`
decision. For `invoke`, discover the installed `engineering-assurance` plugin
through the host's plugin metadata and invoke its named `assurance-onboarding`
skill through the host. Do not guess an installation path or copy the provider
skill into this repository.

Plugin absence, incompatible entry-point metadata, an explicit non-invocation
decision, or provider failure does not undo or reclassify a successfully
verified scaffold. Report the handoff outcome separately from scaffold success.
