# Jest and React Testing Library (RTL)

Test behavior from user perspective in jsdom.

## Core Rules & Requirements

### 1. Library & Setup
-   Use React Testing Library (RTL).
-   Import hooks using `React.useX` style.
-   Mock external dependencies only (API, context, router).
-   Do **not** mock internal implementation details.

### 2. Required Coverage
-   Rendering: Default props/state.
-   Prop Variations: All meaningful configurations.
-   Edge Cases: `null`, `undefined`, empty, disabled.
-   Conditional Rendering: All branches.
-   Event Handling: Click, change, submit, keyboard.
-   Accessibility: Roles, labels, aria attributes (`jest-axe`).

### 3. Queries & Assertions
-   Prefer `getByRole`, `getByLabelText`, `getByText`.
-   Avoid `getByTestId`.
-   Use `jest.fn()` for callbacks.
-   Use `await`/`findBy`/`waitFor` for async.
-   Avoid snapshot-only tests.

### 4. Structure
-   Group by behavior using `describe`.
-   Keep deterministic and CI-safe.
-   Return single `ComponentName.test.tsx`.

## Example Spec

```typescript
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Button } from './Button';

describe('Button', () => {
    /**
     * Description:
     *     Verifies onClick handler trigger.
     * Assumptions:
     *     - Button enabled.
     * Criteria:
     *     - onClick called once.
     * Trace: FR-001-AC-02
     */
    it('should call onClick when clicked', async () => {
        const handleClick = jest.fn();
        render(<Button onClick={handleClick}>Click Me</Button>);
      
        await userEvent.click(screen.getByRole('button', { name: /click me/i }));
      
        expect(handleClick).toHaveBeenCalledTimes(1);
    });
});
```
