"use client";
// "Head to head": every measurement of a results file as comparison graphs,
// the frameworks side by side (fixed colours by identity, values written on
// the marks, details on hover or focus, a table under each group).
import { useState, type ReactNode } from "react";
import { colorOf, fmtMs, fmtNum, labelOf, type BenchFile, type Outcome } from "@/lib/bench";
import { ticks, useWidth } from "./shared";

interface Row {
  fw: string;
  value: number | null;
  /** Shown on hover / focus. */
  detail?: string[];
  /** Shown instead of a bar when there is no value. */
  missing?: string;
}

const LOWER = "lower is better";

function times(value: number, best: number): string {
  if (!(best > 0) || value === best) return "best";
  const r = value / best;
  return `${r >= 10 ? Math.round(r) : r.toFixed(1)}× ${r >= 1 ? "more" : "less"}`;
}

/** Horizontal bars, one per framework, on one linear scale from zero. */
function CompareBars({
  title,
  note = LOWER,
  rows,
  format,
  files,
}: {
  title: string;
  note?: string;
  rows: Row[];
  format: (v: number) => string;
  files: BenchFile[];
}) {
  const [hover, setHover] = useState<string | null>(null);
  const values = rows.map((r) => r.value).filter((v): v is number => v !== null && Number.isFinite(v));
  const max = Math.max(...values, 0);
  const best = values.length ? Math.min(...values) : 0;
  return (
    <figure className="h2h-chart">
      <figcaption>
        <span className="h2h-title">{title}</span>
        <span className="h2h-note">{note}</span>
      </figcaption>
      <div className="h2h-bars" role="list">
        {rows.map((r) => {
          const ok = r.value !== null && Number.isFinite(r.value);
          const share = ok && max > 0 ? Math.max((r.value as number) / max, 0.006) : 0;
          const open = hover === r.fw;
          return (
            <div
              key={r.fw}
              role="listitem"
              className={`h2h-row${open ? " open" : ""}`}
              tabIndex={0}
              onMouseEnter={() => setHover(r.fw)}
              onMouseLeave={() => setHover(null)}
              onFocus={() => setHover(r.fw)}
              onBlur={() => setHover(null)}
              aria-label={`${labelOf(files, r.fw)}: ${ok ? format(r.value as number) : r.missing ?? "no value"}`}
            >
              <span className="h2h-name">
                <span className="swatch" style={{ ["--c" as string]: colorOf(r.fw) }} aria-hidden="true" />
                {labelOf(files, r.fw)}
              </span>
              <span className="h2h-track">
                {ok ? (
                  <>
                    <span className="h2h-bar" style={{ width: `calc((100% - 7.5rem) * ${share})`, ["--c" as string]: colorOf(r.fw) }} />
                    <span className="h2h-value">
                      <b>{format(r.value as number)}</b>
                      {values.length > 1 && <span className="h2h-times">{times(r.value as number, best)}</span>}
                    </span>
                  </>
                ) : (
                  <span className="h2h-missing">{r.missing ?? "no value"}</span>
                )}
              </span>
              {open && r.detail && r.detail.length > 0 && (
                <span className="h2h-tip" role="tooltip">
                  {r.detail.map((d) => (
                    <span key={d}>{d}</span>
                  ))}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </figure>
  );
}

interface Series {
  fw: string;
  points: { x: string; value: number | null; detail?: string }[];
}

/** Lines across a few x positions (e.g. 1, 100, 1000 runs at once);
    a log y scale when the values span more than 20×. */
function CompareLines({ title, note = LOWER, series, format, files }: { title: string; note?: string; series: Series[]; format: (v: number) => string; files: BenchFile[] }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [at, setAt] = useState<number | null>(null);
  const xs = series[0]?.points.map((p) => p.x) ?? [];
  const values = series.flatMap((s) => s.points.map((p) => p.value)).filter((v): v is number => v !== null && v > 0);
  const lo = Math.min(...values);
  const hi = Math.max(...values);
  const log = values.length > 0 && hi / lo > 20;
  const left = 64;
  const right = 16;
  const top = 12;
  const height = 220;
  const bottom = 30;
  const w = Math.max(width, 280);
  const plotW = w - left - right;
  const plotH = height - top - bottom;
  const yLo = log ? 10 ** Math.floor(Math.log10(lo)) : 0;
  const yHi = log ? 10 ** Math.ceil(Math.log10(hi)) : hi * 1.1;
  const y = (v: number) => top + plotH - (log ? (Math.log10(v) - Math.log10(yLo)) / (Math.log10(yHi) - Math.log10(yLo)) : (v - yLo) / (yHi - yLo)) * plotH;
  const x = (i: number) => left + (xs.length === 1 ? plotW / 2 : (i / (xs.length - 1)) * plotW);
  const yTicks = ticks(yLo, yHi, log, 4);
  return (
    <figure className="h2h-chart">
      <figcaption>
        <span className="h2h-title">{title}</span>
        <span className="h2h-note">
          {note}
          {log ? "; log scale" : ""}
        </span>
      </figcaption>
      <div className="h2h-legend">
        {series.map((s) => (
          <span key={s.fw}>
            <span className="swatch" style={{ ["--c" as string]: colorOf(s.fw) }} aria-hidden="true" />
            {labelOf(files, s.fw)}
          </span>
        ))}
      </div>
      <div ref={ref} className="h2h-plot" onMouseLeave={() => setAt(null)}>
        {width > 0 && values.length > 0 && (
          <svg width={w} height={height} role="img" aria-label={`${title}: ${series.map((s) => `${labelOf(files, s.fw)} ${s.points.map((p) => `${p.x}: ${p.value === null ? "none" : format(p.value)}`).join(", ")}`).join("; ")}`}>
            {yTicks.map((t) => (
              <g key={t}>
                <line className="h2h-gridline" x1={left} x2={w - right} y1={y(t)} y2={y(t)} />
                <text className="h2h-axis" x={left - 8} y={y(t)} textAnchor="end" dominantBaseline="middle">
                  {format(t)}
                </text>
              </g>
            ))}
            {xs.map((label, i) => (
              <text
                key={label}
                className="h2h-axis"
                x={x(i)}
                y={height - 8}
                textAnchor={xs.length > 1 && i === 0 ? "start" : xs.length > 1 && i === xs.length - 1 ? "end" : "middle"}
              >
                {label}
              </text>
            ))}
            {at !== null && <line className="h2h-cross" x1={x(at)} x2={x(at)} y1={top} y2={top + plotH} />}
            {series.map((s) => {
              const pts = s.points.map((p, i) => (p.value !== null && p.value > 0 ? ([x(i), y(p.value)] as const) : null));
              const path = pts.filter(Boolean).map((p, i) => `${i ? "L" : "M"}${p![0]},${p![1]}`).join(" ");
              return (
                <g key={s.fw} style={{ ["--c" as string]: colorOf(s.fw) }}>
                  <path className="h2h-line" d={path} />
                  {pts.map((p, i) => p && <circle key={i} className="h2h-dot" cx={p[0]} cy={p[1]} r={at === i ? 6 : 4.5} />)}
                </g>
              );
            })}
            {xs.map((label, i) => (
              <rect
                key={label}
                className="h2h-hit"
                x={x(i) - plotW / Math.max(xs.length - 1, 1) / 2}
                y={top}
                width={plotW / Math.max(xs.length - 1, 1)}
                height={plotH}
                tabIndex={0}
                onMouseEnter={() => setAt(i)}
                onFocus={() => setAt(i)}
                onBlur={() => setAt(null)}
              />
            ))}
          </svg>
        )}
        {at !== null && (
          <div className="h2h-tip h2h-tip-plot" role="tooltip" style={{ left: Math.min(x(at) + 12, w - 190) }}>
            <b>{xs[at]}</b>
            {series.map((s) => (
              <span key={s.fw}>
                <span className="swatch" style={{ ["--c" as string]: colorOf(s.fw) }} aria-hidden="true" />
                {labelOf(files, s.fw)}: {s.points[at]?.value === null ? "–" : format(s.points[at].value as number)}
                {s.points[at]?.detail ? ` (${s.points[at].detail})` : ""}
              </span>
            ))}
          </div>
        )}
      </div>
    </figure>
  );
}

const OUTCOMES: { key: Outcome; label: string; icon: string }[] = [
  { key: "pass", label: "pass", icon: "✓" },
  { key: "partial", label: "partial", icon: "◐" },
  { key: "fail", label: "fail", icon: "✕" },
];

/** Per framework: its reliability tests as one bar split by outcome. */
function OutcomeBars({ data, fws, files }: { data: BenchFile; fws: string[]; files: BenchFile[] }) {
  const total = Math.max(...fws.map((fw) => data.reliability.filter((r) => r.framework === fw).length), 1);
  return (
    <figure className="h2h-chart h2h-wide">
      <figcaption>
        <span className="h2h-title">Reliability tests passed</span>
        <span className="h2h-note">out of {total}; more green is better</span>
      </figcaption>
      <div className="h2h-bars" role="list">
        {fws.map((fw) => {
          const mine = data.reliability.filter((r) => r.framework === fw);
          return (
            <div key={fw} role="listitem" className="h2h-row" aria-label={`${labelOf(files, fw)}: ${OUTCOMES.map((o) => `${mine.filter((r) => r.outcome === o.key).length} ${o.label}`).join(", ")}`}>
              <span className="h2h-name">
                <span className="swatch" style={{ ["--c" as string]: colorOf(fw) }} aria-hidden="true" />
                {labelOf(files, fw)}
              </span>
              <span className="h2h-track h2h-stack">
                {OUTCOMES.flatMap((o) =>
                  mine
                    .filter((r) => r.outcome === o.key)
                    .map((r) => (
                      <span key={r.test} className={`h2h-seg seg-${o.key}`} style={{ width: `${100 / total}%` }} title={`${r.test}: ${o.label}${r.reason ? `, ${r.reason}` : ""}`}>
                        <span aria-hidden="true">{o.icon}</span>
                      </span>
                    )),
                )}
              </span>
              <span className="h2h-count">
                {OUTCOMES.map((o) => (
                  <span key={o.key} className={`count-${o.key}`}>
                    {o.icon} {mine.filter((r) => r.outcome === o.key).length} {o.label}
                  </span>
                ))}
              </span>
            </div>
          );
        })}
      </div>
    </figure>
  );
}

/** One cell of a numbers table: a number to compare (lower is better),
    or an outcome, or a note when there is no value. */
interface Cell {
  value?: number | null;
  main?: string;
  sub?: string;
  outcome?: Outcome;
  missing?: string;
}

interface TableRow {
  label: string;
  cells: Cell[];
}

/** The numbers behind a group of graphs, opened on request: frameworks as
    columns, the best value of each row marked, the others as a multiple. */
function NumbersTable({ fws, files, rows, first }: { fws: string[]; files: BenchFile[]; rows: TableRow[]; first: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`h2h-table${open ? " open" : ""}`}>
      <button type="button" className="h2h-table-toggle" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span className="h2h-chevron" aria-hidden="true" />
        {open ? "Hide the numbers" : "Show the numbers"}
      </button>
      {open && (
        <div className="h2h-table-frame">
          <table>
            <thead>
              <tr>
                <th scope="col">{first}</th>
                {fws.map((fw) => (
                  <th key={fw} scope="col">
                    <span className="h2h-col">
                      <span className="swatch" style={{ ["--c" as string]: colorOf(fw) }} aria-hidden="true" />
                      {labelOf(files, fw)}
                    </span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                const values = row.cells.map((c) => c.value).filter((v): v is number => typeof v === "number" && Number.isFinite(v));
                const best = values.length > 1 ? Math.min(...values) : null;
                return (
                  <tr key={row.label}>
                    <th scope="row">{row.label}</th>
                    {row.cells.map((c, i) => {
                      if (c.outcome) {
                        const o = OUTCOMES.find((x) => x.key === c.outcome)!;
                        return (
                          <td key={i} className="h2h-cell-outcome">
                            <span className={`h2h-chip chip-${o.key}`}>
                              <span aria-hidden="true">{o.icon}</span> {o.label}
                            </span>
                          </td>
                        );
                      }
                      if (c.main === undefined) {
                        return (
                          <td key={i} className="h2h-cell-missing">
                            {c.missing ?? "–"}
                          </td>
                        );
                      }
                      const isBest = best !== null && c.value === best;
                      return (
                        <td key={i} className={isBest ? "h2h-best" : undefined}>
                          <span className="h2h-cell-main">
                            {c.main}
                            {isBest && <span className="h2h-badge">best</span>}
                          </span>
                          {best !== null && !isBest && typeof c.value === "number" && best > 0 && (
                            <span className="h2h-cell-sub">{times(c.value, best)}</span>
                          )}
                          {c.sub && <span className="h2h-cell-sub">{c.sub}</span>}
                        </td>
                      );
                    })}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** "; 3 batches: 0.38–0.41 s" when a load scenario was repeated. */
function batchSpread(walls: number[] | null | undefined): string {
  if (!walls || walls.length < 2) return "";
  return `; ${walls.length} batches: ${fmtMs(Math.min(...walls))} to ${fmtMs(Math.max(...walls))}`;
}

function Group({ title, lede, children }: { title: string; lede: string; children: ReactNode }) {
  return (
    <div className="h2h-group">
      <h3>{title}</h3>
      <p className="lab-muted">{lede}</p>
      {children}
    </div>
  );
}

export default function HeadToHead({ data, fws, files, label }: { data: BenchFile; fws: string[]; files: BenchFile[]; label: string }) {
  const result = (scenario: string, fw: string) => data.results.find((r) => r.scenario === scenario && r.framework === fw);
  const speed = data.scenarios.filter((s) => s.kind === "speed");
  const load = data.scenarios.filter((s) => s.kind === "load");
  const missing = (scenario: string, fw: string) => {
    const r = result(scenario, fw);
    if (!r) return "not measured";
    return r.status === "unsupported" ? `unsupported${r.note ? `: ${r.note}` : ""}` : r.error ?? "failed";
  };
  const okValue = (scenario: string, fw: string, key: "median_ms" | "wall_ms" | "peak_rss_mb" | "cpu_ms" | "rss_per_run_kb") => {
    const r = result(scenario, fw);
    return r && r.status === "ok" && typeof r[key] === "number" ? (r[key] as number) : null;
  };
  const foot = (fw: string) => data.footprint.find((f) => f.framework === fw);
  const memoryScenario = speed.find((s) => s.id === "agent_call") ?? speed[0];
  const cpuScenario = speed.find((s) => s.id === "agent_tools") ?? speed[0];
  const biggest = load[load.length - 1];

  return (
    <section className="lab-card h2h" aria-labelledby="h2h-title">
      <h2 id="h2h-title">Head to head</h2>
      <p className="lab-muted">
        Every measurement of {label}, the frameworks side by side. Hover or focus a bar for its details; &ldquo;Show the numbers&rdquo; under
        each group opens them as a table, with the best value of each row marked.
      </p>

      {speed.length > 0 && (
        <Group title="Speed" lede="Median time per run, after a warm-up; the label says how many times longer than the fastest.">
          <div className="h2h-grid">
            {speed.map((s) => (
              <CompareBars
                key={s.id}
                title={s.title}
                files={files}
                format={fmtMs}
                rows={fws.map((fw) => {
                  const r = result(s.id, fw);
                  return {
                    fw,
                    value: okValue(s.id, fw, "median_ms"),
                    missing: missing(s.id, fw),
                    detail: r
                      ? [`median ${fmtMs(r.median_ms)}`, `p95 ${fmtMs(r.p95_ms)}`, `fastest ${fmtMs(r.min_ms)}, slowest ${fmtMs(r.max_ms)}`, `${fmtNum(r.runs)} runs`]
                      : undefined,
                  };
                })}
              />
            ))}
          </div>
          <NumbersTable
            first="Median per run"
            fws={fws}
            files={files}
            rows={speed.map((s) => ({
              label: s.title,
              cells: fws.map((fw) => {
                const v = okValue(s.id, fw, "median_ms");
                return v === null
                  ? { missing: missing(s.id, fw) }
                  : { value: v, main: fmtMs(v), sub: `p95 ${fmtMs(result(s.id, fw)?.p95_ms)}, ${fmtNum(result(s.id, fw)?.runs)} runs` };
              }),
            }))}
          />
        </Group>
      )}

      {load.length > 0 && (
        <Group title="Under load" lede="Many agent runs at once in one process (model 100 ms per call, one tool of 50 ms each): how long the whole batch takes, and the memory it needs.">
          <div className="h2h-grid h2h-grid-2">
            <CompareLines
              title="Time for the whole batch"
              files={files}
              format={fmtMs}
              series={fws.map((fw) => ({
                fw,
                points: load.map((s) => ({ x: `${(s as { n?: number }).n ?? s.title} at once`.replace(/^(\d+)(\d{3}) /, "$1,$2 "), value: okValue(s.id, fw, "wall_ms") })),
              }))}
            />
            <CompareLines
              title="Peak memory of the process"
              files={files}
              format={(v) => fmtNum(v, "MB")}
              series={fws.map((fw) => ({
                fw,
                points: load.map((s) => ({ x: `${(s as { n?: number }).n ?? s.title} at once`.replace(/^(\d+)(\d{3}) /, "$1,$2 "), value: okValue(s.id, fw, "peak_rss_mb") })),
              }))}
            />
          </div>
          {biggest && (
            <div className="h2h-grid">
              <CompareBars
                title={`CPU time for ${biggest.title.toLowerCase()}`}
                files={files}
                format={fmtMs}
                rows={fws.map((fw) => ({ fw, value: okValue(biggest.id, fw, "cpu_ms"), missing: missing(biggest.id, fw) }))}
              />
              <CompareBars
                title={`Memory per run, ${biggest.title.toLowerCase()}`}
                files={files}
                format={(v) => fmtNum(v, "KB")}
                rows={fws.map((fw) => ({ fw, value: okValue(biggest.id, fw, "rss_per_run_kb"), missing: missing(biggest.id, fw) }))}
              />
            </div>
          )}
          <NumbersTable
            first="Time for the batch"
            fws={fws}
            files={files}
            rows={load.map((s) => ({
              label: s.title,
              cells: fws.map((fw) => {
                const v = okValue(s.id, fw, "wall_ms");
                return v === null
                  ? { missing: missing(s.id, fw) }
                  : {
                      value: v,
                      main: fmtMs(v),
                      sub: `${fmtNum(okValue(s.id, fw, "peak_rss_mb"), "MB")} peak, CPU ${fmtMs(okValue(s.id, fw, "cpu_ms"))}${batchSpread(result(s.id, fw)?.batch_walls_ms)}`,
                    };
              }),
            }))}
          />
        </Group>
      )}

      {(memoryScenario || cpuScenario) && (
        <Group title="Memory and CPU" lede="What one process needs: its peak memory while running agents one by one, and the CPU time each run costs.">
          <div className="h2h-grid">
            {memoryScenario && (
              <CompareBars
                title={`Peak memory: ${memoryScenario.title.toLowerCase()}`}
                files={files}
                format={(v) => fmtNum(v, "MB")}
                rows={fws.map((fw) => ({
                  fw,
                  value: okValue(memoryScenario.id, fw, "peak_rss_mb"),
                  missing: missing(memoryScenario.id, fw),
                  detail: [`after imports and set-up: ${fmtNum(result(memoryScenario.id, fw)?.baseline_rss_mb, "MB")}`],
                }))}
              />
            )}
            {cpuScenario && (
              <CompareBars
                title={`CPU per run: ${cpuScenario.title.toLowerCase()}`}
                files={files}
                format={fmtMs}
                rows={fws.map((fw) => ({ fw, value: okValue(cpuScenario.id, fw, "cpu_ms"), missing: missing(cpuScenario.id, fw) }))}
              />
            )}
          </div>
        </Group>
      )}

      {data.reliability.length > 0 && (
        <Group title="Reliability" lede="The same faults injected into each framework, with its defaults. Each test's reason and log are in the matrix below.">
          <OutcomeBars data={data} fws={fws} files={files} />
          <NumbersTable
            first="Test"
            fws={fws}
            files={files}
            rows={[...new Set(data.reliability.map((r) => r.test))].map((test) => ({
              label: test.replace(/_/g, " "),
              cells: fws.map((fw) => {
                const r = data.reliability.find((x) => x.test === test && x.framework === fw);
                return r ? { outcome: r.outcome } : { missing: "not tested" };
              }),
            }))}
          />
        </Group>
      )}

      {data.footprint.length > 0 && (
        <Group title="Footprint" lede="What you install and wait for: import time in a fresh interpreter, the size of the virtual environment, and the packages it pulls in.">
          <div className="h2h-grid">
            <CompareBars title="Import time" files={files} format={fmtMs} rows={fws.map((fw) => ({ fw, value: foot(fw)?.import_ms ?? null }))} />
            <CompareBars title="Install size" files={files} format={(v) => fmtNum(v, "MB")} rows={fws.map((fw) => ({ fw, value: foot(fw)?.install_mb ?? null }))} />
            <CompareBars title="Packages installed" files={files} format={(v) => fmtNum(v)} rows={fws.map((fw) => ({ fw, value: foot(fw)?.packages ?? null }))} />
          </div>
          <NumbersTable
            first="Lower is better"
            fws={fws}
            files={files}
            rows={[
              { label: "Import time", fmt: fmtMs, get: (fw: string) => foot(fw)?.import_ms },
              { label: "Install size", fmt: (v: number) => fmtNum(v, "MB"), get: (fw: string) => foot(fw)?.install_mb },
              { label: "Packages installed", fmt: (v: number) => fmtNum(v), get: (fw: string) => foot(fw)?.packages },
            ].map((m) => ({
              label: m.label,
              cells: fws.map((fw) => {
                const v = m.get(fw);
                return typeof v === "number" ? { value: v, main: m.fmt(v) } : { missing: "–" };
              }),
            }))}
          />
        </Group>
      )}
    </section>
  );
}
