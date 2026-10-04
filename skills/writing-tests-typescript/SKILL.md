---
name: writing-tests-typescript
description: Instructions for writing TypeScript tests (Jest), including typed mocks and testing patterns.
---

# Writing TypeScript Tests

This skill provides guidelines and patterns for writing TypeScript tests using `jest`, compliant with the Agent-IX Golden Path.

**Source Spec**: Derived from `typescript-lib-cookiecutter` and `nodejs-lib` patterns.

## Core Rules

1.  **Test Runner**: Use `jest`.
2.  **Location**: Tests are typically located in a `tests/` directory or alongside source files as `*.test.ts` or `*.spec.ts`.
3.  **Naming**: Use descriptive names for `describe` blocks and `test`/`it` functions.
4.  **Formatting**: Follow `prettier` and `eslint` rules.
5.  **Traceability**: A test that asserts acceptance criteria carries a `Trace:` line in its
    JSDoc (or a `// Trace:` comment) listing the criterion ids, comma separated
    (`Trace: FR-001-AC-2, FR-001-AC-3`). `quire matrix` computes the Test Matrix from these
    tags; ids in `Description` or `Criteria` bind nothing. Do not add rows to `spec/tests.md`.

## Basic Structure

```typescript
import { sum } from '../src/math';

/**
 * Tests for the Math module functions.
 */
describe('Math module', () => {
  /**
   * Description:
   *     Verifies that the sum function correctly adds two numbers.
   *
   * Assumptions:
   *     - Inputs are numbers
   *
   * Criteria:
   *     - Result is the arithmetic sum of inputs
   *
   * Trace: FR-001-AC-1
   */
  it('adds two numbers correctly', () => {
    expect(sum(1, 2)).toBe(3);
  });
});
```

## Mocking

Use `jest.mock` for module mocking. Use `jest.spyOn` for method spying.

### Pattern: Mocking a Module

```typescript
import { performAction } from '../src/action';
import { api } from '../src/api';

jest.mock('../src/api');

/**
 * Tests for the Action module.
 */
describe('Action', () => {
    /**
     * Description:
     *     Verifies that performAction makes a POST request to the API.
     *
     * Assumptions:
     *     - api.post is a jest mock
     *
     * Criteria:
     *     - api.post is called exactly once
     */
    it('calls the api', async () => {
        (api.post as jest.Mock).mockResolvedValue({ data: 'ok' });
        
        await performAction();
        
        expect(api.post).toHaveBeenCalled();
    });
});
```

### Pattern: Typed Mocks (Better)

Use `jest.Mocked<T>` for better type safety.

```typescript
import { api } from '../src/api';
jest.mock('../src/api');

const mockedApi = api as jest.Mocked<typeof api>;

// usage
mockedApi.post.mockResolvedValue(...)
```

## Async Testing

Always await promises.

```typescript
/**
 * Description:
 *     Verifies that data is fetched successfully.
 *
 * Assumptions:
 *     - fetchData resolves with data
 *
 * Criteria:
 *     - Returned data is defined
 */
it('fetches data', async () => {
    const data = await fetchData();
    expect(data).toBeDefined();
});
```

## Setup and Teardown

Use `beforeAll`, `afterAll`, `beforeEach`, `afterEach` for managing test lifecycle.

```typescript
beforeEach(() => {
    jest.clearAllMocks();
});
```
