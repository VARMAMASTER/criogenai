"use client";
// One pattern's story, as a run stage in the home page's scene style: the
// graph takes the run's state colours as it moves, a log of the run fills in
// beside it, a person's decision appears on the stage, and the answer, the
// time split and the full transcript follow. Scripted by default; with a key
// in KeyPanel, a real model makes the decisions.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { makeCtx, MAX_MODEL_CALLS, type Decision, type Line, type Timing } from "@/lib/patterns/engine";
import { runOnEngine } from "@/lib/patterns/live";
import { byId } from "@/lib/patterns";
import Diagram, { edgeKey, type RunState } from "./Diagram";
import KeyPanel from "./KeyPanel";
import { useKey } from "./KeyProvider";

// The log's colour for each kind of line (the run states of styles/scene.css).
const TONE: Record<Line["kind"], string> = {
  user: "note",
  model: "run",
  tool: "run",
  note: "note",
  state: "done",
  final: "done",
  error: "fail",
  human: "wait",
};

const KIND_LABEL: Record<Line["kind"], string> = {
  user: "input",
  model: "model",
  tool: "tool",
  note: "note",
  state: "state",
  final: "answer",
  error: "check",
  human: "person",
};

export default function Story({ id, code }: { id: string; code: ReactNode }) {
  const def = byId(id)!;
  const { conn } = useKey();
  const [input, setInput] = useState(def.task);
  const [params, setParams] = useState<Record<string, number>>(() => Object.fromEntries((def.params ?? []).map((p) => [p.id, p.value])));
  const [speed, setSpeed] = useState(1);
  const [lines, setLines] = useState<Line[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [edge, setEdge] = useState<[string, string] | null>(null);
  const [visited, setVisited] = useState<Set<string>>(new Set());
  const [taken, setTaken] = useState<Set<string>>(new Set());
  const [failedAt, setFailedAt] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [open, setOpen] = useState<Set<number>>(new Set());
  const activeRef = useRef<string | null>(null);
  const ownLine = useRef(1_000_000);
  const note = (who: string, kind: Line["kind"], text: string) =>
    setLines((ls) => [...ls, { id: ownLine.current++, who, kind, text }]);
  const [counts, setCounts] = useState({ model: 0, tool: 0 });
  const [busy, setBusy] = useState(false);
  const [running, setRunning] = useState(false);
  const [answer, setAnswer] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [waiting, setWaiting] = useState<{ draft: string; answer: (d: Decision) => void } | null>(null);
  const [edit, setEdit] = useState("");
  const [took, setTook] = useState<number | null>(null);
  const [timing, setTiming] = useState<Timing | null>(null);
  const stop = useRef<AbortController | null>(null);
  const logRef = useRef<HTMLOListElement>(null);
  const live = conn != null && def.usesModel;

  useEffect(() => () => stop.current?.abort(), []);
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines, waiting]);

  const reset = () => {
    setLines([]);
    setActive(null);
    setEdge(null);
    setVisited(new Set());
    setTaken(new Set());
    setFailedAt(null);
    setEditing(false);
    setOpen(new Set());
    activeRef.current = null;
    setCounts({ model: 0, tool: 0 });
    setAnswer(null);
    setError("");
    setWaiting(null);
    setTook(null);
    setTiming(null);
  };

  const run = async () => {
    stop.current?.abort();
    reset();
    const ctl = new AbortController();
    stop.current = ctl;
    setRunning(true);
    const t0 = performance.now();
    const ctx = makeCtx({
      live,
      speed,
      params,
      signal: ctl.signal,
      hooks: {
        line: (l) => setLines((ls) => [...ls, { ...l, live }]),
        active: (n, e) => {
          activeRef.current = n;
          setActive(n);
          setEdge(e);
          if (e) setTaken((t) => new Set(t).add(edgeKey(e[0], e[1])));
        },
        visited: (n) => setVisited((v) => (v.has(n) ? v : new Set(v).add(n))),
        counts: setCounts,
        timing: setTiming,
        busy: setBusy,
        waitHuman: (draft, answer) => {
          setEdit(draft);
          setWaiting({ draft, answer });
        },
      },
    });
    try {
      const task = input.trim() || def.task;
      if (!def.engine) throw new Error("this pattern has no engine run");
      const out = await runOnEngine({ ...def, engine: def.engine }, ctx, task, { conn: live ? conn : null, speed, signal: ctl.signal });
      if (!ctl.signal.aborted) {
        const ms = performance.now() - t0;
        setAnswer(out);
        setTook(ms);
        note("run", "final", `finished in ${(ms / 1000).toFixed(1)} s`);
      }
    } catch (e) {
      if ((e as Error).name !== "AbortError") {
        setError((e as Error).message);
        setFailedAt(activeRef.current);
        note("run", "error", `failed: ${(e as Error).message}`);
      } else {
        note("run", "note", "stopped");
      }
    } finally {
      if (stop.current === ctl) {
        setRunning(false);
        setBusy(false);
        setWaiting(null);
        setActive(null);
        setEdge(null);
      }
    }
  };

  const decide = (d: Decision) => {
    note("you", "human", d.kind === "approve" ? "approved" : d.kind === "edit" ? "approved with your edits" : "rejected");
    waiting?.answer(d);
    setWaiting(null);
    setEditing(false);
  };

  const toggle = (id: number) =>
    setOpen((o) => {
      const next = new Set(o);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  // The graph's colours: visited nodes and taken edges are done; the node
  // at work runs (or waits for you); a failure marks where it stopped.
  const nodeStates: Record<string, RunState> = {};
  for (const n of visited) nodeStates[n] = "done";
  if (active) nodeStates[active] = waiting ? "wait" : "run";
  if (failedAt) nodeStates[failedAt] = "fail";
  const edgeStates: Record<string, RunState> = {};
  for (const k of taken) edgeStates[k] = "done";
  if (edge && active) edgeStates[edgeKey(edge[0], edge[1])] = waiting ? "wait" : "run";

  const status = error
    ? { tone: "fail", text: "Failed" }
    : waiting
      ? { tone: "wait", text: "Waiting for you" }
      : running
        ? { tone: "run", text: busy ? "The model is thinking" : "Running" }
        : took != null
          ? { tone: "done", text: `Done in ${(took / 1000).toFixed(1)} s` }
          : { tone: "idle", text: "Ready" };
  const edited = editing && edit.trim() !== (waiting?.draft ?? "").trim();

  return (
    <article className="sb-story">
      <header className="sb-head">
        <p className="lab-eyebrow">{def.group}</p>
        <h1>{def.title}</h1>
        <p className="sb-short">{def.short}</p>
      </header>

      <section className="sb-card sb-about" aria-label="What it is">
        <div>
          {def.what.map((p, i) => (
            <p key={i}>{p}</p>
          ))}
        </div>
        <dl className="sb-when">
          <dt>Use it when</dt>
          {def.use.map((u) => (
            <dd key={u}>{u}</dd>
          ))}
          <dt>Skip it when</dt>
          {def.avoid.map((u) => (
            <dd key={u} className="sb-avoid">
              {u}
            </dd>
          ))}
        </dl>
      </section>

      <section className="sb-card" aria-labelledby="sb-run">
        <div className="sb-run-head">
          <h2 id="sb-run">Run it</h2>
          <span className={`sb-mode ${live ? "sb-mode-live" : ""}`}>
            {!def.usesModel ? "No model needed" : live ? "Real model · simulated tools" : "Scripted model · no key needed"}
            {def.engine ? " · Criogen engine in your browser" : ""}
          </span>
        </div>
        <figure className="scene pg-stage">
          <div className="scene-stage">
            <div className="pg-stagebar">
              <span className={`pg-status pg-status-${status.tone}`} aria-live="polite">
                {status.text}
              </span>
              <div className="pg-stagebar-actions">
                {!live && (
                  <div className="sb-speed" role="group" aria-label="Speed">
                    {[1, 2, 4].map((sp) => (
                      <button key={sp} type="button" className="pg-chip" aria-pressed={speed === sp} onClick={() => setSpeed(sp)} disabled={running}>
                        {sp}×
                      </button>
                    ))}
                  </div>
                )}
                {running ? (
                  <button type="button" className="pg-chip" onClick={() => stop.current?.abort()}>
                    Stop
                  </button>
                ) : (
                  <button type="button" className="pg-run-button" onClick={run}>
                    {lines.length ? "Run again" : "Run"}
                  </button>
                )}
              </div>
            </div>
            <div className="scene-scroll">
              <Diagram graph={def.graph} nodes={nodeStates} edges={edgeStates} title={def.title} />
            </div>
            {waiting && (
              <div className="scene-ask pg-ask" role="group" aria-label="Your decision">
                <p className="scene-ask-prompt">Send this?</p>
                {editing ? (
                  <textarea rows={4} value={edit} onChange={(e) => setEdit(e.target.value)} aria-label="Draft (edit before approving)" />
                ) : (
                  <p className="pg-ask-draft">{waiting.draft}</p>
                )}
                <div className="scene-ask-buttons">
                  <button type="button" data-choice="approve" onClick={() => decide(edited ? { kind: "edit", text: edit } : { kind: "approve" })}>
                    {edited ? "Approve with my edits" : "Approve"}
                  </button>
                  {!editing && (
                    <button type="button" data-choice="edit" onClick={() => setEditing(true)}>
                      Edit
                    </button>
                  )}
                  <button type="button" data-choice="reject" onClick={() => decide({ kind: "reject" })}>
                    Reject
                  </button>
                </div>
              </div>
            )}
          </div>
          <div className="scene-log pg-log">
            <div className="scene-log-head">
              <span>The run</span>
              <span className="sb-counts" aria-live="polite">
                {def.usesModel && (
                  <>
                    model <b>{counts.model}</b>
                    {live ? `/${MAX_MODEL_CALLS}` : ""} · tools <b>{counts.tool}</b>
                  </>
                )}
                {took != null && (
                  <>
                    {def.usesModel ? " · " : ""}
                    {(took / 1000).toFixed(1)} s
                  </>
                )}
              </span>
            </div>
            <ol ref={logRef} aria-label="What the run did">
              {!lines.length && <li data-tone="note">Press Run to watch the pattern work, step by step.</li>}
              {lines.map((l) => {
                const expanded = open.has(l.id);
                return (
                  <li key={l.id} data-tone={TONE[l.kind]} className={expanded ? "pg-open" : undefined}>
                    <button type="button" className="pg-logline" aria-expanded={expanded} onClick={() => toggle(l.id)}>
                      <b>{l.who}</b> {l.text}
                    </button>
                    {l.ms != null && <small className="sb-ms">{seconds(l.ms)}</small>}
                  </li>
                );
              })}
              {busy && (
                <li data-tone="run" className="pg-thinking">
                  thinking…
                </li>
              )}
            </ol>
          </div>
        </figure>

        <div className="sb-controls pg-controls">
          <label className="sb-input">
            <span>{def.usesModel ? "Task" : "Goal"}</span>
            <textarea rows={2} maxLength={600} value={input} onChange={(e) => setInput(e.target.value)} disabled={running || !def.usesModel} />
          </label>
          {def.params?.map((p) => (
            <label key={p.id} className="sb-param">
              <span>
                {p.label}: <b>{params[p.id]}</b>
              </span>
              <input
                type="range"
                min={p.min}
                max={p.max}
                step={p.step ?? 1}
                value={params[p.id]}
                disabled={running}
                onChange={(e) => setParams((ps) => ({ ...ps, [p.id]: Number(e.target.value) }))}
              />
            </label>
          ))}
        </div>
        {def.usesModel && <KeyPanel />}

        {error && (
          <p className="sb-error" role="alert">
            {error}
          </p>
        )}
        {answer != null && (
          <div className="sb-answer" role="status">
            <span>Result</span>
            <p>{answer}</p>
          </div>
        )}
        {timing && <TimeSplit t={timing} />}
        {lines.length > 0 && (
          <details className="pg-transcript">
            <summary>Full transcript ({lines.length} lines)</summary>
            <ol className="sb-log" aria-label="Transcript">
              {lines.map((l) => (
                <li key={l.id} className={`sb-line sb-l-${l.kind}`}>
                  <span className="sb-who">
                    <i>{KIND_LABEL[l.kind]}</i> {l.who}
                  </span>
                  <span className="sb-text">
                    {l.text}
                    {l.ms != null && <small className="sb-ms">{seconds(l.ms)}</small>}
                  </span>
                </li>
              ))}
            </ol>
          </details>
        )}
        {def.engine ? (
          <p className="sb-small sb-honest">
            This run is the TypeScript code below, running on the Criogen engine in your browser (WebAssembly). Without a key, a scripted
            model stands in for <code>MODEL</code>; with your key, your provider answers, called from this tab. The tools return made-up data.
          </p>
        ) : (
          <p className="sb-small sb-honest">
            This run is a small JavaScript copy of the pattern&apos;s control flow, so it can work in your browser. It is not the Criogen engine.
            The code below is how you build the same pattern with Criogen.
          </p>
        )}
      </section>

      <section className="sb-card" aria-labelledby="sb-code">
        <div className="sb-run-head">
          <h2 id="sb-code">Build it with Criogen</h2>
          <span className={`sb-mode ${def.criogen.kind === "helper" ? "sb-mode-live" : ""}`}>
            {def.criogen.kind === "helper" ? "Ready-made" : "Built from primitives"}: <code>{def.criogen.api}</code>
          </span>
        </div>
        {code}
      </section>
    </article>
  );
}

const seconds = (ms: number) => (ms >= 1000 ? `${(ms / 1000).toFixed(2)} s` : `${Math.max(0, Math.round(ms))} ms`);

/** Where the run's time went, by wall clock (parallel calls are not counted twice). */
function TimeSplit({ t }: { t: Timing }) {
  const parts = [
    { key: "model", label: t.live ? "Model (your provider, over the network)" : "Scripted model (a pause, to read along)", ms: t.model },
    { key: "tool", label: "Tools (the sample's functions)", ms: t.tool },
    { key: "engine", label: "Criogen engine and this page (everything else)", ms: t.engine },
    { key: "wait", label: "Playground pauses (to tell the story, or waiting for you)", ms: t.wait },
  ].filter((p) => p.ms > 0.5);
  const total = Math.max(t.total, 1);
  return (
    <section className="sb-time" aria-label="Where the time went">
      <h3>
        Where the time went <span>{seconds(t.total)} in all</span>
      </h3>
      <div className="sb-time-bar" role="img" aria-label={parts.map((p) => `${p.label}: ${seconds(p.ms)}`).join("; ")}>
        {parts.map((p) => (
          <span key={p.key} className={`sb-tb-${p.key}`} style={{ width: `${Math.max(0.6, (100 * p.ms) / total)}%` }} />
        ))}
      </div>
      <ul className="sb-time-legend">
        {parts.map((p) => (
          <li key={p.key}>
            <i className={`sb-tb-${p.key}`} />
            {p.label}: <b>{seconds(p.ms)}</b> ({((100 * p.ms) / total).toFixed(1)}%)
          </li>
        ))}
      </ul>
      <p className="sb-small">
        {t.modelCalls} model call{t.modelCalls === 1 ? "" : "s"}, {t.toolCalls} tool call{t.toolCalls === 1 ? "" : "s"}. Criogen&apos;s share is
        an upper bound: it also holds this page drawing the story. On a server, the engine&apos;s own work is a fraction of a millisecond per
        step (<a href="/benchmarks/">benchmarks</a>).
      </p>
    </section>
  );
}
