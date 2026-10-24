"use client";
// The playground: the same job in AgentBuild and in LangChain, each in its own
// process against one fake model server, measured from outside. With a
// playground server running on this computer (playground/server.py) the page
// runs jobs there and streams the results; without one it replays recordings
// made by that same server (playground/record.py). Either way every number is
// computed from the server's events in lib/playground.ts.
import { useCallback, useEffect, useRef, useState } from "react";
import Chart from "./Chart";
import {
  apply,
  compare,
  fmt,
  fresh,
  FRAMEWORKS,
  replayPlan,
  type Job,
  type PlayEvent,
  type PlayState,
  type Recorded,
} from "@/lib/playground";

/** Where playground/server.py listens (it only accepts this site's pages). */
const SERVER = "http://127.0.0.1:4600";
const REPLAY_SPEED = 6;

interface Info {
  machine: { cores?: number; ram_gb?: number; os?: string };
  frameworks: Record<string, { label: string; ready: boolean }>;
}

const PRESETS = {
  realistic: { label: "With a model's waiting time", model_delay_ms: 200, tool_delay_ms: 100, runs: 30, concurrency: 1 },
  overhead: { label: "Framework cost only", model_delay_ms: 0, tool_delay_ms: 0, runs: 100, concurrency: 10 },
} as const;
type PresetId = keyof typeof PRESETS;

const LIMIT_NAMES: Record<string, string> = {
  tools: "tools",
  model_delay_ms: "model delay",
  tool_delay_ms: "tool delay",
  runs: "benchmark runs",
  concurrency: "at the same time",
  message: "message length",
};

