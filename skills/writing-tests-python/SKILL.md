---
name: writing-tests-python
description: Instructions for writing Python tests (pytest), including mocking, fixtures, and golden path standards.
---

# Writing Python Tests

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

This skill provides generic guidelines for writing Python tests using `pytest`, compliant with the Agent-IX Golden Path.

**Source Spec**: Standard Python best practices and Agent-IX conventions.

## Core Rules

1.  **Test Runner**: Always use `pytest`.
2.  **Coverage**: Every feature specified in the spec MUST have corresponding tests. Code must be fully exercised.
3.  **Organization**: Organize tests into classes (e.g., `class TestUserService:`).
4.  **Docstrings**: All test classes and methods must have compliant docstrings explaining *what* is being tested and the expected outcome.
5.  **Database Testing**: Database interactions MUST NOT be mocked. Use `pytest-sqlmodel` for fixtures for db setup and transactional isolation. READ `writing-tests-pg-data` for specific instructions.
6.  **Tooling**: Use `pytest-mock` (the `mocker` fixture) for all mocking. Do NOT use `unittest.mock.patch` decorators.

## Code Style & Docstrings

Follow the **Agent-IX style** for test docstrings.

**Format**:
-   **Description**: What is being tested.
-   **Assumptions**: Preconditions or context (e.g. data state, mocking).
-   **Criteria**: Pass conditions, in prose. Criterion ids here bind nothing.
-   **Trace**: One `Trace:` line listing the criterion ids the test asserts, comma
    separated (`Trace: FR-001-AC-01, FR-005-AC-1`). This line is the tag `quire matrix`
    reads to compute the Test Matrix; a test without it is untagged. Do not add rows to
    `spec/tests.md`.

```python
class TestMathOperations:
    """Tests for basic math operations."""

    def test_addition(self):
        """
        Description:
            Test that addition returns the sum of two numbers.
        
        Assumptions:
            - Inputs are positive integers
            
        Criteria:
            - Result equals the mathematical sum of inputs
            - Result is a positive integer

        Trace: FR-001-AC-01
        """
        assert add(1, 2) == 3
```

## Fixtures

Use `conftest.py` for shared fixtures. Keep fixtures simple and specific to the application logic (not DB).

### Example

```python
@pytest.fixture
def sample_config():
    """Provides a default configuration for testing."""
    return {"timeout": 30, "retries": 3}
```

## Mocking Strategy

### Level 1: Dependency Injection (Preferred)
Design your classes to accept external dependencies in `__init__`. This avoids patching entirely and leads to cleaner, more testable code.

```python
# Code
class Service:
    def __init__(self, client: httpx.Client):
        self.client = client

# Test
def test_service(mocker):
    # Create a mock object directly
    mock_client = mocker.Mock(spec=httpx.Client)
    
    # Inject it
    service = Service(client=mock_client)
    
    # Act
    service.do_something()
    
    # Assert
    mock_client.get.assert_called_once()
```

### Level 2: Patching External Edges (Fallback)
If DI is not possible, use `mocker.patch` to mock the **External Edge** where it is imported.

-   **Edge**: The boundary where your code calls the third-party library.
-   **Do NOT** patch internal logic within your own module.
-   **Target**: Patch the symbol *where it is used* (e.g. `my_module.httpx.Client`), not where it is defined.

**Do NOT use `@patch` decorators.** They are prone to ordering errors.

```python
def test_legacy_service(mocker):
    # Patching the import in 'my_module'
    mock_post = mocker.patch("my_module.httpx.post")
    mock_post.return_value.json.return_value = {"id": "123", "status": "success"}

    # Call the real internal service
    result = service.create_order(item="book")
    assert result.id == "123"
```

### Don't Do This
```python
# BAD: Mocking internal logic
mocker.patch("my_app.services.calculate_total", return_value=100) 

# BAD: Using decorators (confusing argument order)
@patch("my_module.foo")
def test_something(self, mock_foo):
    pass
```
