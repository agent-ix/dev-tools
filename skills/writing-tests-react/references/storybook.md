# Storybook Stories

Component development, documentation, and visual/interaction testing.

## Core Principles & Requirements (CSF)

Use Component Story Format (CSF). Export default meta and named stories.

### 1. Interactive Controls
-   Include fully typed args.
-   Add controls for all configurable props.
-   Mock callbacks with actions (e.g., `fn()`).

### 2. Comprehensive Variants
Cover all states:
-   Normal: Happy path.
-   Edge Cases: Long text, boundaries.
-   Loading: Async states (controllable via args).
-   Error: Validation/failure states.
-   Disabled: Interactive elements disabled.

### 3. Testability & Interactions
Expose controllable props.
-   Use `play` functions for interactions.
-   Use Testing Library utilities (`@storybook/test`).
-   Simulate realistic behavior (click, type, keyboard).
-   Assert visible outcomes.
-   Cover happy path and failure/edge path.

### 4. Maintainability
-   Avoid implementation duplication.
-   Keep deterministic.
-   Link stories to Requirements (FR/AC) with a `Trace:` line. That line is
    documentation only: quire binds only test, bench and fuzz functions, so a story
    never tags a criterion. A criterion counts as tagged only when a Jest/RTL test
    carries its id (see `jest-rtl.md`).

## Standard Story Structure

```typescript
import type { Meta, StoryObj } from '@storybook/react';
import { Button } from './Button';
import { userEvent, within, expect, fn } from '@storybook/test';

/**
 * Description:
 *     Button component metadata: Button Interactions.
 */
const meta: Meta<typeof Button> = {
  component: Button,
  title: 'Components/Button',
  tags: ['autodocs'],
  argTypes: {
    variant: {
      control: 'select',
      options: ['primary', 'secondary', 'danger'],
    },
    onClick: { action: 'clicked' },
    disabled: { control: 'boolean' },
    isLoading: { control: 'boolean' },
  },
  args: {
    onClick: fn(),
  }
};

export default meta;
type Story = StoryObj<typeof Button>;

/**
 * Description:
 *     Default healthy state: renders primary variant.
 *
 * Trace: FR-001-AC-01
 */
export const Default: Story = {
  args: {
    variant: 'primary',
    children: 'Click Me',
    disabled: false,
    isLoading: false,
  },
};

/**
 * Description:
 *     Click behavior interaction: fires onClick.
 *
 * Trace: FR-001-AC-02
 */
export const ClickInteraction: Story = {
  args: {
    ...Default.args,
    children: 'Submit Form',
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const button = canvas.getByRole('button', { name: /submit form/i });
    
    await userEvent.click(button);
    
    await expect(args.onClick).toHaveBeenCalled();
  },
};
```
