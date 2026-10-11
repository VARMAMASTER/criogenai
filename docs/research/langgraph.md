# Research: LangGraph internals

Status: research note, not a decision. Read alongside
[`docs/design/0001-execution-model.md`](../design/0001-execution-model.md).

**Source studied:** `langgraph` 1.2.14, `langgraph-checkpoint` 4.2.0, and
`langgraph-prebuilt` 1.1.0, as installed under
`research/.venv/Lib/site-packages/langgraph/`. Every `path:line` citation below
is relative to that package directory. For example, `pregel/_algo.py:256` means
`site-packages/langgraph/pregel/_algo.py`, line 256. Mechanisms are described in
our own words, and no LangGraph code is reproduced. Statements marked *(by
inspection)* are conclusions drawn from reading the code that we have not
checked by running it.

---

## TL;DR

1. **The engine schedules channels, not nodes.** Nodes (`PregelNode`)
   subscribe to *channels*. A node runs in step N+1 when a channel it triggers
   on has a newer version than the version that node last saw. Static edges,
   joins, routers, `Send`, `Command.goto`, and `defer` are all compiled into
   writes on hidden channels.
2. **State keys are channels, and reducers are a channel's `update(values)`.**
   The default `LastValue` channel raises when a key gets more than one write in
   a step. That is the same rule as our `update_conflict`.
3. **The state commit is atomic, but each task's writes are durable as soon as
   the task finishes.** When a task finishes, its writes are saved as
   "pending writes" against the current checkpoint. Channels change only at the
   end of the step. On resume, finished tasks get their saved writes back and
   only unfinished tasks run again. This is the most valuable idea in the
   codebase.
4. **Interrupts are exceptions.** On resume the node runs again *from the top*,
   and each `interrupt()` call gets its answer by call position. The Functional
   API (`@entrypoint`/`@task`) uses the same replay-by-call-order model for
   whole workflows.
5. **Conditional edges run inside the source node's task.** They see only that
   node's own writes applied to the pre-step state, not the writes of sibling
   nodes. Our routers see the merged state.
6. **The design debt is large.** `pregel/main.py` is 4,429 lines, and most of
   the sync and async code paths are duplicated. All plumbing travels through a
   `RunnableConfig["configurable"]` dict with about 20 magic keys. The engine
   depends on LangChain `Runnable` and callbacks, and checkpoints use
   Python-specific msgpack serialization. A typo in a `goto` target only logs a
   warning.

---

## 1. Pregel engine (`pregel/`)

### 1.1 Core objects

| Object | Where | Role |
|---|---|---|
| `Pregel` | `pregel/main.py:456` | The compiled graph. It holds `nodes`, `channels`, the input and output channel names, `step_timeout`, `retry_policy`, `cache_policy`, `checkpointer`, `store`, `cache`, `trigger_to_nodes`, and `node_error_handler_map` (`main.py:711-762`). It always adds a reserved `__pregel_tasks` channel of type `Topic(Send)` (`main.py:810-815`). |
| `PregelNode` | `pregel/_read.py:97` | Holds `triggers` (channels that wake the node), `channels` (channels read as input), an optional `mapper` (for example, dict to Pydantic model), the `bound` runnable, `writers`, and retry/cache/timeout policies. `node` builds a sequence of bound runnable followed by writers (`_read.py:227-245`). |
| `PregelExecutableTask` | `types.py:694-708` | One unit of work in a step: name, input, proc, a `writes` deque, the patched config, triggers, retry policy, cache key, a deterministic `id`, `path`, and timeout. |
| `PregelLoop` | `pregel/_loop.py:162` | Per-run state: the checkpoint, channels, pending writes, step and stop, and status (`input`/`pending`/`done`/`draining`/`interrupt_*`/`out_of_steps`, `_loop.py:271-279`). It has two subclasses, `SyncPregelLoop` (`:1535`) and `AsyncPregelLoop` (`:1791`). |
| `PregelRunner` | `pregel/_runner.py:136` | Runs one step's tasks concurrently, commits each task's writes as it finishes, and handles fail-fast and timeouts. |

### 1.2 The superstep loop

The driver is the generator `Pregel.stream` (`main.py:2724`). `invoke` is built
on top of it, and `astream` is a near-copy at `main.py:3130`. The core loop is at
`main.py:3031-3055`:

1. **`loop.tick()`** (`_loop.py:614-696`) starts by checking the step limit. If
   `step > stop`, the status becomes `out_of_steps` (`:622-624`), which surfaces
   later as `GraphRecursionError` (`main.py:3069-3078`). Otherwise it plans tasks
   with `prepare_next_tasks` (`:627-644`). If there are no tasks, the status
   becomes `done`. If a drain was requested, it becomes `draining` (`:672-674`).
   On resume, it hands loaded pending writes back to the tasks that produced
   them (`:677-679`). It also checks `interrupt_before` (`:682-686`).
2. **Cached writes.** `loop.match_cached_writes()` fills `task.writes` from the
   node cache (`_loop.py:1615-1628`).
3. **`runner.tick(...)`** runs only tasks that have no writes yet
   (`main.py:3035`). While it runs, it yields control so that stream output can
   be emitted between task completions.
4. **`loop.after_tick()`** (`_loop.py:698-756`) calls `apply_writes` for all
   tasks, emits `values`, clears pending writes, and saves a checkpoint with
   `source="loop"`. It also checks `interrupt_after`.
5. In `durability="sync"` mode, the loop then blocks on the checkpoint
   future (`main.py:3054-3055`).

The step counter starts at `metadata.step + 1` of the loaded checkpoint.
`stop = step + recursion_limit + 1` (`_loop.py:1769-1770`). **The default
`recursion_limit` is 10007**, and it can be overridden with the
`LANGGRAPH_DEFAULT_RECURSION_LIMIT` environment variable
(`_internal/_config.py:32`). The limit counts supersteps, not nodes.

### 1.3 Task planning: PUSH vs PULL, triggers and versions

`prepare_next_tasks` (`_algo.py:392-513`) builds the union of two kinds of
tasks:

- **PUSH tasks.** There is one for each `Send` packet in the `__pregel_tasks`
  Topic (`_algo.py:441-466`). The task path is `(PUSH, idx)`. Each task gets
  `Send.arg` as input instead of reading state (`_algo.py:938-1107`).
- **PULL tasks.** There is one for each node whose triggers fired
  (`_algo.py:490-512`). `_triggers` (`_algo.py:1260-1277`) decides this: a node
  fires if any of its trigger channels is *available* and its version in
  `checkpoint.channel_versions` is greater than the node's entry in
  `checkpoint.versions_seen[node]`.
- **Optimization.** If the previous step reports which channels it updated,
  the engine only examines nodes that subscribe to those channels
  (`trigger_to_nodes`). Candidates are sorted for determinism
  (`_algo.py:475-486`).
- **Input building.** For a PULL task, the engine reads the subscribed channels
  into a dict (or reads a single channel), applies the mapper, and memoizes the
  result per input-shape in `input_cache` (`_algo.py:1348-1392`).
- **Task config.** Every executable task gets a freshly patched config. The
  config carries `CONFIG_KEY_SEND` (the deque `extend` used to record writes),
  `CONFIG_KEY_READ` (a `local_read` partial), the checkpointer, the checkpoint
  namespace and map, a scratchpad, and a `Runtime` (`_algo.py:706-759`). This
  dict is how a node talks to the engine.

### 1.4 Applying writes

`apply_writes` (`_algo.py:232-345`) is the commit:

1. **Sort.** Tasks are sorted by `task_path_str(path[:3])` (`:256`). This makes
   the order that writes reach reducers deterministic, whatever order the tasks
   finished in.
2. **Record what each node has seen.** For every task, `versions_seen[node]` is
   set to the current versions of its triggers (`:261-269`).
