#!/usr/bin/env python3
"""Builds the SQLite databases the tests read (checked in, so tests need no Python).

  shop-v1.sqlite      tables with indexes, a view, a trigger, NULLs, blobs, unicode,
                      a WITHOUT ROWID table, a long text spilling into overflow pages,
                      an AUTOINCREMENT table (sqlite_sequence) and a virtual generated column
  shop-v2.sqlite      v1 with one row updated, one deleted and a column added (ALTER TABLE)
  shop-v1-vacuum.db   v1 after VACUUM with a smaller page size: same schema and rows, other layout
  shop-v1-utf16.db    v1 stored with UTF-16le text encoding
"""
import os
import shutil
import sqlite3

here = os.path.dirname(os.path.abspath(__file__))


def path(name):
    p = os.path.join(here, name)
    for suffix in ("", "-journal", "-wal", "-shm"):
        if os.path.exists(p + suffix):
            os.remove(p + suffix)
    return p


def build(name, encoding="UTF-8"):
    db = sqlite3.connect(path(name))
    db.execute(f"PRAGMA encoding = '{encoding}'")
    db.execute("PRAGMA user_version = 7")
    db.executescript(
        """
        CREATE TABLE customers (
            id    INTEGER PRIMARY KEY,   -- rowid alias
            name  TEXT NOT NULL,
            email TEXT UNIQUE,
            score REAL DEFAULT 0.0,
            avatar BLOB,
            name_upper TEXT GENERATED ALWAYS AS (upper(name)) VIRTUAL
        );
        CREATE TABLE orders(id INTEGER PRIMARY KEY AUTOINCREMENT, customer_id INTEGER NOT NULL REFERENCES customers(id),
                            total NUMERIC(10,2), note TEXT, CHECK (total >= 0));
        CREATE INDEX idx_orders_customer ON orders (customer_id);
        CREATE INDEX idx_customers_name ON customers(name COLLATE NOCASE);
        CREATE TABLE settings (key TEXT, scope TEXT, value, PRIMARY KEY (scope, key)) WITHOUT ROWID;
        CREATE TABLE log (message TEXT, at INTEGER);
        CREATE VIEW big_orders AS
            SELECT c.name, o.total FROM orders o JOIN customers c ON c.id = o.customer_id
            WHERE o.total > 100;
        CREATE TRIGGER orders_log AFTER INSERT ON orders
        BEGIN
            INSERT INTO log (message, at) VALUES ('order ' || new.id, 0);
        END;
        """
    )
    customers = [
        (1, "Alice", "alice@example.com", 4.5, bytes([1, 2, 3])),
        (2, "Bob", None, None, None),
        (3, "Zoë Ünïcode 日本語", "zoe@example.com", 3.25, bytes(range(256)) * 4),
        (4, "Dave", "dave@example.com", 0.0, b""),
        (5, "Line\nbreak 'quoted'", None, -1.5, None),
    ]
    db.executemany("INSERT INTO customers (id, name, email, score, avatar) VALUES (?, ?, ?, ?, ?)", customers)
    for i in range(1, 41):
        note = "x" * 9000 if i == 7 else (None if i % 5 == 0 else f"note {i}")
        db.execute("INSERT INTO orders (customer_id, total, note) VALUES (?, ?, ?)", (i % 5 + 1, i * 12.5, note))
    db.executemany(
        "INSERT INTO settings VALUES (?, ?, ?)",
        [("theme", "user", "dark"), ("lang", "user", "de"), ("timeout", "global", 30), ("ratio", "global", 0.75)],
    )
    db.commit()
    return db


v1 = build("shop-v1.sqlite")
v1.close()

shutil.copy(os.path.join(here, "shop-v1.sqlite"), path("shop-v2.sqlite"))
v2 = sqlite3.connect(os.path.join(here, "shop-v2.sqlite"))
v2.executescript(
    """
    UPDATE customers SET email = 'bob@example.com' WHERE id = 2;
    DELETE FROM orders WHERE id = 20;
    ALTER TABLE customers ADD COLUMN vip INTEGER DEFAULT 0;
    UPDATE settings SET value = 'light' WHERE key = 'theme';
    """
)
v2.commit()
v2.close()

shutil.copy(os.path.join(here, "shop-v1.sqlite"), path("shop-v1-vacuum.db"))
vac = sqlite3.connect(os.path.join(here, "shop-v1-vacuum.db"))
vac.execute("PRAGMA page_size = 1024")
vac.execute("VACUUM")
vac.close()

build("shop-v1-utf16.db", "UTF-16le").close()
