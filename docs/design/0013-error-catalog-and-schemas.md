# 0013: Error catalog and JSON Schemas

Status: **Accepted** (task 8 of the roadmap in `docs/progress.md`; the user
gave full design freedom). Delivers what [0003](0003-error-model.md) §9
promised ("generated from the Rust enums and checked in CI") and the
language-neutral contracts of [0002](0002-messages-and-tools.md) as
published JSON Schemas.

## 1. Why

Three languages (Rust, Python, TypeScript), a gateway, and apps that
store our JSON (checkpoints, audit records) all depend on the same codes
and shapes. Today they live in Rust enums, prose tables and SDK mapping
tables that can drift apart. After this:

- one **error catalog** (`schemas/error-codes.json`) lists every stable
  code, and tests fail if Rust raises a code the catalog lacks, if the
  docs table differs from the catalog, or if an SDK maps a code to the
  wrong error class;
- one **schema bundle** (`schemas/agentbuild.schema.json`) describes the
  JSON every language exchanges, and tests validate real serialized Rust
  values against it.

## 2. The error catalog

```json
{"version": 1, "codes": [
  {"code": "rate_limited", "layer": "model", "category": "transient",
   "retryable": true, "family": "model", "http_status": 429,
   "description": "The provider asked us to slow down …"},
  …
]}
```

| Field | Meaning |
|---|---|
| `code` | the stable snake_case code (0003 §9: added, never renamed or removed before 1.0) |
| `layer` | where it appears: `run` (a run's error), `model` (a `ModelError` kind), `storage`, `thread`, `tool_result` (inside a tool message, not an error of the run), `build` (an agent or workflow that cannot be built), `gateway` (HTTP error bodies) |
| `category` | 0003 §3: `transient`, `provider_fatal`, `developer`, `policy`, `cancellation`, `model_recoverable`, `internal` |
| `retryable` | whether retrying the same request can succeed |
| `family` | the SDK error class: `model`, `tool`, `limit`, `busy`, `suspended`, `storage`, `validation`, `cancelled`, `other` |
| `http_status` | the gateway's status, where it maps one |

`docs/errors.md` is generated from the catalog (a test compares it and
rewrites it when `AGENTBUILD_UPDATE_DOCS=1`); 0003 §9 points to it instead
of keeping its own table.

**Checks** (in `crates/agentbuild-agent/tests/catalog.rs`):

1. Every code the Rust enums produce (`RunErrorKind`, `ModelErrorKind`,
   `StorageErrorKind`, each built and asked for `code()`) is in the
   catalog with the right layer.
2. Every literal code in the sources (`with_kind("…")`,
   `AgentError::new("…", …)`, tool-result codes) is in the catalog: a
   scan of `crates/*/src`, so a new code cannot ship undocumented.
3. `docs/errors.md` matches the catalog.
4. The Python and TypeScript test suites load the catalog (the scripts
   pass its path) and check that every code with a `family` raises that
   family's class.

## 3. The schema bundle

One file, JSON Schema 2020-12, with every type under `$defs` and
references between them as `#/$defs/…`:

`Message`, `ContentBlock` (one of its variants by `type`), `MediaSource`,
`ToolSpec`, `ToolAnnotations`, `ModelRequest`, `Settings`, `OutputSpec`,
`ModelResponse`, `Usage`, `MessageDelta`, `BlockDelta`, `ModelError`,
`ApprovalRequest`, `Decision`, `AuditRecord`, `UsageRecord` (gateway),
`ErrorObject` (0003 §6.1, what SDKs and the gateway report).

Rules: fields the Rust types may omit are optional; enums list today's
values; objects allow unknown fields where the Rust type is
`#[non_exhaustive]` and serde ignores extras, so adding a field is not a
breaking change for readers (0003 rule "strict now" applies to what we
*write*, not to what we accept).

**Checks** (`crates/agentbuild-agent/tests/schemas.rs`): real values,
built with the public API and serialized by serde, covering every
content block, delta, decision and error kind, validate against their
`$defs`; hand-made invalid values are rejected with useful paths.

## 4. The validator grows the keywords the schemas need

`agentbuild_agent::validate_arguments` (0002 §5.1) gains `anyOf`,
`oneOf`, `allOf` and local `$ref` (`#/$defs/…`, `#/definitions/…`).
The same validator checks tool arguments, so tool schemas that use them
are now enforced: Pydantic writes `anyOf` for every `Optional` parameter
and `$ref` for nested models. This **tightens** validation for such
tools (arguments that did not match were accepted before); the model
sees the problem and corrects itself, as for any invalid arguments.
Unsupported keywords (`pattern`, `format`, remote `$ref`) stay ignored.

## 5. Not in this slice

Code generation of SDK types from the schemas; a schema for checkpoints
(their state is the app's own JSON); publishing the schemas at a URL.