export default function Playground({ recorded }: { recorded: Recorded | null }) {
  const [info, setInfo] = useState<Info | null>(null);
  const [probed, setProbed] = useState(false);
  const [mode, setMode] = useState<"recorded" | "live">("recorded");
  const [canRun, setCanRun] = useState(false);
  const [, setTick] = useState(0);
  const state = useRef<PlayState>(fresh());
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [recId, setRecId] = useState(recorded?.recordings[0]?.id ?? "");
  const [preset, setPreset] = useState<PresetId>("realistic");
  const [message, setMessage] = useState("Where is my refund for order A-1001?");
  const [tools, setTools] = useState(3);
  const [modelDelay, setModelDelay] = useState<number>(PRESETS.realistic.model_delay_ms);
  const [toolDelay, setToolDelay] = useState<number>(PRESETS.realistic.tool_delay_ms);
  const [runs, setRuns] = useState<number>(PRESETS.realistic.runs);
  const [concurrency, setConcurrency] = useState<number>(PRESETS.realistic.concurrency);
  const stop = useRef<(() => void) | null>(null);
  const redraw = useCallback(() => setTick((n) => n + 1), []);

  // Is a playground server running on this computer?
  useEffect(() => {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), 1500);
    fetch(`${SERVER}/api/info`, { signal: ctl.signal })
      .then((r) => (r.ok ? (r.json() as Promise<Info>) : Promise.reject(new Error("no"))))
      .then((i) => {
        setInfo(i);
        if (Object.values(i.frameworks).every((f) => f.ready)) {
          setCanRun(true);
          setMode("live");
        }
      })
      .catch(() => undefined)
      .finally(() => {
        clearTimeout(timer);
        setProbed(true);
      });
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  }, []);

  useEffect(() => () => stop.current?.(), []);

  const choose = (id: PresetId) => {
    setPreset(id);
    const p = PRESETS[id];
    setModelDelay(p.model_delay_ms);
    setToolDelay(p.tool_delay_ms);
    setRuns(p.runs);
    setConcurrency(p.concurrency);
  };

  const finish = (text: string) => {
    stop.current = null;
    setRunning(false);
    setStatus(text);
    redraw();
  };

  /** Show a recording as it happened, six times faster. */
  const replay = (id: string) => {
    const rec = recorded?.recordings.find((r) => r.id === id);
    if (!rec) return;
    stop.current?.();
    state.current = fresh();
    setError("");
    setRunning(true);
    setStatus("Replaying at 6× speed…");
    const plan = replayPlan(rec.events);
    const t0 = performance.now();
    let next = 0;
    const timer = setInterval(() => {
      const upTo = (performance.now() - t0) * REPLAY_SPEED;
      while (next < plan.length && plan[next].at <= upTo) apply(state.current, plan[next++].ev);
      redraw();
      if (next >= plan.length) {
        clearInterval(timer);
        finish("Replay complete");
      }
    }, 100);
    stop.current = () => {
      clearInterval(timer);
      finish("Stopped");
    };
  };

  const skip = () => {
    const rec = recorded?.recordings.find((r) => r.id === recId);
    if (!rec) return;
    stop.current = null;
    state.current = fresh();
    for (const ev of rec.events) apply(state.current, ev);
    setRunning(false);
    setStatus("Showing the recording");
    redraw();
  };

  // Show the first recording straight away, as the page's answer to "what is this".
  useEffect(() => {
    if (recorded && recorded.recordings[0] && !state.current.job) {
      state.current = fresh();
      for (const ev of recorded.recordings[0].events) apply(state.current, ev);
      setStatus("Showing the recording");
      redraw();
    }
  }, [recorded, redraw]);

  const runLive = async (kind: "live" | "bench") => {
    const body = {
      message,
      tools,
      model_delay_ms: modelDelay,
      tool_delay_ms: toolDelay,
      // "Run both at once" is one run, or one burst of `concurrency` runs; a benchmark repeats `runs` times.
      runs: kind === "live" ? (concurrency > 1 ? Math.min(concurrency, 100) : 0) : runs,
      concurrency,
      order: kind === "live" ? "parallel" : "sequential",
    };
    state.current = fresh();
    setError("");
    setRunning(true);
    setStatus("Running…");
    redraw();
    const ctl = new AbortController();
    stop.current = () => ctl.abort();
    try {
      const res = await fetch(`${SERVER}/api/run`, { method: "POST", body: JSON.stringify(body), signal: ctl.signal });
      if (!res.ok || !res.body) throw new Error(((await res.json().catch(() => ({}))) as { error?: string }).error ?? `HTTP ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let last = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          const ev = JSON.parse(line) as PlayEvent;
          apply(state.current, ev);
          if (ev.e === "error") setError(`${FRAMEWORKS.find((f) => f.id === ev.fw)?.label}: ${ev.error}`);
        }
        if (performance.now() - last > 120) {
          redraw();
          last = performance.now();
        }
      }
      finish("Complete");
    } catch (e) {
      if ((e as Error).name === "AbortError") finish("Stopped");
      else {
        setError((e as Error).message);
        finish("");
      }
    }
  };

  const s = state.current;
  const rows = compare(s);
  const chips = rows.filter((r) => r.win && r.row.chip);
  const limited = Object.entries(s.job?.limited ?? {});
  const job: Job | undefined = s.job;
  const live = mode === "live";
  const rec = recorded?.recordings.find((r) => r.id === recId);

  return (
    <div className="pg">
      <section className="lab-card" aria-labelledby="pg-ask">
        <h2 id="pg-ask">{live ? "Run a job" : "Recorded runs"}</h2>
        {probed && !live && (
          <p className="lab-muted">
            No local playground server was found, so these are recordings of real runs
            {recorded ? ` (${recorded.machine.cores} cores, ${recorded.machine.ram_gb} GB RAM, ${recorded.machine.os}, ${recorded.recorded_at.slice(0, 10)})` : ""}.
            To send your own message, start the server (see “Run it locally” below) and reload.
          </p>
        )}
        {live && info && (
          <p className="lab-muted">
            Connected to the local playground server ({info.machine.cores} cores, {info.machine.ram_gb} GB RAM, {info.machine.os}).
          </p>
        )}
        {canRun && (
          <div className="pg-modes" role="group" aria-label="Source">
            <button type="button" className="lab-button" aria-pressed={live} disabled={running} onClick={() => setMode("live")}>
              Live
            </button>
            <button type="button" className="lab-button" aria-pressed={!live} disabled={running} onClick={() => setMode("recorded")}>
              Recorded
            </button>
          </div>
        )}

        {live ? (
          <>
            <label className="pg-field pg-wide">
              <span>User message</span>
              <textarea maxLength={2000} value={message} onChange={(e) => setMessage(e.target.value)} />
            </label>
            <div className="pg-modes" role="group" aria-label="Preset">
              {(Object.keys(PRESETS) as PresetId[]).map((id) => (
                <button key={id} type="button" className="lab-button" aria-pressed={preset === id} onClick={() => choose(id)}>
                  {PRESETS[id].label}
                </button>
              ))}
            </div>
            <details className="pg-advanced">
              <summary>Advanced</summary>
              <div className="pg-options">
                <Num label="Tools the agent uses (0–5)" value={tools} min={0} max={5} set={setTools} />
                <Num label="Model delay (ms)" value={modelDelay} min={0} max={5000} set={setModelDelay} />
                <Num label="Tool delay (ms)" value={toolDelay} min={0} max={5000} set={setToolDelay} />
                <Num label="Benchmark runs" value={runs} min={1} max={200} set={setRuns} />
                <Num label="At the same time (1–100)" value={concurrency} min={1} max={100} set={setConcurrency} />
              </div>
            </details>
            <div className="pg-buttons">
              <button type="button" className="lab-button pg-primary" disabled={running} onClick={() => runLive("bench")}>
                Benchmark (A, B, B, A)
              </button>
              <button type="button" className="lab-button" disabled={running} onClick={() => runLive("live")}>
                Run both at once
              </button>
              <button type="button" className="lab-button" disabled={!running} onClick={() => stop.current?.()}>
                Stop
              </button>
              <span className="lab-muted" role="status" aria-live="polite">
                {status}
              </span>
            </div>
            <p className="lab-muted pg-small">
              <b>Benchmark</b> runs AgentBuild, LangChain, LangChain, AgentBuild and averages each framework&apos;s two passes, so order and background load
              cancel out; use it for numbers you intend to quote. <b>Run both at once</b> starts both together for a side-by-side view; they compete for the
              same CPU, so do not quote it.
            </p>
          </>
        ) : (
          recorded && (
            <>
              <div className="pg-modes" role="group" aria-label="Recording">
                {recorded.recordings.map((r) => (
                  <button key={r.id} type="button" className="lab-button" aria-pressed={recId === r.id} disabled={running} onClick={() => { setRecId(r.id); stop.current?.(); state.current = fresh(); for (const ev of r.events) apply(state.current, ev); setStatus("Showing the recording"); redraw(); }}>
                    {r.label}
                  </button>
                ))}
              </div>
              {rec && <p className="lab-muted">{rec.about}</p>}
              <div className="pg-buttons">
                <button type="button" className="lab-button pg-primary" disabled={running} onClick={() => replay(recId)}>
                  Replay
                </button>
                <button type="button" className="lab-button" disabled={running} onClick={skip}>
                  Show final result
                </button>
                <button type="button" className="lab-button" disabled={!running} onClick={() => stop.current?.()}>
                  Stop
                </button>
                <span className="lab-muted" role="status" aria-live="polite">
                  {status}
                </span>
              </div>
            </>
          )
        )}
        {error && (
          <p className="pg-error" role="alert">
            {error}
          </p>
        )}
        {limited.length > 0 && (
          <p className="lab-muted pg-small" role="status">
            Limited to what this computer allows: {limited.map(([k, [asked, used]]) => `${LIMIT_NAMES[k] ?? k} ${asked} → ${used}`).join(", ")}.
          </p>
        )}
      </section>

      <div className="pg-two">
        {FRAMEWORKS.map((f, i) => {
          const fs = s.fw[f.id];
          return (
            <section key={f.id} className={`lab-card pg-fw-${i + 1}`} aria-label={f.label}>
              <div className="pg-head">
                <h2>
                  <span className="pg-swatch" aria-hidden="true" />
                  {f.label}
                </h2>
                <span className={`pg-badge pg-${fs.status}`}>{fs.status}</span>
              </div>
              <div className="lab-muted pg-small">{fs.version ?? " "}</div>
              <div className={`pg-answer${fs.answer ? "" : " pg-empty"}`}>{fs.answer || "No answer yet."}</div>
              <Timeline tools={fs.tools} first={fs.metrics.first_ms} />
            </section>
          );
        })}
      </div>

      <section className="lab-card" aria-labelledby="pg-use">
        <h2 id="pg-use">CPU and memory</h2>
        <div className="pg-two pg-flat">
          <Chart state={s} kind="cpu" unit="%" title="CPU, % of one core (above 100% = more than one core)" />
          <Chart state={s} kind="rss" unit=" MB" title="Resident memory, MB (whole process)" />
        </div>
        <ul className="pg-legend">
          {FRAMEWORKS.map((f, i) => (
            <li key={f.id} className={`pg-fw-${i + 1}`}>
              <i aria-hidden="true" /> {f.label}
            </li>
          ))}
        </ul>
      </section>

      <section className="lab-card" aria-labelledby="pg-cmp">
        <h2 id="pg-cmp">Results</h2>
        {chips.length > 0 && (
          <ul className="pg-chips">
            {chips.map((c) => (
              <li key={c.row.key}>
                {c.row.label}: <b>{c.note}</b>
              </li>
            ))}
          </ul>
        )}
        <div className="pg-scroll">
          <table className="pg-table">
            <thead>
              <tr>
                <th scope="col">Measurement</th>
                <th scope="col">AgentBuild</th>
                <th scope="col">LangChain</th>
                <th scope="col">Difference</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.row.key}>
                  <th scope="row">{c.row.label}</th>
                  <td className={c.win === "agentbuild" ? "pg-win" : ""}>
                    {fmt(c.agentbuild, c.row.digits)}
                    {c.agentbuild != null ? c.row.unit : ""}
                  </td>
                  <td className={c.win === "langchain" ? "pg-win" : ""}>
                    {fmt(c.langchain, c.row.digits)}
                    {c.langchain != null ? c.row.unit : ""}
                  </td>
                  <td className="pg-note">{c.note}</td>
                </tr>
              ))}
              {rows.length === 0 && (
                <tr>
                  <td colSpan={4} className="lab-muted">
                    No results yet.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
        {job && (
          <p className="lab-muted pg-small">
            Setup: {job.tools} tools at {job.tool_delay_ms} ms each, {job.model_delay_ms} ms per model call
            {job.runs > 0 ? `, ${job.runs} runs, ${job.concurrency} at a time` : ", one run each"}.{" "}
            {job.order === "parallel"
              ? "Both frameworks ran concurrently on one machine and affected each other."
              : job.balanced
                ? "Order A, B, B, A with a fresh process per pass; figures are the mean of each framework's two passes."
                : "Frameworks ran one after the other."}{" "}
            {job.runs <= 0 && "With a single run, memory, CPU and import time mostly reflect loading the framework, not speed under load. "}
            Single machine, single measurement: indicative, not conclusive.
          </p>
        )}
      </section>
    </div>
  );
}

function Num({ label, value, min, max, set }: { label: string; value: number; min: number; max: number; set: (n: number) => void }) {
  return (
    <label className="pg-field">
      <span>{label}</span>
      <input type="number" min={min} max={max} value={value} onChange={(e) => set(Number(e.target.value))} />
    </label>
  );
}

/** The first run: the model waits, then the tools run (together), then the model answers. */
function Timeline({ tools, first }: { tools: { name: string; start_ms: number; end_ms: number }[]; first?: number }) {
  if (first == null && !tools.length) return null;
  const span = Math.max(first ?? 0, ...tools.map((t) => t.end_ms), 1);
  const bars: [string, number, number, string][] = [];
  if (tools.length) {
    const begin = Math.min(...tools.map((t) => t.start_ms));
    const end = Math.max(...tools.map((t) => t.end_ms));
    bars.push(["model", 0, begin, "model"], ...tools.map((t): [string, number, number, string] => [t.name, t.start_ms, t.end_ms, "tool"]));
    if (first != null) bars.push(["model", end, first, "model"]);
  } else if (first != null) bars.push(["model", 0, first, "model"]);
  return (
    <div className="pg-timeline">
      <div className="lab-muted pg-small">First run, step by step: model call, parallel tool calls, model call</div>
      {bars.map(([name, a, b, kind], i) => (
        <div className="pg-row" key={`${name}${i}`}>
          <span className="pg-name">{name}</span>
          <div className="pg-lane">
            <div className={`pg-bar pg-${kind}`} style={{ left: `${(a / span) * 100}%`, width: `${Math.max(0.5, ((b - a) / span) * 100)}%` }} />
          </div>
        </div>
      ))}
      <div className="pg-axis">
        <span>0</span>
        <span>{fmt(span)} ms</span>
      </div>
    </div>
  );
}