3. **Pick the next version.** A single `next_version = max(all versions) + 1` is
   computed through the checkpointer's `get_next_version` (`:271-282`). This
   works as a step-global logical clock, not a per-channel counter.
4. **Consume triggers.** `consume()` is called on every trigger channel that
   was read. This is how barriers reset (`:284-292`).
5. **Group and apply writes.** Writes are grouped by channel. Control channels
   (`__no_writes__`, `__pregel_push`, `__resume__`, `__interrupt__`,
   `__return__`, `__error__`) are skipped. **Writes to unknown channels are
   logged and dropped** (`:294-313`). Then `channel.update(values)` runs once
   per channel. A channel that reports a change gets the new version
   (`:315-323`).
6. **Notify untouched channels.** Every available channel that was not written
   gets `update([])` (`:325-333`). This is how `EphemeralValue` clears itself
   after one step.
7. **Finish.** If no updated channel triggers any node, the step would be the
   last one. In that case every channel gets `finish()`, which wakes
   `defer`red nodes and `*AfterFinish` channels (`:335-342`).

### 1.5 Determinism

- **Task IDs** are a hash (xxh3 for checkpoint `v>1`, otherwise uuid5) of:
  checkpoint id, namespace, step, node name, `PULL`, and the sorted triggers
  (`_algo.py:613-623`). For PUSH tasks, the hash input includes the Send index
  (`:990-997`) or the parent path plus a call counter (`:834-842`). Task IDs are
  stable for a given checkpoint, so writes saved before a crash can be matched
  to the same task after a restart.
- **Write application** order is sorted by path (§1.4). Concurrent completion
  order does not affect the result.
- **Checkpoint IDs** are uuid6 values with `clock_seq=step`, which makes them
  monotonic and sortable (`checkpoint/base/__init__.py:819,854`).
- **Non-determinism leaks back in through user code.** For example,
  `add_messages` assigns `uuid4` IDs to messages that lack one
  (`graph/message.py:203-208`). The loop had to add `ensure_message_ids` before
  background serialization to stop replays from producing different IDs
  (`_loop.py:473-480`).

### 1.6 Concurrency: sync vs async

- **Sync.** Tasks run on a thread pool from `get_executor_for_config`
  (`_executor.py:48-75`). Each submission copies the contextvars context so
  that `get_config()` works inside threads (`:64-71`). With a single task, no
  timeout, and no stream waiter, the runner calls the task inline on the
  calling thread (`_runner.py:204-255`).
- **Async.** Tasks are asyncio tasks, and `config["max_concurrency"]` gates
  them with a semaphore (`_executor.py:131-169`). Sync callables used in async
  graphs are wrapped to run in an executor via `run_in_executor`
  (`_internal/_runnable.py:565-574`).
- **The same pool does background I/O.** Checkpoint `put` and `put_writes` are
  submitted to the same executor (`_loop.py:496-509`, `:1266-1273`).
- **Concurrent streaming.** When `stream_eager`, `subgraphs`, `messages`, or
  `custom` streaming is on, a "waiter" future is added to the futures set. This
  lets the generator yield stream chunks while tasks are still running
  (`main.py:3003-3024`, `_runner.py:256-258`).

### 1.7 Retries

`run_with_retry` (`pregel/_retry.py:573-682`) works as follows:

- The per-node policy wins over the graph default (`:579`).
- Before every attempt it clears `task.writes`, then calls
  `task.proc.invoke(...)`.
- It matches the exception against a list of `RetryPolicy` values (the first
  match wins, `:647-655`).
- It backs off exponentially with optional jitter (`:663-674`). The sync path
  uses `time.sleep`, which blocks a worker thread.
- It sets `CONFIG_KEY_RESUMING` so that subgraphs continue from their own
  checkpoints instead of restarting (`:681-682`).
- **Defaults.** `RetryPolicy` defaults to `initial_interval=0.5`,
  `backoff_factor=2`, `max_interval=128`, `max_attempts=3`, and `jitter=True`
  (`types.py:427-445`). The default `retry_on` refuses to retry `ValueError`,
  `TypeError`, `LookupError`, `OSError`, and similar errors, and retries HTTP 5xx
  responses and `ConnectionError` (`_internal/_retry.py:1-29`).
- **What gets retried.** The node's writers, including its conditional edges,
  are part of the same sequence (`_read.py:227-245`). A failing router therefore
  re-runs the whole node.

### 1.8 Timeouts

There are two layers:

- **`Pregel.step_timeout`** is a wall-clock limit for the whole step. It is
  passed to `runner.tick(timeout=...)` (`main.py:3036`). When it expires, the
  remaining futures are cancelled and the run raises `TimeoutError`
  (`_runner.py:693-699`).
- **Per-node `TimeoutPolicy`** (`types.py:460-523`) has `run_timeout` (a hard
  cap) and `idle_timeout` (no progress for N seconds).
  `refresh_on="auto"` treats writes, stream events, child task scheduling,
  and any LangChain callback event as progress. `refresh_on="heartbeat"` counts
  only explicit `runtime.heartbeat()` calls (`_retry.py:128-141`).
  It is implemented with watchdog tasks raced against the node coroutine
  (`_retry.py:422-517`). On timeout, the engine clears the task's writes,
  cancels it, and raises `NodeTimeoutError`. `NodeTimeoutError` deliberately
  does not subclass `TimeoutError`, so the default retry policy retries it
  (`errors.py:190-199`). **Per-node timeouts are async-only.** Sync nodes are
  rejected because Python threads cannot be cancelled (`_internal/_timeout.py:8-25`,
  `_retry.py:580-583`).

### 1.9 Cancellation and drain

- **Fail-fast.** When a task fails, other in-flight futures are `cancel()`ed
  (`_runner.py:618-636`, `:680-682`). For sync futures this only prevents tasks
  that **have not started yet** (`_executor.py:59`). `BackgroundExecutor.__exit__`
  then waits for all tasks to finish (`_executor.py:43-46`). In the sync
  engine, a failing step therefore still waits for slow sibling nodes.
- **User-raised cancellation.** A `CancelledError` raised by user code is
  converted to `NodeCancelledError` so the run fails instead of tearing down
  silently (`_retry.py:635-640`, `errors.py:168-187`).
- **Cooperative drain.** `RunControl.request_drain()` (`runtime.py:79-105`) is
  checked at each step boundary (`_loop.py:672-674`). The run stops after
  saving its checkpoint and raises `GraphDrained` (`main.py:3079-3082`,
  `errors.py:54-64`). The run can be resumed later. It is designed for SIGTERM
  handling.

### 1.10 Error handling inside a step

`PregelRunner.commit` (`_runner.py:575-615`) handles each finished task:

- **Success.** The writes are saved through `put_writes`. If the node wrote
  nothing, a `__no_writes__` marker is added so that the task still counts as
  finished (`:610-613`).
- **`GraphInterrupt`.** The interrupt and any resume values are saved
  (`:586-592`).
- **Other exceptions.** `(ERROR, exc)` is saved with the task's writes
  (`:597-604`), and the exception is re-raised after the step in
  `_panic_or_proceed` (`:652-699`). Interrupts from parallel tasks are collected
  and combined into one `GraphInterrupt` (`:685-692`).
- **Node error handlers** (new in 1.x) come from
  `add_node(..., error_handler=...)` or
  `set_node_defaults(error_handler=...)` (`graph/state.py:272-335`). They add a
  hidden `__error_handler__<node>` node (`state.py:868-880`). On failure, the
  runner schedules an immediate PUSH task for the handler, which receives a
  `NodeError(node, error)` (`_algo.py:1110-1248`, `_runner.py:298-324`). The
  marker is made durable before the handler runs (`_loop.py:1644-1647`), and
  the handler is rescheduled on resume (`_loop.py:785-850`).

