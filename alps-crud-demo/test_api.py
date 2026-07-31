"""Pytest suite for the alps-crud-demo FastAPI app.

Uses FastAPI's TestClient to exercise the in-process app. A function-scoped
fixture resets the SQLite database (including its journal / WAL / shm
sidecars) before AND after each test so tests are fully isolated.
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

import db
import main


def _cleanup_db_files() -> None:
    """Remove the on-disk DB file plus its journal / WAL / shm sidecars."""
    for path in (
        db.DB_PATH,
        db.DB_PATH.with_suffix(".db-journal"),
        db.DB_PATH.with_suffix(".db-shm"),
        db.DB_PATH.with_suffix(".db-wal"),
    ):
        if path.exists():
            path.unlink()


@pytest.fixture
def client():
    """Return a TestClient backed by a freshly-initialized DB.

    The DB is removed (and its sidecars too) before AND after each test so
    AUTOINCREMENT restarts at 1 and tests cannot leak data into each other.
    """
    _cleanup_db_files()
    db.init_db()
    with TestClient(main.app) as c:
        yield c
    _cleanup_db_files()


def test_create_then_get(client: TestClient) -> None:
    """POST creates an item; a follow-up GET /items/{id} returns the same data."""
    payload = {"name": "apple", "quantity": 5}
    post_resp = client.post("/items", json=payload)
    assert post_resp.status_code == 200
    created = post_resp.json()
    assert set(created.keys()) == {"id", "name", "quantity"}
    assert created["name"] == "apple"
    assert created["quantity"] == 5

    get_resp = client.get(f"/items/{created['id']}")
    assert get_resp.status_code == 200
    assert get_resp.json() == created


def test_list_in_insertion_order(client: TestClient) -> None:
    """POST three items; GET /items returns them ordered by id ascending."""
    names = ["apple", "banana", "cherry"]
    quantities = [1, 2, 3]
    for name, qty in zip(names, quantities):
        resp = client.post("/items", json={"name": name, "quantity": qty})
        assert resp.status_code == 200

    list_resp = client.get("/items")
    assert list_resp.status_code == 200
    body = list_resp.json()
    assert len(body) == 3
    assert [item["name"] for item in body] == names
    assert [item["quantity"] for item in body] == quantities
    # IDs should be strictly ascending (insertion order).
    ids = [item["id"] for item in body]
    assert ids == sorted(ids)
    assert len(set(ids)) == 3


def test_get_missing_returns_404(client: TestClient) -> None:
    """GET /items/9999 returns 404 when the item doesn't exist."""
    resp = client.get("/items/9999")
    assert resp.status_code == 404


def test_delete_then_get_returns_404(client: TestClient) -> None:
    """DELETE /items/{id} then GET /items/{id} returns 404.

    The DELETE response must have an empty body (`b""`) to confirm FastAPI
    emitted a guaranteed-empty 204 (no stray JSON payload).
    """
    post_resp = client.post("/items", json={"name": "doomed", "quantity": 1})
    assert post_resp.status_code == 200
    item_id = post_resp.json()["id"]

    del_resp = client.delete(f"/items/{item_id}")
    assert del_resp.status_code == 204
    assert del_resp.content == b""

    get_resp = client.get(f"/items/{item_id}")
    assert get_resp.status_code == 404
