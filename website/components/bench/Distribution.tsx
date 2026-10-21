"use client";
// The distribution of per-run times: every sample as a dot, the middle half
// as a box, the median as a line and the 95th percentile as a tick, on one
// shared axis (logarithmic when the frameworks are far apart).
import { colorOf, fmtMs, quantile, type BenchFile } from "@/lib/bench";
import { FwName, ticks, useWidth, type DataSet, type Lane } from "./shared";

const ROW = 46;
const PAD = 28;

export default function Distribution({ lanes, sets, files, kind }: { lanes: Lane[]; sets: DataSet[]; files: BenchFile[]; kind: string }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const rows = lanes.filter((l) => l.r.status === "ok" && ((l.r.samples_ms?.length ?? 0) > 0 || l.r.median_ms != null));
  if (!rows.length) return <p className="lab-muted">No per-run times in the file for this scenario.</p>;

  const all: number[] = [];
  for (const l of rows) {
    all.push(...(l.r.samples_ms ?? []));
    for (const v of [l.r.min_ms, l.r.max_ms, l.r.median_ms, l.r.p95_ms]) if (v != null) all.push(v);
  }
  const positive = all.filter((v) => v > 0);
  let lo = Math.min(...all);
  let hi = Math.max(...all);
  const log = positive.length === all.length && lo > 0 && hi / lo > 20;
  if (!log) {
    lo = Math.min(0, lo);
    hi = hi === lo ? lo + 1 : hi;
  }
  const w = Math.max(width, 200);
  const x = (v: number) =>
    log
      ? PAD + ((Math.log10(v) - Math.log10(lo)) / (Math.log10(hi) - Math.log10(lo) || 1)) * (w - 2 * PAD)
      : PAD + ((v - lo) / (hi - lo)) * (w - 2 * PAD);
  const axis = ticks(lo, hi, log);

  return (
    <div className="dist">
      <p className="lab-muted dist-legend">
        Dots: single runs ({kind === "load" ? "latencies inside the batch" : "per run"}). Box: middle half. Thick line: median. Thin tick: 95th
        percentile.{log ? " Logarithmic axis: the frameworks are more than 20× apart." : ""}
      </p>
      <div className="dist-rows">
        {rows.map((lane) => {
          const s = [...(lane.r.samples_ms ?? [])].sort((a, b) => a - b);
          const q1 = s.length ? quantile(s, 0.25) : null;
          const q3 = s.length ? quantile(s, 0.75) : null;
          const med = lane.r.median_ms ?? (s.length ? quantile(s, 0.5) : null);
          const p95 = lane.r.p95_ms ?? null;
          const c = colorOf(lane.fw);
          const yours = lane.set.key === "yours";
          return (
            <div className="dist-row" key={`${lane.fw}-${lane.set.key}`}>
              <div className="dist-label">
                <FwName fw={lane.fw} set={lane.set} sets={sets} files={files} />
              </div>
              <div className="dist-plot" ref={rows[0] === lane ? ref : undefined}>
                {width > 0 && (
                  <svg width={w} height={ROW} role="img" aria-label={`${lane.fw}: median ${fmtMs(med)}, 95th percentile ${fmtMs(p95)}, ${s.length} samples`}>
                    {q1 !== null && q3 !== null && (
                      <rect x={x(q1)} y={ROW / 2 - 10} width={Math.max(x(q3) - x(q1), 2)} height={20} rx={4} fill={c} opacity={0.18} stroke={c} strokeOpacity={0.6} strokeDasharray={yours ? "4 3" : undefined} />
                    )}
                    {s.map((v, i) => (
                      <circle key={i} cx={x(v)} cy={ROW / 2 + (((i * 7919) % 17) - 8)} r={2.6} fill={c} opacity={0.55}>
                        <title>{fmtMs(v)}</title>
                      </circle>
                    ))}
                    {med !== null && <line x1={x(med)} x2={x(med)} y1={ROW / 2 - 15} y2={ROW / 2 + 15} stroke="var(--ab-text)" strokeWidth={3} strokeLinecap="round" />}
                    {p95 !== null && <line x1={x(p95)} x2={x(p95)} y1={ROW / 2 - 9} y2={ROW / 2 + 9} stroke="var(--ab-muted)" strokeWidth={2} strokeLinecap="round" />}
                  </svg>
                )}
              </div>
            </div>
          );
        })}
        <div className="dist-row dist-axis-row" aria-hidden="true">
          <div className="dist-label" />
          <div className="dist-plot">
            {width > 0 && (
              <svg width={w} height={22}>
                <line x1={PAD} x2={w - PAD} y1={2} y2={2} stroke="var(--glass-border-strong)" />
                {axis.map((t) => (
                  <g key={t} transform={`translate(${x(t)} 0)`}>
                    <line y1={2} y2={6} stroke="var(--ab-faint)" />
                    <text y={18} textAnchor="middle" className="axis-text">
                      {fmtMs(t)}
                    </text>
                  </g>
                ))}
              </svg>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