**Task status comes from the write log.** `pregel/_task_status.py:1-50`
documents the rules: a task with any output write is "finished". This has a
surprising consequence that the file itself states: a task that fails *after*
emitting some writes counts as finished (`_task_status.py:43-44`).

---

## 2. Channels (`channels/`)

### 2.1 Interface

`BaseChannel` (`channels/base.py:19-121`) defines these methods:

- `get` and `is_available` read the value.
- `update(values)` is called **once per step with all writes for that channel**,
  in arbitrary order. It returns whether the channel changed.
- `consume()` tells the channel that a subscriber ran.
- `finish()` tells the channel the run is about to end.
- `checkpoint()` and `from_checkpoint()` handle persistence. `copy()` defaults to
  a checkpoint round trip.

### 2.2 Channel catalogue

| Channel | File | Semantics |
|---|---|---|
| `LastValue` | `last_value.py:20-78` | The default for a plain state key. Holds the last value, and **more than one write per step raises `InvalidUpdateError` (`INVALID_CONCURRENT_GRAPH_UPDATE`)** (`:56-67`). |
| `LastValueAfterFinish` | `last_value.py:81-151` | The value becomes visible only after `finish()` and is cleared on consume. Used as the trigger for `defer=True` nodes. |
| `AnyValue` | `any_value.py` | Keeps the last of several writes and assumes they are equal. Clears on an empty step (`:52-62`). |
| `EphemeralValue(guard)` | `ephemeral_value.py:55-68` | Visible for one step only, cleared by the next step's empty update. With `guard=False` it accepts several writes. **Every node's trigger channel `branch:to:<node>` is an `EphemeralValue(guard=False)`.** |
| `UntrackedValue` | `untracked_value.py:48-49` | Never checkpointed (`checkpoint()` returns MISSING). It is also stripped from persisted writes and from `Send` arguments (`_loop.py:454-469`, `_algo.py:1442-1460`). |
| `BinaryOperatorAggregate(typ, op)` | `binop.py:65-155` | Folds `op(acc, v)` over the step's writes. The starting value comes from the type's zero value (`typ()`, `:89-92`). It recognizes `Overwrite` (a dataclass, a sentinel dict, or a JSON-erased dict, `:31-51`) and allows at most one `Overwrite` per step (`:129-141`). |
| `DeltaChannel` (beta) | `delta.py:25-60` docstring | A batch reducer `(state, [writes]) -> state` that must be batching-invariant. Checkpoint blobs hold only a sentinel. State is rebuilt by replaying ancestor writes, with periodic snapshots (`snapshot_frequency`, plus a global 5000-step bound in `_internal/_config.py:33-34`). This channel exists because whole-value blobs of message lists grow quadratically. |
| `Topic(accumulate)` | `topic.py:77-85` | A pub/sub list. It flattens list writes and either clears every step or accumulates. `__pregel_tasks` is a `Topic(Send)`. |
| `NamedBarrierValue(names)` | `named_barrier_value.py:13-81` | Becomes available when every name in the set has written. `consume()` resets it. **Multi-source joins use this channel.** |
| `NamedBarrierValueAfterFinish` | `named_barrier_value.py:84-167` | A barrier combined with finish, used for joins into `defer` nodes. |

### 2.3 Reducers and `StateGraph` schemas

`_get_channels` (`graph/state.py:1815-1873`) reads `get_type_hints(schema,
include_extras=True)` and maps each key to a channel. The first matching rule
below applies:

1. A managed value (`Annotated[int, RemainingStepsManager]`) becomes a
   `ManagedValueSpec`. Managed values are computed per task from the
   scratchpad, not stored in channels (`managed/is_last_step.py`).
2. If `Annotated[...]` metadata holds a `BaseChannel` instance or class, that
   channel is used. A `DeltaChannel` gets the outer type injected
   (`state.py:1876-1901`).
3. If the last item of `Annotated[...]` metadata is a callable with **exactly
   two positional parameters**, the key becomes
   `BinaryOperatorAggregate(typ, fn)`. Any other arity raises "Invalid reducer
   signature" (`state.py:1904-1922`).
4. Otherwise, the key becomes `LastValue(typ)`.

**`add_messages`** (`graph/message.py:61-244`) is just such a two-argument
reducer:

- It converts both sides to messages and assigns missing IDs.
- `RemoveMessage(REMOVE_ALL_MESSAGES)` truncates the list.
- A message whose ID already exists replaces the old one.
- `RemoveMessage(id)` deletes a message, and raises if the ID is unknown.
- Other messages are appended.

Its cost is O(len(left)) per write, and **it mutates its input messages'
`id` attributes in place** (`:203-208`).

**Schema merging:** the state, input, output, and every node or branch input
schema are merged into **one global channel namespace**. A conflicting
non-`LastValue` definition raises an error. Differences between `LastValue`
definitions are ignored silently (`state.py:343-373`).
`BinaryOperatorAggregate.__eq__` treats any lambda as equal to any other
operator (`binop.py:54-62`). Two schemas that declare different lambda reducers
for the same key therefore merge without error.

### 2.4 What compile() turns edges into

| Graph concept | Channel encoding |
|---|---|
| Node `X` trigger | `branch:to:X`, an `EphemeralValue(Any, guard=False)`. With `defer=True` it is a `LastValueAfterFinish` instead (`state.py:1525-1530`). |
| Static edge `A → B` | `A` gets an extra writer that writes `None` to `branch:to:B` (`state.py:1551-1559`). |
| Join `[A, B] → C` | A channel named `join:A+B:C` of type `NamedBarrierValue(str, {A, B})` is added to `C`'s triggers. Each source writes its own name to it (`state.py:1560-1575`). |
| Conditional edge from `A` | A branch writer is appended to `A`'s writers. It runs the path function and writes to `branch:to:<dest>` or emits `Send`s (`state.py:1577-1624`, `graph/_branch.py:122-225`). |
| `Command(goto=...)` returned by a node | The `_control_branch` mapper turns `goto` into `branch:to:` writes or `TASKS` writes. `graph=PARENT` raises `ParentCommand` (`state.py:1749-1775`). |
| Graph input | A `START` `EphemeralValue` and a hidden `START` node that copies input keys into state channels (`state.py:1350-1355`, `:1508-1514`). |

**Joins are durable by construction.** Barrier progress (`seen`) is a channel
value, so a checkpoint captures it with no extra work. Our `join_progress`
lives outside `State` and is not persisted.

---

## 3. Graph API (`graph/`)

### 3.1 Builder and compile

- **`StateGraph(state_schema, context_schema, input_schema=, output_schema=)`**
  (`state.py:216-270`) registers channels for all three schemas.
- **`add_node`** (`state.py:667-926`) does the following:
  - It coerces the node to a LangChain `Runnable`.
  - It infers the node's input schema from the first parameter's type hint.
  - It infers `Command[Literal[...]]` destinations from the return annotation.
    Those destinations are used only for drawing the graph.
  - It accepts `retry_policy`, `cache_policy`, `timeout`, `error_handler`,
    `defer`, and `destinations`.
  - It rejects names that contain `|` or `:` (`NS_SEP` and `NS_END`).
- **`add_edge`** with a list of start nodes creates a "waiting edge", which is a
  join (`state.py:928-980`).
- **`add_conditional_edges(source, path, path_map)`** (`state.py:982-1030`):
  if `path_map` is missing, the engine tries to infer one from a `Literal`
  return annotation (`_branch.py:96-116`).
- **`compile`** (`state.py:1177-1401`) validates the graph. It applies node
  defaults (retry, cache, timeout, error handler), builds the `CompiledStateGraph`
  (`stream_mode="updates"` by default for StateGraph), and attaches nodes,
  edges, and branches as described in §2.4. It also builds a msgpack
  deserialization allowlist from all schema types when
  `LANGGRAPH_STRICT_MSGPACK` is set (`state.py:1234-1254`).

### 3.2 Validation

