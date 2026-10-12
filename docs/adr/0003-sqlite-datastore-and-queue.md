# ADR-0003: SQLite as the only datastore and job queue

- **Status:** Accepted
- **Date:** 2026-10-11
- **Deciders:** project owner
- **Related:** [doc 03 §2](../03-architecture-and-nfrs.md#2-architectural-style), §5, §7 ([NFR-R5](../03-architecture-and-nfrs.md#nfr-r5), [NFR-S1](../03-architecture-and-nfrs.md#nfr-s1)), §8.1; [doc 04 §2](../04-tech-stack.md#2-server-rust); [doc 07 Q2](../07-roadmap-and-open-questions.md#q2) (hosting not pursued)

## Context

The first draft chose PostgreSQL for a lock-free multi-worker job queue (`SKIP LOCKED`), row-level security, point-in-time recovery and managed hosting. Those reasons came from the hosted-service scenario, which has since been dropped ([doc 07 Q2](../07-roadmap-and-open-questions.md#q2)). The target is now one instance for one person or a household, often on a Pi, NUC or small VPS, so every container and every hundred MB of RAM counts.

The server is already designed as a single Rust binary that runs the API, the pipeline workers and the feed server in one process. The extraction sandbox and the TTS process never touch the database.

## Decision

Use **SQLite** (≥ 3.45, bundled through `sqlx`'s `sqlite` feature) as the **only** datastore, including the job queue and outbox. We won't maintain a second database backend.

### Configuration
- `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`.
  - `synchronous=NORMAL` in WAL mode never corrupts the database. After a *power loss* it can lose the last few committed transactions; every pipeline stage is idempotent and re-runnable, so that's acceptable.
- **One write connection** (a pool of size 1) through which all writes go, plus a small read pool. Serialising writes in the application avoids `SQLITE_BUSY` instead of retrying around it.
- Transactions stay short. Network fetches, extraction, synthesis and encoding happen **outside** transactions; only state transitions are written.

### Job queue
- A `jobs` table with `run_after`, `attempts`, `locked_by`, `locked_until` (lease) and a per-user fairness key.
- Claiming a job is a single `UPDATE … WHERE id = (SELECT … ORDER BY priority, fairness LIMIT 1) RETURNING *` on the write connection. Writes are serialised, so the claim is atomic without `SKIP LOCKED`.
- Workers are tokio tasks in the same process. Concurrency is limited by a configurable worker count and, in practice, by TTS throughput.
- Lease expiry (after a crash) and retries with backoff work as before ([doc 03 §5](../03-architecture-and-nfrs.md#5-pipeline-flow--failure-handling)).

### Events and outbox
- State change and outbox row are written in the same transaction (durable, for retries and any future webhooks).
- After commit, events go out on an in-process `tokio::sync::broadcast` channel to SSE subscribers. Postgres `LISTEN/NOTIFY` isn't needed in a single process.

### Storage and backups
- The database file lives on a **local disk volume**. Network filesystems (NFS/SMB) aren't supported because SQLite's locking is unreliable on them. On startup the server checks the filesystem type on Linux and refuses to start on a network filesystem unless the user explicitly overrides it.
- A **built-in scheduled online backup** (`VACUUM INTO`) writes a consistent copy to a backups volume daily by default and keeps the last N copies. Restoring means stopping the server and replacing the file.
- Optionally, self-hosters can run **Litestream** to stream the database continuously to S3-compatible storage. This is documented, not bundled.
- The server takes an automatic backup **before running migrations**.

### Migrations and testing
- `sqlx` embedded migrations, run on startup. Each migration stays compatible with the previous release so rollback works ([NFR-M3](../03-architecture-and-nfrs.md#nfr-m3)).
- Tests use real SQLite on temp files: fast, no containers, same engine as production. CI also runs `PRAGMA integrity_check` after the chaos tests.

## Alternatives considered

| Option | Why not |
|--------|---------|
| PostgreSQL only (previous choice) | Its benefits (multi-host workers, RLS, PITR, managed hosting) only matter for hosting. It costs an extra container, more memory and harder backups for self-hosters. |
| Support both SQLite and PostgreSQL | Doubles the integration-test matrix and makes every query and migration lowest-common-denominator. Not worth it without a hosting goal. |
| Embedded key-value store (redb, sled) | Gives up SQL, migrations and ad-hoc inspection, and needs a home-grown query layer. SQLite is better understood and easier to back up and inspect. |

## Consequences

**Positive**
- One fewer container; lower memory; a simpler `docker compose` setup.
- Backups are file copies; restore is trivial and testable in CI.
- Faster, hermetic tests with no database container.
- Simple to inspect and debug with the `sqlite3` CLI.

**Negative**
- Only one process may write. The API, workers and feed server must stay in one process, and running several server replicas against one database isn't supported.
- No row-level security; authorisation relies entirely on the repository layer (as already designed).
- A power loss can lose the last few seconds of state changes (recovered by idempotent re-runs).

**⚑ Hosting impact**
- A multi-tenant hosted service would need PostgreSQL (or similar) for multi-host workers, RLS and managed backups. The repository and queue code sits behind `rtm-ports` traits, so that would mean writing a new storage adapter, not reworking the app. No such adapter is built or maintained.
