"""FastAPI CRUD app backed by the sqlite3 persistence layer in db.py."""

from __future__ import annotations

from fastapi import FastAPI, HTTPException, status
from pydantic import BaseModel

import db


class ItemIn(BaseModel):
    name: str
    quantity: int


app = FastAPI(title="alps-crud-demo")


@app.post("/items")
def create_item(item: ItemIn) -> dict:
    """Insert a new item and return ``{id, name, quantity}``."""
    new_id = db.insert_item(item.name, item.quantity)
    return {"id": new_id, "name": item.name, "quantity": item.quantity}


@app.get("/items")
def read_items() -> list[dict]:
    """Return all items in insertion order (id ASC)."""
    return db.list_items()


@app.get("/items/{item_id}")
def read_item(item_id: int) -> dict:
    """Return the item with the given id, or 404 if absent."""
    row = db.get_item(item_id)
    if row is None:
        raise HTTPException(status_code=404, detail="Item not found")
    return row


@app.delete("/items/{item_id}", status_code=status.HTTP_204_NO_CONTENT)
def delete_item(item_id: int) -> None:
    """Delete the item with the given id, or 404 if absent."""
    if not db.delete_item(item_id):
        raise HTTPException(status_code=404, detail="Item not found")
    return None
