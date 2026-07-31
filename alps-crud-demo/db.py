"""SQLite persistence layer for the items CRUD app.

Uses the stdlib ``sqlite3`` module only -- no ORM. All values are passed via
parameterized ``?`` placeholders to avoid SQL injection.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

# Database file lives next to this module so the app works regardless of CWD.
DB_PATH = Path(__file__).parent / "items.db"


def get_conn() -> sqlite3.Connection:
    """Return a fresh ``sqlite3.Connection`` configured for this app.

    The connection has ``row_factory=sqlite3.Row`` so callers can access
    columns by name. ``check_same_thread=False`` lets the connection be
    passed across threads (useful for uvicorn workers / TestClient teardown).
    The caller owns the connection lifecycle -- close it (or use it as a
    context manager) and commit when appropriate.
    """
    conn = sqlite3.connect(DB_PATH, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    return conn


def init_db() -> None:
    """Create the ``items`` table if it doesn't already exist."""
    with get_conn() as conn:
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                quantity INTEGER NOT NULL
            )
            """
        )
        conn.commit()


# Side-effect on import: ensure the schema exists so importing db (or, by
# extension, importing main and starting uvicorn) is enough to make the app
# ready to serve traffic. ``init_db`` is idempotent thanks to IF NOT EXISTS.
init_db()


def _row_to_dict(row: sqlite3.Row) -> dict:
    return {"id": row["id"], "name": row["name"], "quantity": row["quantity"]}


def insert_item(name: str, quantity: int) -> int:
    """Insert a new item and return its auto-incremented id."""
    with get_conn() as conn:
        cursor = conn.execute(
            "INSERT INTO items (name, quantity) VALUES (?, ?)",
            (name, quantity),
        )
        conn.commit()
        return cursor.lastrowid


def get_item(item_id: int) -> dict | None:
    """Return the item with the given id as a dict, or None if missing."""
    with get_conn() as conn:
        row = conn.execute(
            "SELECT id, name, quantity FROM items WHERE id = ?",
            (item_id,),
        ).fetchone()
    return _row_to_dict(row) if row is not None else None


def list_items() -> list[dict]:
    """Return all items in insertion order (oldest first by id ASC)."""
    with get_conn() as conn:
        rows = conn.execute(
            "SELECT id, name, quantity FROM items ORDER BY id ASC"
        ).fetchall()
    return [_row_to_dict(row) for row in rows]


def delete_item(item_id: int) -> bool:
    """Delete the item with the given id. Return True if a row was deleted."""
    with get_conn() as conn:
        cursor = conn.execute(
            "DELETE FROM items WHERE id = ?",
            (item_id,),
        )
        conn.commit()
        return cursor.rowcount > 0
