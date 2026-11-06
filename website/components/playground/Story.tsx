"use client";
// One pattern's story: the diagram lights up as the run moves, the
// transcript fills in, and the answer lands at the bottom. Scripted by
// default; with a key in KeyPanel, a real model makes the decisions.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { makeCtx, MAX_MODEL_CALLS, type Decision, type Line, type Timing } from "@/lib/patterns/engine";
import { runOnEngine } from "@/lib/patterns/live";
import { byId } from "@/lib/patterns";
import Diagram from "./Diagram";
import KeyPanel from "./KeyPanel";
import { useKey } from "./KeyProvider";

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
          setActive(n);
          setEdge(e);
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
        setAnswer(out);
        setTook(performance.now() - t0);
      }
    } catch (e) {
      if ((e as Error).name !== "AbortError") setError((e as Error).message);
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
    waiting?.answer(d);
    setWaiting(null);
  };

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
        <Diagram graph={def.graph} active={active} edge={edge} visited={visited} title={def.title} />
        <div className="sb-controls">
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
          <div className="sb-buttons">
            <button type="button" className="lab-button sb-primary" onClick={run} disabled={running}>
              {lines.length ? "Run again" : "Run"}
            </button>
            <button type="button" className="lab-button" onClick={() => stop.current?.abort()} disabled={!running}>
              Stop
            </button>
            {!live && (
              <div className="sb-speed" role="group" aria-label="Speed">
                {[1, 2, 4].map((s) => (
                  <button key={s} type="button" className="lab-button" aria-pressed={speed === s} onClick={() => setSpeed(s)} disabled={running}>
                    {s}×
                  </button>
                ))}
              </div>
            )}
            <span className="sb-counts" aria-live="polite">
              {def.usesModel && (
                <>
                  model calls <b>{counts.model}</b>
                  {live ? ` / ${MAX_MODEL_CALLS} max` : ""} ·{" "}
                </>
              )}
              {def.usesModel && (
                <>
                  tool calls <b>{counts.tool}</b>
                </>
              )}
              {took != null && (
                <>
                  {def.usesModel ? " · " : ""}finished in {(took / 1000).toFixed(1)} s
                </>
              )}
              {busy && <span className="sb-thinking"> · thinking…</span>}
            </span>
          </div>
        </div>
        {def.usesModel && <KeyPanel />}

        <ol className="sb-log" ref={logRef} aria-label="Transcript" aria-live="polite">
          {!lines.length && <li className="sb-empty">Press Run to watch the pattern work step by step.</li>}
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
          {waiting && (
            <li className="sb-line sb-l-human sb-approval">
              <span className="sb-who">
                <i>you</i> reviewer
              </span>
              <div className="sb-text">
                <textarea rows={4} value={edit} onChange={(e) => setEdit(e.target.value)} aria-label="Draft (edit before approving)" />
                <div className="sb-buttons">
                  <button type="button" className="lab-button sb-primary" onClick={() => decide(edit.trim() === waiting.draft.trim() ? { kind: "approve" } : { kind: "edit", text: edit })}>
                    {edit.trim() === waiting.draft.trim() ? "Approve" : "Approve with my edits"}
                  </button>
                  <button type="button" className="lab-button sb-danger" onClick={() => decide({ kind: "reject" })}>
                    Reject
                  </button>
                </div>
              </div>
            </li>
          )}
        </ol>
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
