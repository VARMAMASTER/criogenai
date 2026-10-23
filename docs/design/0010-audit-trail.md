# 0010: Audit trail

Status: **Accepted** (task 3 of the roadmap in `docs/progress.md`; the user
gave full design freedom). Builds on [0008](0008-persistence-and-memory.md)
(storage, threads, memory) and [0004](0004-agent-loop.md) (the agent loop,
approvals).

## Goals

A durable record of **who did what, when**, for software that handles
sensitive data (the first user is a hospital system):

1. Every consequential action is recorded: runs, model calls, tool calls,
   approval requests and decisions, thread rewinds and deletions, memory
   writes and deletions, retention, key rotation.
2. **No content.** Records hold ids, names, counts, codes and hashes, never
   messages, tool arguments, results or memory values. A record can be
   shown to an auditor without showing patient data.
3. **Tamper-evident.** Records form a hash chain: changing, removing or
   reordering a record breaks verification from that record on.
4. **Append-only** through the API: nothing updates or deletes records;
   retention never touches them.
5. In the same database as threads (SQLite, Postgres), or any custom sink.

Non-goals: access control (who may read the trail is the app's and the
database's business), and proof against someone who can rewrite the whole
table and its chain (see §5: anchoring).

## 1. Records

```json
{"seq": 42, "at_ms": 1760500000000, "action": "approval.decided",
 "actor": "user:dr-rao", "thread_id": "visit-7f3a", "run_id": "0199…",
 "subject": "send_prescription", "outcome": "approve",
 "details": {"call_id": "call_9"},
 "prev_hash": "5e1c…", "hash": "a07b…"}
```

| Field | Meaning |
|---|---|
| `seq` | 1, 2, 3… per log, without gaps |
| `at_ms` | when it was appended (Unix ms) |
| `action` | what happened (table in §2) |
| `actor` | who: set by the app (`Thread::actor`), `None` for the system |
| `thread_id`, `run_id` | where, if any |
| `subject` | what it acted on: a tool name, a model, a checkpoint id |
| `outcome` | how it ended: `completed`, an error code, a decision kind |
| `details` | small JSON with ids and numbers only |
| `prev_hash` | the previous record's `hash` (64 zeros for the first) |
| `hash` | SHA-256 (hex) of the record's fields and `prev_hash` (§5) |

## 2. Actions

| Action | Written by | Subject / outcome / details |
|---|---|---|
| `run.finished` | a thread operation (`send`, `approve`, `decide`, `retry`) | operation / `completed`, `suspended` or the error code / `version` |
| `model.called` | the agent's model node | `provider/model` / `ok` or the error kind / input and output tokens |
| `tool.executed` | the agent's tools node, per call that ran | tool / `success` or `error` / `call_id`, `duration_ms` |
| `approval.requested` | a thread operation that suspended | tool / `pending` / `call_id` |
| `approval.decided` | `approve`, `reject`, `decide` | tool / the decision kind / `call_id` |
| `thread.rewound` | `Thread::rewind` | checkpoint id / `ok` |
| `thread.deleted` | `Thread::delete` | — / `deleted` or `not_found` |
| `memory.saved`, `memory.forgotten` | the `remember` / `forget` tools | — / `ok` / `namespace`, `key_sha256` |
| `storage.pruned` | `Storage::prune` | — / `ok` / the report's counts |
| `storage.reencrypted` | `Storage::reencrypt` | — / `ok` / the report's counts |

Memory keys are chosen by the model and may name a condition
(`allergy_penicillin`), so only their SHA-256 is recorded. Namespaces are
ids chosen by the app (0008 §15.2: use opaque ids).

Calls that were not executed (rejected, answered by the reviewer, already
done before a crash and skipped on retry) have no `tool.executed` record;
the decision is recorded instead.

## 3. API

```rust
// In storage: the trail of the same database (SQL), or in process memory.
let storage = Storage::connect(url).await?.with_audit();
// …or any sink:
let storage = storage.with_audit_log(MyAuditLog::new());

let chat = agent.thread("visit-7f3a").actor("user:dr-rao");
chat.send("…").await?;

let log = storage.audit().unwrap();
let records = log.query(&AuditQuery::new().thread("visit-7f3a")).await?;
let report = log.verify().await?;          // checks the whole chain
let (seq, hash) = log.head().await?;       // anchor elsewhere (§5)
```

```rust
trait AuditLog: Send + Sync {
    fn append(&self, event: AuditEvent) -> Result<AuditRecord>;   // assigns seq, at_ms, hashes
    fn query(&self, query: &AuditQuery) -> Result<Vec<AuditRecord>>; // seq order
    fn verify(&self) -> Result<AuditVerification>;
    fn head(&self) -> Result<Option<(u64, String)>>;
}
```

- `AuditEvent` is a record without `seq`, `at_ms` and hashes; builders
  `AuditEvent::new(action).actor(…).thread(…).run(…).subject(…).outcome(…).detail(k, v)`.
- `AuditQuery`: `thread`, `actor`, `action`, `after_seq` (paging),
  `since_ms` / `until_ms`, `limit` (default 100).
- `AuditVerification { records, ok, first_broken: Option<u64>, head }`.
- Backends: `InMemoryAuditLog`; `SqliteCheckpointer::audit_log()` and
  `PostgresCheckpointer::audit_log()` (table `agentbuild_audit`,
  migration 5). `Storage::connect` keeps the database's log ready;
  `with_audit()` turns it on (in-memory storage: an `InMemoryAuditLog`).

## 4. Failure behaviour: fail closed

If an audit record cannot be written, the action it describes fails:

- In the model and tools nodes the step fails (`audit_failed`, retryable
  like a storage error), so nothing runs or is committed without a record.
  A tool that already ran keeps its effect; its record is retried with the
  step only if the step is retried, which may run the tool again. Apps
  that cannot accept that use approval for such tools (the decision is
  recorded before the call runs).
- Thread operations fail with `audit_failed` after their storage effect
  (the run is committed; the record of its outcome is missing and the
  error says so).

New error code: `audit_failed` (transient, 0003 §9).

## 5.1 Signed anchors, and the database's own protection (audit, October 2026)

The SHA-256 chain has no secret, so anyone who can write to the table can
cut off the tail, or edit records and recompute every later hash, and
`verify` still says `ok`. Two additions, neither replacing the chain:

- **The table refuses edits.** Migration 6 adds triggers (SQLite) and a
  trigger function (Postgres) that make `UPDATE`, `DELETE` and, on
  Postgres, `TRUNCATE` fail with "agentbuild_audit is append-only". They
  stop SQL injection elsewhere in an app and careless tooling; dropping a
  trigger takes schema rights.
- **Anchors.** `AuditLog::anchor(key)` signs the head (`seq`, `hash`) with
  HMAC-SHA256 under a key kept outside the database;
  `verify_anchor(&anchor, key)` checks the chain **and** that the record
  the anchor names is still there with the same hash. A cut tail (the
  record is missing) or a recomputed chain (its hash differs) makes it
  report `ok: false` at the anchor's `seq`; an anchor `key` did not sign is
  an `invalid_value` error. Store anchors where the database's writers
  cannot reach, and make them often.

## 5. The hash chain

`hash = SHA-256(canonical JSON of [seq, at_ms, action, actor, thread_id,
run_id, subject, outcome, details, prev_hash])`, hex-encoded. Canonical:
`details` with sorted keys (serde_json's map is sorted), no whitespace.

- Appends are serialized per log so the chain stays linear: SQLite holds
  the write lock (`BEGIN IMMEDIATE`); Postgres holds a transaction
  advisory lock. Several processes can append.
- `verify` recomputes every hash in `seq` order and checks the links and
  that `seq` has no gaps; it reports the first broken record.
- The chain detects edits, deletions and reordering by anyone who cannot
  recompute it. Someone with write access to the table can rewrite the
  whole chain; to detect that, apps copy `head()` somewhere else from time
  to time (another system, a ticket, a signed log) and compare.

## 6. Storage format

`agentbuild_audit(seq INTEGER PRIMARY KEY, at_ms, action, actor,
thread_id, run_id, subject, outcome, details TEXT (JSON), prev_hash,
hash)`, indexes on `thread_id` and `at_ms`. Not encrypted: it holds no
content, and auditors need to read it. Never pruned.

## 7. Not built

- Exporters (to a SIEM, to files): `query` pages through the trail; an
  exporter is a loop over it.
- Signing records with a private key (the chain plus anchoring covers
  tamper evidence for now).
- Audit records for workflows without agents.
