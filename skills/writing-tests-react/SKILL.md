---
name: writing-tests-react
description: Index to React component testing guides.
---

# Writing React Tests

Index for testing React components.

## Select Guide

### 1. [Functional Unit Tests (Jest / RTL)](references/jest-rtl.md)
Use for verifyng:
-   Component logic, state, side effects.
-   DOM structure, accessibility (jest-axe).
-   Service integration (mocked).
-   Execution: Headless (jsdom), CI/CD.
-   *Includes Jest Test Generation Prompt.*

### 2. [Visual & Interaction Tests (Storybook)](references/storybook.md)
Use for verifying:
-   Visual states (Loading, Error, Empty).
-   User flows (play functions).
-   Documentation.
-   Execution: Browser, visual regression.
-   *Includes Story Generation Prompt.*
