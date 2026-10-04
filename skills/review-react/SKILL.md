---
name: review-react
description: Review React components for test coverage, including unit tests and Storybook stories with requirement traceability.
---

# React Code Review

React component review with separate trace checks for unit tests and Storybook stories.
Tests and stories bind to acceptance criteria by criterion id; the Test Matrix is
computed from those tags by `quire matrix`, never written by hand.

## Detection

Identify React projects by:
- `spec/spec.md` has `component-type: react-component`, OR
- `package.json` has `react` in dependencies/devDependencies

## Unit Test Traceability

Scan `tests/` for RTL tests covering functional requirements.

| Check | Pass | Fail |
|-------|------|------|
| Test file per FR | `tests/unit/<Component>.test.tsx` exists | Missing file |
| AC trace | JSDoc `Trace:` line (or `// Trace:` comment) lists the FR-XXX-AC-Y ids the test asserts | No `Trace:` line, or ids only in Description/Criteria |

## Package and Import Verification

Verify exports, dependency wiring, and import integrity. Import and resolution errors are real errors — they indicate broken peer dependencies, missing ESM config, or incorrect exports maps. Fix the root cause. Never hide errors with workarounds.

### Checks

Exports:
- `src/index.ts` explicitly exports all public components and utilities
- Interfaces/types used by consumers are exported (not implicitly `any`)
- `package.json` has `main`, `types`, and `exports` block pointing to `dist/` outputs
- A test file (e.g. `tests/exports.test.ts`) imports `*` from `../src` and asserts all public members are defined

Dependencies:
- All versions use proper SemVer (`^0.1.0`), not branch names or invalid tags
- Heavy/shared deps (e.g. `mermaid`, `react`, framework libs) are in `peerDependencies`, not `dependencies`

Import integrity:
- All imports use string literals: `import('pkg')` or `import pkg from 'pkg'`
- No variable-expression dynamic imports: `const m = 'pkg'; import(m)` — webpack emits `Critical dependency: the request of a dependency is an expression` and breaks tree-shaking in all downstream consumers
- No workaround comments: `// Hide from static analyzer`, `// Avoid ESM resolution`, `/* @vite-ignore */`
- Jest ESM issues handled via config (`transformIgnorePatterns`, `moduleNameMapper`), never by modifying source imports

### Export test example

```typescript
// tests/exports.test.ts
import * as PublicApi from '../src';

describe('Public API Exports', () => {
  it('should export all public components and hooks', () => {
    expect(PublicApi.MyComponent).toBeDefined();
    expect(PublicApi.useMyHook).toBeDefined();
  });
});
```

## Storybook Traceability

Scan `src/components/` for story files. Each story documents the FR/AC ids it
demonstrates with a JSDoc `Trace:` line. That line is documentation only: quire binds
only test, bench and fuzz functions, so a story never tags a criterion. A criterion
covered only by a story is not a Storybook gap; report it under Coverage Check as
needing a binding Jest/RTL test.

### File Requirements

| Check | Pass | Fail |
|-------|------|------|
| Story file per component | `<Component>.stories.tsx` co-located | Missing file |

### Story Requirements (Per Story)

| Check | Pass | Fail |
|-------|------|------|
| Trace Sync | AC IDs in Story traces match the exact list in corresponding `spec.md` frontmatter | Found outdated or missing AC IDs |
| AC doc trace | JSDoc `Trace:` line lists the FR-XXX-AC-Y ids the story demonstrates (documentation) | No `Trace:` line |
| Controls | `argTypes` for configurable props | Missing interactivity |
| Actions | Callbacks wired to `@storybook/test` actions | No action logging |
| Interactions | `play` function for stateful behaviors | No interaction tests |

### Coverage Check

Do not author a coverage matrix or add rows to `spec/tests.md`. Run
`quire matrix --strict` in the reviewed repo. Every criterion the matrix reports
as untagged — including one only a story traces — is a finding that it needs a
binding Jest/RTL test. `quire matrix` needs quire-cli 0.34.0 or later. `--strict` exits non-zero when a
criterion is `untagged`, when it is `tagged-by-ignored-test`, or when the spec has
zero criteria (FR-026 §G).

### Story Docstring Format

```typescript
/**
 * Description:
 *     Demonstrates editor with placeholder text.
 *
 * Assumptions:
 *     - Editor accepts placeholder prop
 *
 * Criteria:
 *     - Configurable placeholder renders
 *     - Placeholder disappears on focus
 *
 * Trace: FR-001-AC-1, FR-001-AC-2
 */
export const WithPlaceholder: Story = { ... };
```

## Component File Structure

Each file in `src/components/` MUST export exactly one React component. Co-located helpers (hooks, types, constants) that are private to that component are permitted, but a second exported component must live in its own file.

| Check | Pass | Fail |
|-------|------|------|
| One component per file | File exports a single React component | File exports multiple React components |
| Co-located types | Types/interfaces specific to the component live in the same file or a sibling `*.types.ts` | Shared types scattered across component files |
| Sub-components | Compound component parts (e.g. `Menu.Item`) are in a subdirectory (`Menu/MenuItem.tsx`) | Multiple compound parts defined in a single file |

> **Note:** Internal helper components that are not exported (e.g. a small `ListItem` used only inside `List.tsx`) are acceptable within the same file, as long as they are not exported.

## Gap Rating

| Coverage | Rating |
|----------|--------|
| 100% ACs bound by Jest/RTL tests + stories with controls + actions | Full Compliance |
| Stories exist, some ACs lack a binding test | Partial |
| 0 stories | Critical Gap |

## Output

Report sections:

1. Unit Test Coverage: X/Y ACs bound by Jest/RTL tests (%), from `quire matrix`
2. Storybook Coverage: X/Y ACs demonstrated in stories (%) — documentation, not binding
3. Storybook Gaps (missing story files, stories without a `Trace:` line)
4. Criteria needing a binding test (untagged in `quire matrix`, including story-only)
