// A line chart of one measurement over time, one line per framework, each
// starting at its own zero (a framework that ran after the other overlays it).
import { fmt, FRAMEWORKS, type PlayState } from "@/lib/playground";

const W = 520;
const H = 200;
const PAD = { l: 46, r: 16, t: 12, b: 24 };

function niceMax(v: number): number {
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 5, 10]) if (v <= m * p) return m * p;
  return v;
}

export default function Chart({ state, kind, unit, title }: { state: PlayState; kind: "cpu" | "rss"; unit: string; title: string }) {
  const series = FRAMEWORKS.map((f, i) => {
    const raw = state.fw[f.id].samples;
    const zero = raw.length ? raw[0].t : 0;
    return { ...f, cls: `pg-fw-${i + 1}`, pts: raw.map((p) => ({ t: p.t - zero, v: p[kind] })) };
  });
  const tmax = Math.max(1000, ...series.flatMap((s) => s.pts.map((p) => p.t)));
  const vmax = niceMax(Math.max(1, ...series.flatMap((s) => s.pts.map((p) => p.v))));
  const X = (t: number) => PAD.l + (t / tmax) * (W - PAD.l - PAD.r);
  const Y = (v: number) => H - PAD.b - (v / vmax) * (H - PAD.t - PAD.b);
  const digits = vmax < 10 ? 1 : 0;
  const ticks = [0, 1, 2, 3, 4];
  const last = (s: (typeof series)[number]) => s.pts[s.pts.length - 1];
  const summary = series
    .filter((s) => s.pts.length)
    .map((s) => `${s.label} ends at ${fmt(last(s).v, digits)}${unit}`)
    .join(", ");
  return (
    <figure className="pg-chart">
      <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`${title}. ${summary || "No data yet."}`}>
        {ticks.map((i) => {
          const v = (vmax / 4) * i;
          return (
            <g key={`y${i}`}>
              <line className="pg-grid" x1={PAD.l} x2={W - PAD.r} y1={Y(v)} y2={Y(v)} />
              <text className="pg-tick" x={PAD.l - 6} y={Y(v) + 4} textAnchor="end">
                {fmt(v, digits)}
              </text>
            </g>
          );
        })}
        {ticks.map((i) => (
          <text key={`x${i}`} className="pg-tick" x={X((tmax / 4) * i)} y={H - 6} textAnchor="middle">
            {fmt((tmax / 4000) * i, 1)} s
          </text>
        ))}
        {series.map(
          (s) =>
            s.pts.length > 0 && (
              <g key={s.id} className={s.cls}>
                <polyline className="pg-line" points={s.pts.map((p) => `${X(p.t).toFixed(1)},${Y(p.v).toFixed(1)}`).join(" ")} />
                <circle className="pg-dot" cx={X(last(s).t)} cy={Y(last(s).v)} r="3.5" />
                <text className="pg-end" x={Math.min(X(last(s).t) + 6, W - 44)} y={Y(last(s).v) - 7}>
                  {fmt(last(s).v, digits)}
                  {unit}
                </text>
              </g>
            ),
        )}
      </svg>
      <figcaption>{title}</figcaption>
    </figure>
  );
}
