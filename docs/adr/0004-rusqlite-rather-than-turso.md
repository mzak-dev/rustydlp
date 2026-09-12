# 0004 — rusqlite rather than turso for the library

Date: 2026-09-12
Status: Accepted

## Context

The library was stored with `turso`, a from-scratch SQLite reimplementation with
an async API. It was chosen while the interface was gpui, whose executor made an
async store the natural fit, and the schema was written without FOREIGN KEY
clauses because turso's constraint support was still moving.

Two things changed. The interface rewrite removed the executor — nothing in this
crate runs on one now, and the store is only ever touched from a worker thread,
so every call site had become `pollster::block_on(store.something())`. And a
dependency audit put numbers on it:

| | crates (Windows target) |
|---|---|
| `turso` | **158** |
| `rusqlite` (bundled) | **10** |

158 of the 273 crates in the whole build were the database — for the one
component that needed neither async nor a novel engine.

## Decision

`rusqlite` with the `bundled` feature, synchronous.

## Consequences

- **The build lost 148 crates.** More than removing gpui itself did.
- **`bundled` compiles SQLite into the binary**, so the `.exe` has no system
  `sqlite3` dependency — which matters for a portable Windows app.
- **The async disappears rather than moving.** Every `block_on` around a store
  call is gone; `store.rs` has no futures at all. `pollster` stays, but only for
  the one-shots and streams `core/` hands back from its own threads.
- **`Connection` is `Send` but not `Sync`**, and the store is shared across
  worker threads behind an `Arc`, so it sits behind a `Mutex`. That also
  serialises access, which is what SQLite wants from a single connection anyway.
  The mutex is not reentrant, so the item/file loaders take `&Connection` rather
  than re-locking.
- **`save_job` and `delete_job` are now transactions.** Rewriting a job's
  children wholesale is exactly what a transaction is for, and a failure halfway
  through can no longer leave a job with half an item list. turso's constraint
  support was the reason that was not done before.
- **The schema is unchanged, FOREIGN KEYs still absent.** Real SQLite supports
  them, but they cannot be added to existing tables, and databases written by the
  turso build already exist. Referential integrity stays in `save_job`/
  `delete_job`.
- **Existing databases should open unchanged** — turso writes the SQLite file
  format — but that is **unverified**: no library written by the turso build was
  available to test against. Worth checking against a real one before release.
