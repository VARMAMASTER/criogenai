"use client";
// Memory and CPU for one scenario as bars, and for the concurrent_N
// scenarios, wall time and memory against N.
import type { CSSProperties } from "react";
import { colorOf, fmtMs, fmtNum, labelOf, type BenchFile } from "@/lib/bench";
import { FwName, ticks, useWidth, type DataSet, type Lane } from "./shared";

function Bars({
  title,
  lanes,
  value,
  under,
  format,
  sets,
  files,
  note,
}: {
  title: string;
  lanes: Lane[];
  value: (l: Lane) => number | null | undefined;
  under?: (l: Lane) => number | null | undefined;
  format: (v: number) => string;
  sets: DataSet[];
  files: BenchFile[];
  note?: string;
}) {
  const rows = lanes.filter((l) => l.r.status === "ok" && value(l) != null);
  if (!rows.length) return null;
  const max = Math.max(...rows.map((l) => value(l)!), ...rows.map((l) => under?.(l) ?? 0));
  return (
    <div className="bars">
      <h4>{title}</h4>
      {note && <p className="lab-muted bars-note">{note}</p>}
      <ul>
        {rows.map((l) => {
          const v = value(l)!;
          const u = under?.(l);
          return (
            <li key={`${l.fw}-${l.set.key}`} className={l.set.key === "yours" ? "yours" : undefined} style={{ "--c": colorOf(l.fw) } as CSSProperties}>
              <FwName fw={l.fw} set={l.set} sets={sets} files={files} />
              <div className="bar-track">
                <span className="bar" style={{ width: `${max > 0 ? (v / max) * 100 : 0}%` }}>
                  {u != null && u > 0 && <span className="bar-under" style={{ width: `${Math.min(100, (u / v) * 100)}%` }} />}
                </span>
              </div>
              <span className="bar-value">
                {format(v)}
                {u != null && <span className="lab-muted"> · after set-up {format(u)}</span>}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export default function Resources({ lanes, kind, sets, files }: { lanes: Lane[]; kind: string; sets: DataSet[]; files: BenchFile[] }) {
  const mb = (v: number) => fmtNum(v, "MB");
  const parts = [
    <Bars
      key="mem"
      title="Peak memory"
      note="The process's peak resident set; the darker part is the resident set after imports and set-up."
      lanes={lanes}
      value={(l) => l.r.peak_rss_mb}
      under={(l) => l.r.baseline_rss_mb}
      format={mb}
      sets={sets}
      files={files}
    />,
    <Bars
      key="cpu"
      title={kind === "load" ? "CPU time for the batch" : "CPU time per run"}
      note="User + system time of the measured part."
      lanes={lanes}
      value={(l) => l.r.cpu_ms}
      format={fmtMs}
      sets={sets}
      files={files}
    />,
    kind === "load" ? (
      <Bars
        key="per-run"
        title="Memory per run"
        note="Growth of the resident set divided by N."
        lanes={lanes}
        value={(l) => l.r.rss_per_run_kb}
        format={(v) => fmtNum(v, "KB")}
        sets={sets}
        files={files}
      />
    ) : null,
  ];
  const any = lanes.some((l) => l.r.status === "ok" && (l.r.peak_rss_mb != null || l.r.cpu_ms != null || l.r.rss_per_run_kb != null));
  if (!any) return <p className="lab-muted">No memory or CPU figures in the file for this scenario.</p>;
  return <div className="bars-grid">{parts}</div>;
}

// ---- concurrent_N: a small chart per measure, N on a log axis --------------

interface Point {
  n: number;
  v: number;
}

function LineChart({
  title,
  series,
  format,
}: {
  title: string;
  series: { key: string; fw: string; label: string; yours: boolean; points: Point[] }[];
  format: (v: number) => string;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const all = series.flatMap((s) => s.points);
  if (!all.length) return null;
  const ns = [...new Set(all.map((p) => p.n))].sort((a, b) => a - b);
  const w = Math.max(width, 240);
  const h = 210;
  const m = { l: 62, r: 40, t: 12, b: 34 };
  const nLo = ns[0];
  const nHi = ns[ns.length - 1];
  const x = (n: number) => (nHi === nLo ? m.l + (w - m.l - m.r) / 2 : m.l + ((Math.log10(n) - Math.log10(nLo)) / (Math.log10(nHi) - Math.log10(nLo))) * (w - m.l - m.r));
  const yMax = Math.max(...all.map((p) => p.v)) * 1.08 || 1;
  const y = (v: number) => m.t + (1 - v / yMax) * (h - m.t - m.b);
  const yt = ticks(0, yMax, false, 4);
  return (
    <figure className="line-chart">
      <figcaption>{title}</figcaption>
      <div ref={ref} className="line-chart-box">
        {width > 0 && (
          <svg width={w} height={h} role="img" aria-label={`${title} against the number of runs at once`}>
            {yt.map((t) => (
              <g key={t}>
                <line x1={m.l} x2={w - m.r} y1={y(t)} y2={y(t)} stroke="var(--glass-border)" />
                <text x={m.l - 8} y={y(t)} textAnchor="end" dominantBaseline="central" className="axis-text">
                  {format(t)}
                </text>
              </g>
            ))}
            {ns.map((n, i) => (
              <text key={n} x={x(n)} y={h - 12} textAnchor={ns.length > 1 && i === ns.length - 1 ? "end" : ns.length > 1 && i === 0 ? "start" : "middle"} className="axis-text">
                N = {n.toLocaleString("en-US")}
              </text>
            ))}
            {series.map((s) => {
              const pts = [...s.points].sort((a, b) => a.n - b.n);
              return (
                <g key={s.key} style={{ color: colorOf(s.fw) }}>
                  <polyline
                    points={pts.map((p) => `${x(p.n)},${y(p.v)}`).join(" ")}
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={2}
                    strokeDasharray={s.yours ? "6 4" : undefined}
                  />
                  {pts.map((p) => (
                    <circle key={p.n} cx={x(p.n)} cy={y(p.v)} r={4.5} fill="currentColor" stroke="var(--lab-surface)" strokeWidth={2}>
                      <title>{`${s.label}, N = ${p.n}: ${format(p.v)}`}</title>
                    </circle>
                  ))}
                </g>
              );
            })}
          </svg>
        )}
      </div>
    </figure>
  );
}

export function ConcurrencyCharts({ sets, fws, files }: { sets: DataSet[]; fws: string[]; files: BenchFile[] }) {
  const build = (pick: (r: DataSet["data"]["results"][number]) => number | null | undefined) =>
    fws.flatMap((fw) =>
      sets.map((set) => {
        const points: Point[] = [];
        for (const r of set.data.results) {
          const match = /^concurrent_(\d+)$/.exec(r.scenario);
          const v = pick(r);
          if (match && r.framework === fw && r.status === "ok" && v != null) points.push({ n: Number(match[1]), v });
        }
        const label = `${labelOf(files, fw)}${sets.length > 1 ? ` (${set.tag})` : ""}`;
        return { key: `${fw}-${set.key}`, fw, label, yours: set.key === "yours", points };
      }),
    ).filter((s) => s.points.length);
  const wall = build((r) => r.wall_ms);
  const mem = build((r) => r.peak_rss_mb);
  if (!wall.length && !mem.length) return null;
  const legend = wall.length ? wall : mem;
  return (
    <div className="conc">
      <h4>As the number of runs at once grows</h4>
      <ul className="legend">
        {legend.map((s) => (
          <li key={s.key}>
            <span className={s.yours ? "legend-line yours" : "legend-line"} style={{ color: colorOf(s.fw) }} aria-hidden="true" />
            {s.label}
          </li>
        ))}
      </ul>
      <p className="lab-muted">A framework that failed or could not run a batch has no point there.</p>
      <div className="conc-charts">
        <LineChart title="Wall time of the batch" series={wall} format={fmtMs} />
        <LineChart title="Peak memory" series={mem} format={(v) => fmtNum(v, "MB")} />
      </div>
    </div>
  );
}
