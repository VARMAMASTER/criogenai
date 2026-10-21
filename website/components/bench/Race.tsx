"use client";
// The race: one lane per framework, each reaching the finish line in time
// proportional to its measured number, scaled so the slowest takes a few
// seconds. The real numbers and the scale are printed beside it.
import { useEffect, useRef, useState, type CSSProperties } from "react";
import { colorOf, fmtMs, type BenchFile, type BenchScenario } from "@/lib/bench";
import { FwName, useReducedMotion, type DataSet, type Lane } from "./shared";

const TARGET_MS = 4000; // the slowest lane, on screen

export function metricOf(kind: string, lane: Lane): number | null {
  const v = kind === "load" ? (lane.r.wall_ms ?? null) : (lane.r.median_ms ?? null);
  return lane.r.status === "ok" && v !== null && Number.isFinite(v) && v > 0 ? v : null;
}

export function metricName(kind: string): string {
  return kind === "load" ? "wall time of the whole batch" : kind === "footprint" ? "median per import" : "median per run";
}

function ordinal(n: number): string {
  return n === 1 ? "1st" : n === 2 ? "2nd" : n === 3 ? "3rd" : `${n}th`;
}

function scaleText(scale: number): string {
  if (scale >= 1.05) return `slowed down ${scale >= 100 ? Math.round(scale).toLocaleString("en-US") : scale.toPrecision(3)}×`;
  if (scale <= 0.95) return `sped up ${(1 / scale).toPrecision(3)}×`;
  return "in real time";
}

export default function Race({ lanes, scenario, sets, files }: { lanes: Lane[]; scenario: BenchScenario; sets: DataSet[]; files: BenchFile[] }) {
  const reduce = useReducedMotion();
  const box = useRef<HTMLDivElement>(null);
  const [run, setRun] = useState(0); // 0: not started yet
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setRun((r) => (r === 0 ? 1 : r));
        io.disconnect();
      }
    }, { threshold: 0, rootMargin: "0px 0px -25% 0px" });
    io.observe(el);
    return () => io.disconnect();
  }, []);
  // A new scenario starts a new race.
  useEffect(() => setRun((r) => (r === 0 ? 0 : r + 1)), [scenario.id]);

  const values = lanes.map((l) => metricOf(scenario.kind, l));
  const measured = values.filter((v): v is number => v !== null);
  if (!lanes.length) return <p className="lab-muted">No results for this scenario in the file.</p>;
  const max = measured.length ? Math.max(...measured) : 0;
  const scale = max > 0 ? TARGET_MS / max : 1;
  const sorted = [...measured].sort((a, b) => a - b);
  const static_ = reduce;

  return (
    <div className="race" ref={box}>
      <ol className="race-lanes">
        {lanes.map((lane, i) => {
          const v = values[i];
          const style = {
            "--c": colorOf(lane.fw),
            "--dur": `${v !== null ? Math.max(v * scale, 60) : 0}ms`,
          } as CSSProperties;
          const state = static_ ? "done" : run === 0 ? "ready" : "go";
          return (
            <li key={`${lane.fw}-${lane.set.key}`} className={`race-lane${lane.set.key === "yours" ? " yours" : ""}`} style={style}>
              <div className="race-label">
                <FwName fw={lane.fw} set={lane.set} sets={sets} files={files} />
              </div>
              {v !== null ? (
                <div className="race-track" data-state={state} key={static_ ? "static" : run}>
                  <span className="race-fill" />
                  <span className="race-result">
                    <b>{fmtMs(v)}</b>
                    <span className="race-rank">{ordinal(sorted.indexOf(v) + 1)}</span>
                  </span>
                </div>
              ) : (
                <div className={`race-missing status-${lane.r.status}`}>
                  <b>{lane.r.status === "unsupported" ? "Not supported" : lane.r.status === "error" ? "Failed" : "No number"}</b>
                  <span>{lane.r.status === "error" ? (lane.r.error ?? "No error message in the file.") : (lane.r.note ?? "No note in the file.")}</span>
                </div>
              )}
            </li>
          );
        })}
      </ol>
      <div className="race-foot">
        <p className="lab-muted">
          Each lane finishes in time proportional to its {metricName(scenario.kind)}.{" "}
          {max > 0 && (
            <>
              Replay {scaleText(scale)}: 1 ms measured takes {fmtMs(scale)} on screen.
            </>
          )}
        </p>
        <button type="button" className="lab-button" onClick={() => setRun((r) => r + 1)} disabled={static_}>
          Replay
        </button>
      </div>
    </div>
  );
}
