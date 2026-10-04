---
name: writing-tests-pg-data
description: Instructions for writing tests for PG Data services, likely involving database fixtures and integration tests.
---

# Writing PG Data Tests

If this plugin is not initialized or an Agent IX command fails, read [the dev-tools setup guide](https://github.com/agent-ix/dev-tools/blob/main/setup.md) for its prerequisites and local diagnosis.

This skill provides guidelines for writing tests for services using `SQLModel` and `postgres`, aligned with the Agent-IX Golden Path.

**Source Spec**: Derived from `pg-data-service` (SQLModel, Alembic).

## Core Rules

1.  **Framework**: Use `pytest-sqlmodel`. This plugin provides all necessary fixtures and database management.
2.  **Real Database**: Tests must ALWAYS run against a real Postgres database managed by the `postgres` fixture. Do not use SQLite/in-memory databases.
3.  **Isolation**: The `db_session` fixture provided by `pytest-sqlmodel` handles transaction isolation (rollbacks) automatically.
4.  **Factories**: Use helper functions (factories) to create test data rather than raw SQL inserts.

## Fixtures

Standard fixtures used in `conftest.py` (provided by `pytest-sqlmodel`):

-   `db_session`: A `Session` object isolated by a transaction. Use this for DB interactions in tests.
-   `postgres`: The underlying `testcontainers` or service-managed postgres instance URL.
-   `client`: A `TestClient` with the session dependency overridden to use `db_session`.

## Testing Models

Test that models can be created and retrieved.

```python
class TestHeroModel:
    """Tests for the Hero model."""

    def test_create_hero(self, session: Session):
        """
        Description:
            Test that a Hero can be created and persisted.
        
        Assumptions:
            - Database is available
            - Session is active
            
        Criteria:
            - Hero is saved with a generated ID
            - Fields match input values

        Trace: FR-001-AC-1
        """
        hero = Hero(name="Deadpond", secret_name="Dive Wilson")
        session.add(hero)
        session.commit()
        session.refresh(hero)
        
        assert hero.id is not None
        assert hero.age is None
```

## Testing API Endpoints

Test that endpoints interact correctly with the DB.

```python
class TestHeroAPI:
    """Tests for the Hero API endpoints."""

    def test_create_hero_api(self, client: TestClient):
        """
        Description:
            Test the create hero POST endpoint.
        
        Assumptions:
            - API is running
            - Database connection is valid
            
        Criteria:
            - Returns 200 OK
            - Response contains created hero with ID
        """
        response = client.post(
            "/heroes/",
            json={"name": "Spider-Boy", "secret_name": "Pedro Parqueador"}
        )
        data = response.json()
        
        assert response.status_code == 200
        assert data["name"] == "Spider-Boy"
        assert data["id"] is not None
```
