# Pathio Backend

Rust + Axum backend for Pathio knowledge-map workspace.

## Requirements

- Rust toolchain (cargo)
- PostgreSQL

## Environment

Create `.env` in `backend/`:

```env
DATABASE_URL=postgres://postgres:123@localhost:5432/pathio_db
PORT=3000
```

Initialize schema (first time):

- Run SQL in `init.sql` against your database.

## Run

```bash
cargo run
```

Health check:

```bash
GET http://127.0.0.1:3000/api/health
```

## Current Free Plan Limits

Implemented in `src/main.rs`:

- Max 3 roadmaps per workspace (`FREE_MAX_ROADMAPS = 3`)
- Max 50 total nodes per workspace across all roadmaps (`FREE_MAX_NODES_PER_ORG = 50`)

Behavior notes:

- Limits are enforced on `POST /api/roadmaps` and `POST /api/nodes`.
- When limit is hit, backend returns `402 Payment Required`.
- Existing nodes can still be edited, moved, renamed, status-updated, and deleted.
- After deleting nodes, free capacity is available again.

## Concurrency Safety

`create_roadmap` and `create_node` run quota checks inside a DB transaction and lock the organization row (`FOR UPDATE`) before count + insert, preventing concurrent over-limit inserts.

## Validate Locally

Compile checks:

```bash
cargo check
cargo test --no-run
```

Recommended API checks:

- Free user can create roadmap #2 and #3, roadmap #4 returns `402`.
- Free workspace can create node #1..#50, node #51 returns `402`.
- At node cap, rename/move/status/delete still succeed.
- Delete one node, then create one node again succeeds.
- Set org `plan_type` to `team`, roadmap/node creation is no longer capped.