`StateGraph.validate` (`state.py:1129-1175`) checks only three things:

- Every edge source is a known node or `START`.
- At least one edge leaves `START`.
- Every edge or branch target is a known node or `END`, and interrupt node
  names exist.

**It does not check reachability, dead ends, or a router mixed with static
edges.** A node with no outgoing edge simply ends its path. A conditional edge
with no `path_map` and no `Literal` annotation is treated as able to reach
every node (`state.py:1158-1162`). Path labels are only checked when a
`path_map` exists. `Pregel.validate` (`pregel/_validate.py:13-107`) checks
channel and node wiring. Our builder is far stricter (doc 0001, "Validation").

### 3.3 Conditional edge semantics: a key difference from AgentBuild

The branch reader uses `ChannelRead.do_read(..., fresh=True)`
(`state.py:1613-1619`). `local_read` with `fresh=True` **copies every channel
and applies only the current task's writes** (`_algo.py:211-219`). Two
consequences follow:

- A router on node A **cannot see writes made by sibling node B in the same
  step**. Our routers run after the merge and see merged state.
- Every router call costs O(#channels) channel copies.

Routing failures are node failures, so the node's retry policy applies to them
too. If there is no `path_map`, a path function may return any string. A string
that names no node becomes a write to a non-existent `branch:to:<typo>` channel,
which is **logged and dropped** (`_algo.py:308-313`). The graph simply stops
on that path.

### 3.4 `Send` (map-reduce fan-out)

`Send(node, arg, timeout=)` (`types.py:738-810`) can be returned from a
conditional edge or carried in `Command.goto`. Either way it is written to the
`__pregel_tasks` Topic. In the next step, each packet becomes a PUSH task whose
input is `arg`, not the graph state (`_algo.py:938-1107`). Results come back
through the normal channels, so a fan-in key needs a reducer (for example
`Annotated[list, operator.add]`). PUSH tasks get the `PUSH` trigger, so they do
not update `versions_seen`. A `Send` to an unknown node is logged and ignored
(`_algo.py:977-979`). A `Send` to `END` raises an error (`_branch.py:209-210`).

### 3.5 `Command`

`Command(graph=None|PARENT, update=..., resume=..., goto=...)`
(`types.py:833-882`) has two uses:

- **Returned from a node.** `update` goes through the same mapper as a dict
  return (`state.py:1456-1492`). `goto` goes through `_control_branch`. Lists
  of `Command`s are allowed. With `graph=PARENT`, the command raises
  `ParentCommand`, which bubbles up. `run_with_retry` in the parent graph
  catches it and applies it through the parent task's writers
  (`_retry.py:618-631`).
- **As graph input.** `map_command` (`pregel/_io.py:56-78`) turns `goto`,
  `resume`, and `update` into writes from `NULL_TASK_ID`. This is how a client
  resumes an interrupt, injects state, or jumps to a node.

### 3.6 Subgraphs

A compiled graph can be used as a node directly, or called inside a node
function.

- **Discovery** works by scanning the node's runnable tree *and the closure
  nonlocals of plain functions* (`pregel/_utils.py:45-72`).
- **Execution.** The child runs its own `PregelLoop` inside the parent task.
  It shares the parent's checkpointer through the config
  (`main.py:844-861`). Its checkpoint namespace is
  `parent_node:<task_id>|child...`. `CONFIG_KEY_CHECKPOINT_MAP` maps each
  ancestor namespace to that ancestor's checkpoint id.
- **Checkpointer modes.** `checkpointer=None` inherits the parent's,
  `False` disables checkpointing, and `True` keeps one history per thread for
  that subgraph (`types.py:109-128`, `main.py:869-884`).
- **Repeated calls.** Calling the same subgraph several times in one node is
  told apart by `scratchpad.subgraph_counter` (`_loop.py:340-355`).
- **Interrupts and replay.** A child interrupt bubbles up as `GraphInterrupt`
  and is saved on the parent task. On resume the parent node runs again, and
  the child resumes from its own checkpoint (`CONFIG_KEY_RESUMING`). Time
  travel into subgraphs needs `ReplayState` and before-bound lookups
  (`_loop.py:1093-1135`, `:1708-1720`), which is some of the most intricate code
  in the repo.

### 3.7 Input/output schemas and private state

- **Input.** The `START` node writes only keys that belong to the input schema
  (`state.py:1445-1450`). Input keys that are not channels produce a warning,
  not an error (`pregel/_io.py:93-97`).
- **Output.** Output channels are the output schema's keys
  (`state.py:1270-1279`).
- **Private state.** Any node may write to *any* channel declared in *any*
  schema (`output_keys = all builder channels`, `state.py:1452-1454`). A
  "private" key is one that appears in a node's input schema but not in the
  state schema. It is still a global channel and is still checkpointed.
- **Input mapping.** A node's input is the subset of channels in its input
  schema, mapped to a Pydantic model or dataclass if the schema is one
  (`state.py:1515-1523`, `:1732-1746`).

---

## 4. Functional API (`func/`)

- **`@entrypoint(checkpointer=, store=, cache=, context_schema=, retry_policy=, ...)`**
  compiles a function into a single-node `Pregel` (`func/__init__.py:516-620`).
  The graph has channels `START` (`EphemeralValue`), `END` (`LastValue`, the
  return value), and `PREVIOUS` (`LastValue`, the value saved for the next
  invocation on the same thread). `entrypoint.final(value=, save=)` lets the
  returned value differ from the saved one (`:475-514`). Generators are
  rejected (`:526-529`).
- **`@task`** wraps a function in `_TaskFunction` (`func/__init__.py:59-99`).
  Calling it reads `CONFIG_KEY_CALL` from the context-local config
  (`pregel/_call.py:276-298`) and returns a `SyncAsyncFuture`. The runner's
  `_call` (`_runner.py:702-788`) then does three things:
  - It creates a PUSH task whose id derives from the parent path and
    `scratchpad.call_counter()` (`_algo.py:800-935`).
  - **If that task id already has writes in the checkpoint (a resume), it
    returns the saved `RETURN` value or error without re-running**
    (`_runner.py:747-758`).
  - Otherwise it schedules the task "next tick", so that the current tick's
    writes are streamed first.
- **Mapping onto Pregel.** The entrypoint body *is* one long-running task. Each
  `@task` call is a dynamically pushed sibling task that runs in the same
  superstep. Durability comes from re-running the entrypoint from the top and
  replaying task results by call order. User code must therefore be
  deterministic in the order and number of task calls. `interrupt()` inside an
  entrypoint uses the same mechanism.

---

## 5. Checkpointing and durability (`checkpoint/`, `pregel/_checkpoint.py`, `pregel/_loop.py`)

### 5.1 Data model

`Checkpoint` (`checkpoint/base/__init__.py:92-123`) has these fields:

- `v`, the format version (`LATEST_VERSION = 2`, `:811`; the field docstring
  still says 1).
- `id`, a uuid6 that is monotonic within a thread.
- `ts`.
- `channel_values`, the channel checkpoints.
- `channel_versions`.
- `versions_seen`, a map from node to {channel: version}.
- `updated_channels`.

`CheckpointMetadata` (`:38-86`) records `source` (`input`/`loop`/`update`/`fork`),
`step` (-1 for the input checkpoint), `parents` (namespace to checkpoint id),
`run_id`, and the delta counters. `CheckpointTuple` (`:139-146`) bundles
config, checkpoint, metadata, `parent_config`, and `pending_writes`.
`PendingWrite = (task_id, channel, value)` (`:31`).

**Addressing.** A checkpoint is found by `configurable.thread_id`,
`configurable.checkpoint_ns` (`""` for the root graph), and
`configurable.checkpoint_id`. Without a `checkpoint_id`, the saver returns the
latest checkpoint. A thread is a linked list, or a tree after forks, of
checkpoints chained by parent id.

### 5.2 Saver interface

`BaseCheckpointSaver` (`checkpoint/base/__init__.py:176-711`) defines:

- `get_tuple`, `list(filter, before, limit)`, and `put(config, checkpoint,
  metadata, new_versions)`.
- `put_writes(config, writes, task_id, task_path)`.
- `delete_thread`, `delete_for_runs`, `copy_thread`, `prune`.
- `get_delta_channel_history`.
- `get_next_version`.
- Async twins of all of the above.

`put` receives **only the versions that changed**. `InMemorySaver.put` stores
one blob per `(thread, ns, channel, version)` and stores the checkpoint without
`channel_values` (`checkpoint/memory/__init__.py:442-458`). Unchanged channels
are therefore not rewritten, but a changed channel is rewritten in full.
`InMemorySaver` versions are zero-padded counters with a random fractional
suffix (`:613-622`), which keeps blob keys from colliding across forks.

### 5.3 When checkpoints and writes are written

- **Input checkpoint.** Saved after input is applied (`source="input"`,
  `_loop.py:1088-1090`).
- **Step checkpoint.** Saved at the end of every step in `after_tick`
  (`source="loop"`, `_loop.py:748`).
- **Fork checkpoint.** Saved on time-travel resume (`source="fork"`,
  `_loop.py:1015-1028`).
- **Update checkpoint.** Saved by `update_state` (`source="update"`).
- **Pending writes.** Every task's writes are saved **as soon as it finishes**
  (`PregelRunner.commit` calls `loop.put_writes`, `_loop.py:430-523`). The
  writes are attached to the checkpoint id of the step being executed.
- **Ordering.** Checkpoint puts are chained, so each waits for the previous
  one (`_checkpointer_put_after_previous`, `_loop.py:1596-1613`). Write puts are
  submitted independently and are not ordered after the checkpoint they refer
  to. Only `DeltaChannel` writes are explicitly flushed before the next
  checkpoint (`_loop.py:205-211`, `:1604-1606`).

**Durability modes** (`types.py:98-104`; the default is `"async"`,
`main.py:2671-2672`):

| Mode | Behaviour | Code |
|---|---|---|
| `sync` | Waits for the checkpoint future before the next step starts. | `main.py:3054-3055` |
| `async` | Puts the checkpoint in the background while the next step runs. | `_loop.py:1263-1273` |
| `exit` | Skips `put_writes` and intermediate checkpoints. Persists once in the loop's exit handler, which also covers errors and interrupts. | `_loop.py:481`, `:1194-1196`, `:1389-1400` |

### 5.4 Resume semantics

`_first` (`_loop.py:860-1140`) decides between three cases:

- **Resume** happens when a prior checkpoint exists and the input is `None`, a
  `Command`, or comes from the same `run_id` (`:871-884`). The engine then
  applies `NULL_TASK_ID` writes such as `Command.update` and `goto`
  (`:983-997`). It marks every channel as seen by the pseudo-node `INTERRUPT`
  (`:999-1004`), so that `interrupt_before` does not fire again immediately,
  and emits the current values.
- **New input on an existing thread.** The engine first *discards* the
  unfinished tasks of the old checkpoint by consuming their triggers, then
  applies the input (`:1033-1066`).
- **Pending writes on resume.** In `tick()`, tasks whose ids appear in the
  loaded pending writes and are finished get their writes back. The runner
  skips them because their `writes` are non-empty. Only unfinished tasks run
  again (`_loop.py:677-679`, `:772-783`, `main.py:3035`).

### 5.5 Time travel, fork, `update_state`

- **Time travel.** Invoking with an explicit `checkpoint_id` sets
  `is_replaying` (`_loop.py:330`). Loaded writes are then *not* reapplied, so
  tasks re-execute (`_loop.py:766-770`). `RESUME` writes are dropped so that
  interrupts fire again (`:886-912`). When resuming from an old checkpoint, a
  new `fork` checkpoint is written so the new branch has its own head
  (`:1015-1028`).
- **`update_state(config, values, as_node)`** and **`bulk_update_state`**
  (`main.py:1615-2105`, with an async copy at `:2106-2592`) write a new
  `source="update"` checkpoint. The update is applied by **running the
  writers of `as_node`**, including its edges and branches, so that the next
  step's tasks are computed as if that node had produced the values
  (`main.py:1986-2012`). `as_node` is inferred from `versions_seen` when not
  given, and is an error when ambiguous (`:1964-1968`). The special values
  `as_node=INPUT`, `END`, and `"__copy__"` cover input injection, clearing
  tasks, and cloning (`:1693-1880`).
- **`get_state` and `get_state_history`** (`main.py:1460-1592`) rebuild
  channels from a checkpoint and call `prepare_next_tasks(for_execution=False)`
  to compute `next`. They also attach pending interrupts and errors.

### 5.6 Serialization

`JsonPlusSerializer` (`checkpoint/serde/jsonplus.py:82-290`) works as follows:

- Its `dumps_typed` returns `(type_tag, bytes)` with the tags `null`, `bytes`,
  `msgpack`, or optionally `pickle`.
- msgpack goes through `ormsgpack`, with extension types for Pydantic v1/v2,
  namedtuples, dataclasses, enums, sets, datetimes, UUIDs, Decimals, numpy
  arrays, and so on. **Each extension type stores the Python module and class
  name, and decoding re-imports that class** (`jsonplus.py:295-420`,
  `:633-700`).
- Because decoding imports arbitrary classes, there is an allowlist
  (`allowed_msgpack_modules`, the `LANGGRAPH_STRICT_MSGPACK` environment
  variable, and a per-graph allowlist derived from schemas,
  `jsonplus.py:92-155`, `state.py:1234-1254`).
- `EncryptedSerializer` wraps any serde with a cipher
  (`serde/encrypted.py:8-80`).
- `BaseCache` uses the same serde, without pickle (`cache/base/__init__.py:18`).

---

## 6. Interrupts and human-in-the-loop

- **Raising.** `interrupt(value, response_schema=)` (`types.py:893-1035`)
  reads the task scratchpad from the context-local config. It increments
  `interrupt_counter`. If `scratchpad.resume[idx]` exists, it returns that
  value. Otherwise it takes the run-level "null resume" value (from
  `Command(resume=v)`) if there is one. Otherwise it raises `GraphInterrupt`
  with `Interrupt.from_ns(value, ns=task checkpoint ns)`. The resume list is
  saved as a `RESUME` write each time a value is consumed (`:1016`, `:1024`).
- **Saving.** On `GraphInterrupt`, the runner saves `(INTERRUPT, interrupts)`
  plus any `RESUME` writes for that task (`_runner.py:586-592`). Siblings are
  not cancelled, because `GraphBubbleUp` is not a failure
  (`_runner.py:618-634`). They finish, and their writes are saved. The root
  loop suppresses the exception. It emits a final `values` chunk with pending
  writes applied, plus an `__interrupt__` update, and returns normally
  (`_loop.py:1402-1441`).
- **Resuming.** The client calls `invoke(Command(resume=v), same thread)`.
  `map_command` writes `(NULL_TASK_ID, RESUME, v)`. If several interrupts are
  pending, the client must pass a map from interrupt id to value, or the run
  raises an error (`_loop.py:955-965`). A map is routed through
  `CONFIG_KEY_RESUME_MAP` and matched to a task by the hash of its namespace
  (`_algo.py:1311-1314`).
- **Re-execution.** The interrupted task has no output writes, so it is
  **re-executed from the top** (§5.4). Side effects before `interrupt()` run
  again. The docstring says so explicitly (`types.py:907-912`). Answers are
  matched **by position**. Interrupt ids derive from the task's namespace
  (`types.py:635-646`, `:1029-1031`), not from the call site. A node that
  calls `interrupt()` conditionally or in a different order on replay will
  receive the wrong answers.
- **Static breakpoints.** `interrupt_before` and `interrupt_after` (at compile
  time or per call) use `should_interrupt` (`_algo.py:155-185`). It fires only
  if some channel changed since the last interrupt, which is tracked through
  `versions_seen[INTERRUPT]`.
- **Requirements.** A checkpointer is required (`_loop.py:950-953`).
  `NodeInterrupt` is deprecated (`errors.py:110-126`). Typed responses use
  `response_schema`, which is validated with a Pydantic `TypeAdapter` on
  resume (`types.py:1003-1015`).

---

## 7. Streaming

Stream modes (`types.py:131-145`) are `values`, `updates`, `checkpoints`,
`tasks`, `debug`, `messages`, and `custom`. There is also an undocumented
`tools` mode in the engine (`main.py:2897-2905`) that is missing from the
`StreamMode` literal.

| Mode | Emitted when / how |
|---|---|
| `values` | After each step whose updates touch output keys (`_loop.py:730-737`). Also on resume (`:1030-1032`) and at an interrupt, together with the interrupt (`:1406-1430`). |
| `updates` | **Per task, as soon as it finishes** (`put_writes` calls `output_writes`, `_loop.py:521-523`, `:1518-1525`). Also on `__interrupt__`. |
| `checkpoints` / `tasks` | Snapshot-shaped events built by `map_debug_*` in `tick()` and `put_writes`. `debug` re-wraps both with a step number and timestamp (`_loop.py:1455-1480`). |
| `custom` | The node calls a `StreamWriter`. It reaches the node as the `writer` kwarg, as `runtime.stream_writer`, or through `get_stream_writer()`. The writer puts `(ns, "custom", payload)` on the queue, reading the namespace from the context-local config (`main.py:2908-2927`). |
| `messages` | A LangChain callback handler, `StreamMessagesHandler`, is attached as an inheritable handler (`main.py:2878-2894`). It turns `on_llm_new_token` into `(chunk, metadata)` pairs, using the `langgraph_*` metadata the engine stamps on each task (`_algo.py:654-660`). It skips models tagged `nostream` (`_messages.py:130-149`). It emits final messages from `on_llm_end` and **messages found in node outputs** (`on_chain_end`), deduplicated by message id (`_messages.py:94-103`, `:166-247`). `push_message()` emits manually (`graph/message.py:392-437`). |

- **Transport.** A `SyncQueue` or async queue is fed by the loop and the
  handlers. The generator drains it between runner yields (`main.py:3041-3050`).
- **Subgraphs.** With `subgraphs=True`, chunks become
  `(namespace_tuple, mode, data)` and child loops write into the parent's
  stream (`main.py:3000-3001`, `_loop.py:338-339`).
- **Output versions.** There are three output formats:
  - v1 is raw tuples.
  - v2 is typed `StreamPart` dicts (`types.py:273-378`).
  - v3 is `stream_events(version="v3")`, an experimental transformer pipeline
    over "protocol events" (`stream/__init__.py:1-6`, `main.py:3703-3770`).
- **Token streaming needs LangChain.** It works only for code that emits
  LangChain callbacks, in practice LangChain chat models running in a
  propagated config context.

---

## 8. Store, cache, runtime and context, config

- **`Runtime`** (`runtime.py:125-293`) is a frozen dataclass with these fields:
  `context` (an immutable per-run user context validated against
  `context_schema`), `store`, `stream_writer`, `heartbeat`, `previous`
  (functional API), `execution_info` (checkpoint_id, ns, task_id, thread_id,
  run_id, attempt number, first-attempt time, `:27-58`), `server_info`, and
  `control`.
  - It is built once per run (`main.py:2939-2949`). It is merged with the
    parent's `Runtime` for subgraphs and overridden per task
    (`_algo.py:688-701`).
- **Injection** is signature-driven. `RunnableCallable` inspects the node
  function's parameters for `config`, `writer`, `store`, `previous`, `runtime`,
  and `error` (`_internal/_runnable.py:166-229`, `:338-364`). Some of these
  match by annotation and some by name only. `get_config()`, `get_store()`,
  `get_stream_writer()`, and `get_runtime()` read a contextvar
  (`config.py:17-29`). In async code this only works on Python 3.11 or later.
- **Store** (`store/base/__init__.py:708-1300`) is cross-thread long-term memory:
  - It is a hierarchical key-value store over `namespace: tuple[str, ...]` and
    `key`.
  - Every operation goes through `batch(ops)` (`GetOp`, `SearchOp`, `PutOp`,
    `ListNamespacesOp`). Methods such as `get`, `put`, `search`, `delete`, and
    `list_namespaces` are conveniences on top.
  - It supports optional TTL (`TTLConfig`, `:545`) and an optional semantic
    index with embeddings (`IndexConfig`, `:578`).
  - It is independent of checkpoints.
- **Cache** (`cache/base/__init__.py:15-48`) caches node and task outputs
  (their *writes*) under `(CACHE_NS_WRITES, identifier(func), node)` plus an
  xxh3 hash of `CachePolicy.key_func(input)`. The default key function pickles
  the input (`types.py:530-538`). The cache is checked before execution
  (`_loop.py:1615-1628`) and written in `put_writes` (`:1675-1691`). Cached
  tasks never go through `commit`, so they read as "not finished" in task
  status (`_task_status.py:40-42`).
- **Managed values** `IsLastStep` and `RemainingSteps`
  (`managed/is_last_step.py`) are fake state keys computed from
  `scratchpad.step` and `scratchpad.stop`.
- **Config** is LangChain's `RunnableConfig`. It carries `recursion_limit`,
  `max_concurrency`, `callbacks`, `tags`, `metadata`, and `configurable`. The
  engine stores its internals under reserved `configurable` keys
  (`_internal/_constants.py`).

---

## 9. Errors (`errors.py`)

| Type | Meaning |
|---|---|
| `GraphRecursionError(RecursionError)` | The step limit was reached (`GRAPH_RECURSION_LIMIT`, `errors.py:67-87`). |
| `InvalidUpdateError` | More than one write to a `LastValue` or guarded channel in a step (`INVALID_CONCURRENT_GRAPH_UPDATE`), or a node returned something that is neither a dict nor a `Command` (`INVALID_GRAPH_NODE_RETURN_VALUE`, `state.py:1487-1492`). Also raised for a bad `Overwrite`, a Send to END, and similar cases. |
| `GraphBubbleUp` | The base class for control flow that passes through user code: `GraphInterrupt`, `ParentCommand`, `GraphDrained`. Retries skip these (`_retry.py:632-634`), and fail-fast does not treat them as failures. |
| `GraphInterrupt` / `NodeInterrupt` (deprecated) | Interrupt signalling. The root graph suppresses it. |
| `GraphDrained` | A cooperative drain finished. The run is resumable. |
| `EmptyInputError` | No input, or an empty `Command`, on a fresh run (`_loop.py:972-973`, `:1091-1092`). |
| `NodeTimeoutError` | A per-node idle or run timeout. It is retryable by default. |
| `NodeCancelledError` | User code raised `CancelledError`. |
| `NodeError` (dataclass) | The context passed to error handlers. |
| `TaskNotFound` | Used in distributed mode. |

Error codes (`errors.py:34-39`) link to troubleshooting pages. `create_error_message`
appends a link. The `MULTIPLE_SUBGRAPHS` code is declared but not raised
anywhere in this version.

---

## 10. Pain points and design debt

These are ordered roughly by how much they should influence AgentBuild.

1. **The plumbing is an untyped dict.** Tasks talk to the engine through
   `config["configurable"]` keys such as `__pregel_send`, `__pregel_read`,
   `__pregel_scratchpad`, `__pregel_runtime`, `__pregel_checkpointer`,
   `checkpoint_map`, `__pregel_call`, and `__pregel_resume_map`
   (`_internal/_constants.py:36-75`, `_algo.py:721-749`, `_retry.py:587-612`). Many user-facing APIs reach this
   dict through a contextvar (`config.py:17-29`, `types.py:1002`). The result
   cannot be type-checked, is hard to trace, and is unavailable on Python
   below 3.11 in async code. In Rust, Node.js, or a PyO3 bridge, this "ambient
   config" has no clean equivalent and needs explicit handles.
2. **Sync and async are duplicated almost everywhere.**
   - `SyncPregelLoop` and `AsyncPregelLoop` (`_loop.py:1535`, `:1791`).
   - `tick` and `atick` (`_runner.py:177`, `:361`).
   - `_call` and `_acall`.
   - `run_with_retry` and `arun_with_retry`.
   - `stream` and `astream` (about 370 and 440 lines, `main.py:2724`, `:3130`).
   - `bulk_update_state` and `abulk_update_state` (about 490 lines each,
     `main.py:1615`, `:2106`).
   - Every checkpointer method has an `a`-prefixed twin.

   Bugs get fixed in one copy and not the other. A Rust core with a single
   async runtime avoids this entirely.
3. **The sync engine cannot really cancel or time out.** Futures that have
   already started cannot be cancelled (`_executor.py:59`), and the executor
   waits for every thread on exit (`:43-46`). Per-node timeouts are refused for
   sync nodes (`_internal/_timeout.py:8-25`). Retries `time.sleep` on a pool
   thread (`_retry.py:674`). Even in async mode, timeouts are cooperative and a
   CPU-bound node blocks them (`types.py:464-468`).
4. **Typos fail quietly instead of loudly.**
   - An unknown `goto` or route target is a write to a channel that does not
     exist, which is **logged and ignored** (`_algo.py:308-313`).
   - An unknown `Send` target is ignored (`_algo.py:977-979`).
   - Unknown input keys produce a warning (`_io.py:93-97`).
   - Graph validation skips reachability and dead-end checks
     (`state.py:1129-1175`).
   - Different `LastValue` declarations for the same key merge silently
     (`state.py:355-363`), and any two lambda reducers compare equal
     (`binop.py:54-62`).

   In practice, a typo makes the graph stop early with no error.
5. **Resume replays node code, and correctness depends on call order.** This
   applies to interrupts (by call index, `types.py:1008-1025`), functional
   tasks (by `call_counter`, `_runner.py:724-758`), and subgraphs (by
   `subgraph_counter`, `_loop.py:340-355`). All of them require the code before
   the resume point to be deterministic and idempotent. Several interrupts in
   one task share a namespace-derived id (`types.py:1029-1031`). This is a
   fundamental limit for Python, and for us too, because we cannot persist
   Python or JavaScript stacks. It should be an explicit, documented contract,
   preferably with explicit keys instead of positional matching.
6. **Partial failures have unusual edge cases.** A task that emitted output
   and then raised counts as finished on resume (`_task_status.py:43-44`).
   Cached tasks count as unfinished (`:40-42`). *(by inspection)* A same-key
   `LastValue` conflict is detected in `after_tick`, after both tasks' writes
   are already durable. A later resume re-applies the same writes and hits the
   same conflict again (`_loop.py:677-679`, `:722-728`). The thread stays
   stuck until someone calls `update_state` or forks.
7. **Persistence is coupled to Python.** Checkpoints store msgpack with
   Python module and class names, and decoding re-imports them
   (`jsonplus.py:305-420`, `:633-700`). That requires an allowlist for safety
   (`:92-155`) and has an optional pickle fallback (`:269-270`, `:287-288`). A
   TypeScript reader cannot consume a Python thread. Typed state is a set of
   Python objects rather than data.
8. **Each step pays for copying and serialization.**
   - Every step calls `copy_checkpoint` (`_loop.py:1270`), runs
     `channel.checkpoint()` for every channel (`pregel/_checkpoint.py:224-296`),
     and re-serializes every *changed* channel in full
     (`memory/__init__.py:446-449`).
   - Routers copy every channel to build the "fresh" view (`_algo.py:211-219`).
   - The first task per input shape gets a shallow copy of the cached input
     dict (`_algo.py:1358-1360`).
   - Each task builds a new merged config dict (`_algo.py:706-759`).
   - A growing `messages` list therefore costs O(n) bytes per step and O(n²)
     per thread. `DeltaChannel`, still in beta, is the fix. It adds about 15
     loop fields of bookkeeping for snapshot cadence, exit-mode accumulators,
     and forced snapshots (`_loop.py:205-259`, `:1157-1226`, `:1285-1380`).
   - Python overhead on top of this includes LangChain `Runnable` wrapping,
     callback managers on every task (`_algo.py:716-720`), and per-task
     `inspect.signature` checks at node construction
     (`_runnable.py:338-364`).
9. **Routers see a different state than you would expect.** A router reads
   pre-step state plus its own node's writes (`_algo.py:211-219`), not the
   merged step state. In a fan-out, a router on one branch cannot see what a
   sibling branch wrote in the same step. The documentation rarely mentions
   this.
10. **Hidden channel encoding leaks out.** Users meet names like
    `branch:to:X`, `join:A+B:C`, and `__pregel_tasks` in checkpoints, debug
    streams, and `update_state`. `update_state(as_node=...)` re-runs that
    node's writers and routers to work out what comes next
    (`main.py:1986-2012`), so editing state can trigger routing code.
11. **Magic introspection everywhere.**
    - Reducers are detected by Annotated metadata arity
      (`state.py:1904-1922`).
    - Node input schemas come from the first parameter's annotation, and
      `Command[Literal]` return types become graph destinations
      (`state.py:813-865`).
    - Dependencies are injected by parameter name and annotation
      (`_runnable.py:166-229`).
    - Subgraphs are discovered by inspecting closure nonlocals
      (`_utils.py:45-72`).

    Clever in Python, impossible to mirror in Rust or TypeScript, and
    surprising when it misfires.
12. **Durability ordering is subtle.** In `async` mode, pending writes for
    step N+1 may land before checkpoint N is stored (`_loop.py:496-509` vs
    `:1263-1273`). Only delta-channel writes are ordered (`:205-211`). Savers
    must tolerate orphan writes. The exit-time code also has an acknowledged
    hack that tells "exiting" apart by object identity (`_loop.py:1143-1153`).
13. **The API surface keeps churning.**
    - Renames: `config_schema` to `context_schema`, `input` and `output` to
      `*_schema`, `checkpoint_during` to `durability`, `retry` to
      `retry_policy`, and `interrupt_id` to `id` (`state.py:225-250`,
      `main.py:2794-2804`, `types.py:648-656`).
    - Streaming has three output versions plus three `stream_events` versions
      (`main.py:2739`, `:3708`).
    - Checkpoints have their own migrations (`_migrate_checkpoint`,
      `state.py:1626`).
14. **LangChain is a hard dependency.** Nodes must be `Runnable`s
    (`coerce_to_runnable`), and tracing, `messages` streaming, `tools`
    streaming, and idle-timeout progress all depend on LangChain callbacks
    (`_retry.py:274-312`, `_messages.py`). A node that calls a non-LangChain LLM
    client gets no token streaming without manual `writer` calls.
15. **The default step limit is not a safety net.** The default
    `recursion_limit` is 10007 (`_internal/_config.py:32`), so a buggy
    agent loop runs about ten thousand steps, with matching LLM spend, before
    it stops.

---

## 11. Implications for AgentBuild

Verdicts: **Adopt** means take the idea mostly as is. **Adapt** means take the
idea with a different mechanism. **Avoid** means do not copy it.

| Concept | Verdict | Reason (one line) |
|---|---|---|
| BSP supersteps, writes invisible until step end | Adopt (already) | Same model as ours and validated at scale; it gives a clean checkpoint boundary. |
| Deterministic write order (sort by task path, not completion) | Adopt (already, extend) | We sort by declaration order. Extend the order to Send tasks, for example by (origin node, emit index). |
| `LastValue` concurrent-write error | Adopt (already) | Identical to `update_conflict`, and it should stay the default for keys without a reducer. |
| Channels as the *user-facing* abstraction | Avoid | Seven channel classes leak engine internals. Expose per-key reducers and keep internal scheduling state private. |
| Version-vector triggering (`channel_versions` / `versions_seen`) | Adapt | Our explicit frontier plus join progress is simpler. Persist the frontier directly instead of deriving it from versions. Use per-key versions only for incremental blob storage. |
| Per-key reducers (`Annotated[T, fn]`) | Adopt, language-neutral | Rust ships built-in reducers (append, extend, merge-object, add, messages-by-id). Custom reducers are SDK callbacks declared explicitly, never inferred from arity. |
| `Overwrite` escape hatch | Adopt | It is cheap and lets one node reset a reducer key, with at most one per step. |
| `DeltaChannel` | Avoid (for now) | Design storage as an append-only write log plus periodic snapshots from day one instead of retrofitting it. |
| Ephemeral / untracked keys | Adapt | Offer a "transient" key flag (not persisted, cleared after the step) instead of separate channel types. |
| Barrier-channel joins | Adapt | We have joins. Take the lesson that join progress is state and belongs in the checkpoint. |
| `defer=True` (run when nothing else is scheduled) | Adapt (later) | It is useful for fan-in after uneven branches. Implement it as a frontier rule, not a channel trick. |
| Routers inside the node task with a "fresh" local read | Avoid | Our routers that see merged state are easier to reason about. Keep them and document the difference. |
| `Send` fan-out | Adopt | Map-reduce is essential for agents. It needs the task-model change listed below. |
| `Command(update, goto)` from nodes | Adapt | Allow it, but validate `goto` against declared targets at runtime and **fail** on unknown targets. Never warn-and-drop. |
| `Command.PARENT` | Avoid (initially) | It couples parent and child graphs through exceptions. Revisit after subgraphs ship. |
| Subgraphs | Adapt | Make them an explicit subgraph node kind (no closure introspection), with namespaced checkpoints `parent_node/task_id`. |
| Input/output/private schemas | Adapt | Filter input and output keys in the core cheaply. Do schema validation at the SDK edge. Unknown input keys should be an error. |
| Functional API (`@entrypoint` / `@task`) | Avoid for v1 | Replay by call order needs deterministic user code. We could offer it later as SDK sugar over `Send` plus durable task results. |
| **Per-task durable pending writes** | **Adopt** | This is the key durability idea: the state commit stays atomic, and finished tasks are not re-run after a crash, failure, or interrupt. |
| Checkpoint model (thread, id, parent, step, source, versions) | Adopt, with JSON | Our state is already JSON. Store it as JSON or CBOR so Python, TypeScript, and Rust share threads. Never embed language type names. |
| Incremental blobs per changed key | Adopt | Store only the keys whose version changed. The cost is near zero once keys carry versions. |
| Durability modes `sync` / `async` / `exit` | Adopt, with stated ordering | Same knobs, but guarantee "writes are durable only after their parent checkpoint" so there are no orphan writes. |
| Time travel and fork via parent pointers | Adopt (later) | It falls out of the data model. Keep `update_state` simple: patch state and set the frontier, without re-running routers. |
| `interrupt()` with re-execution from the top | Adapt | Re-execution is the only portable option. Key answers by an explicit interrupt key (`interrupt(key, payload)`) instead of position, and document idempotency requirements. |
| Static `interrupt_before` / `interrupt_after` | Adopt | They are simple and need no node cooperation. |
| Stream modes `values`, `updates` (per task, as it finishes), `custom`, `tasks`/`debug` | Adopt | The core emits typed events on one bus, and SDKs adapt them to iterators. |
| `messages` streaming through LangChain callbacks | Avoid | Give the SDK an explicit token-emitter API (a `writer` with a message-chunk event type) that is LLM-library agnostic. |
| `Store` (namespaced key-value, separate from checkpoints) | Adopt (later) | It is the right separation between long-term memory and thread state. Make it a trait, injected through context. |
| Node cache | Adapt (later) | Key it by a canonical-JSON input hash, not pickle. Cached tasks must count as finished. |
| `Runtime` / context injection | Adapt | Pass an explicit `Context` (run context, store, writer, execution info, cancellation token) as an argument. No contextvars, and no injection by parameter name. |
| `RetryPolicy` (backoff, jitter, retry_on) | Adopt | Implement it in the Rust core. Match `retry_on` against `NodeError.kind`, never sleep on a worker thread, and keep routers separate from node retries. |
| `TimeoutPolicy` (run plus idle/heartbeat) | Adopt | Tokio can cancel at `.await`. Document that bridged sync Python nodes are only cooperatively cancellable. |
| Node error handlers | Adapt (later) | Useful, but model them as "on error, route to node X with the error in context" within the same step semantics. |
| `RunControl` drain | Adopt | Cooperative stop at a step boundary followed by a checkpoint gives graceful shutdown for servers. |
| Step limit | Keep ours | Our default of 64 is safer than 10007, and it should stay a hard error. |
| Managed values (`RemainingSteps`) | Adapt | Expose step and remaining steps in the execution info, not as fake state keys. |

### Changes our core model would need

1. **The merge rule needs reducers.** A key with a reducer folds all of a
   step's writes in deterministic task order, with an optional single
   `Overwrite`. A key without a reducer keeps today's `update_conflict` rule.
   Doc 0001's statement that "updates replace whole keys" becomes the
   reducer-less default.
2. **The frontier must hold tasks, not node ids.** Today the frontier is a
   `BTreeSet<usize>` (`runtime.rs:89`). Each frontier entry must become
   `Task { id, node, input: State | Payload, origin }` so that `Send` can run
   the same node several times with different inputs in one step. Task ids
   must be deterministic, for example hash(checkpoint_id, step, node, origin,
   index), and stable across a resume.
3. **Node results must allow commands.** Node output changes from `Update` to
   `Update | Command { update, goto: [Target | Send] }`. The runtime checks
   `goto` targets against the declared graph and fails with `unknown_route`.
   Validation must decide how `goto` interacts with the "router xor static
   edges" rule.
4. **Atomicity must be restated.** The rule becomes: the *state* commit is
   all-or-nothing, but each finished task's update is recorded durably and
   individually. A failed or interrupted step then re-runs only the tasks that
   did not finish. This requires the persisted run state to hold a per-step
   write log keyed by task id.
5. **Checkpoints must include scheduling state.** Today `join_progress` and the
   frontier live only in `Run` (`runtime.rs:78-83`). A checkpoint must store
   the step number, the frontier (pending tasks including `Send` payloads),
   join progress, pending interrupts, and the write log. This is LangGraph's
   lesson: everything that decides "what runs next" must be persisted.
6. **Runs need a non-error "interrupted" outcome.** `RunOutput` gains a status
   (`Done | Interrupted { interrupts } | Drained`) and resume input. Interrupts
   in one task must not cancel siblings, unlike fail-fast for errors.
7. **Router timing must be decided explicitly.** Routers keep running
   centrally after the merge. If we add `Command.goto`, a node's own routing
   decision becomes part of its update, which mixes both styles. We must
   define which one wins.
8. **The runtime needs an event bus.** It should emit `TaskStarted`,
   `TaskFinished(update)`, `StepCommitted(values)`, `Custom(payload)`,
   `MessageChunk`, `Interrupted`, and `CheckpointSaved` events, with
   subgraph namespaces. Nodes get a writer handle through `Context`.
9. **Nodes need a context argument.** The `Node::run` signature
   (`node.rs:21`) gains a `Context` argument carrying run context, execution
   info, writer, store, a cancellation token, and resume values. This keeps the
   bridge free of ambient-state magic.
