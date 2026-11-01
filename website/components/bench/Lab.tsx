"use client";
// The benchmark lab: the recorded results file (and, if the visitor loads
// one, theirs beside it), explored by scenario. Every number shown is read
// from a results file; the page invents none.
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { fmtMs, fmtNum, frameworkIds, labelOf, type BenchFile, type BenchScenario } from "@/lib/bench";
import Distribution from "./Distribution";
import DropZone from "./DropZone";
import HeadToHead from "./HeadToHead";
import Race from "./Race";
import Reliability from "./Reliability";
import { ConcurrencyCharts } from "./Resources";
import Resources from "./Resources";
import { FwName, lanesFor, type DataSet, type Lane } from "./shared";

const KIND_LABEL: Record<string, string> = { speed: "Speed", load: "Load", footprint: "Footprint" };
const KIND_ORDER = ["speed", "load", "footprint"];

function when(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : `${d.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

function RecordCard({ set, href, blobHref, title }: { set: DataSet; href: string | null; blobHref: string | null; title: string }) {
  const { data } = set;
  const m = data.machine;
  const machine = [
    m.cpu,
    m.cores != null && m.threads != null ? `${m.cores} cores / ${m.threads} threads` : m.cores != null ? `${m.cores} cores` : null,
    m.ram_gb != null ? `${fmtNum(m.ram_gb)} GB RAM` : null,
    m.os,
  ]
    .filter(Boolean)
    .join(", ");
  const download = href ?? blobHref;
  return (
    <section className={`lab-card record${set.key === "yours" ? " record-yours" : ""}`} aria-label={title}>
      <div className="record-head">
        <h2>{title}</h2>
        {download && (
          <a className="lab-button" href={download} download={set.key === "ours" ? "criogen-compare-results.json" : undefined}>
            Download raw JSON
          </a>
        )}
      </div>
      <dl className="record-grid">
        <div>
          <dt>Machine</dt>
          <dd>{machine || "not in the file"}</dd>
        </div>
        <div>
          <dt>Python</dt>
          <dd>{data.python ?? "not in the file"}</dd>
        </div>
        <div>
          <dt>Recorded</dt>
          <dd>{when(data.recorded)}</dd>
        </div>
        <div className="record-wide">
          <dt>Frameworks</dt>
          <dd>
            <ul className="record-fws">
              {Object.entries(data.frameworks).map(([id, f]) => (
                <li key={id}>
                  <b>{f.label}</b> {f.version ?? "(version not recorded)"}
                  {f.also && Object.keys(f.also).length > 0 && (
                    <span className="lab-muted"> with {Object.entries(f.also).map(([k, v]) => `${k} ${v}`).join(", ")}</span>
                  )}
                </li>
              ))}
            </ul>
          </dd>
        </div>
        <div className="record-wide">
          <dt>Command</dt>
          <dd>
            <code className="record-cmd">{data.command}</code>
          </dd>
        </div>
        {data.amended && data.amended.length > 0 && (
          <div className="record-wide">
            <dt>Corrections</dt>
            <dd>
              <ul className="record-amended">
                {data.amended.map((m) => (
                  <li key={`${m.at}-${m.what}`}>
                    <b>{m.what}</b> <span className="lab-muted">({when(m.at)})</span>: {m.why}
                  </li>
                ))}
              </ul>
            </dd>
          </div>
        )}
      </dl>
    </section>
  );
}

function StatsTable({ lanes, kind, sets, files }: { lanes: Lane[]; kind: string; sets: DataSet[]; files: BenchFile[] }) {
  return (
    <div className="table-wrap">
      <table className="stats">
        <thead>
          <tr>
            <th scope="col">Framework</th>
            <th scope="col">{kind === "load" ? "Runs at once" : "Runs"}</th>
            <th scope="col">Median</th>
            <th scope="col">p95</th>
            <th scope="col">Min</th>
            <th scope="col">Max</th>
            <th scope="col">{kind === "load" ? "Wall (batch)" : "Wall (all runs)"}</th>
            <th scope="col">{kind === "load" ? "CPU (batch)" : "CPU per run"}</th>
          </tr>
        </thead>
        <tbody>
          {lanes.map((l) => (
            <tr key={`${l.fw}-${l.set.key}`}>
              <th scope="row">
                <FwName fw={l.fw} set={l.set} sets={sets} files={files} />
              </th>
              {l.r.status === "ok" ? (
                <>
                  <td>{fmtNum(l.r.runs, "", 6)}</td>
                  <td>{fmtMs(l.r.median_ms)}</td>
                  <td>{fmtMs(l.r.p95_ms)}</td>
                  <td>{fmtMs(l.r.min_ms)}</td>
                  <td>{fmtMs(l.r.max_ms)}</td>
                  <td>{fmtMs(l.r.wall_ms)}</td>
                  <td>{fmtMs(l.r.cpu_ms)}</td>
                </>
              ) : (
                <td colSpan={7} className={`status-cell status-${l.r.status}`}>
                  {l.r.status === "unsupported" ? "Not supported" : "Failed"}: {l.r.status === "error" ? (l.r.error ?? "no message") : (l.r.note ?? "no note")}
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ModelCost({ lanes, files }: { lanes: Lane[]; files: BenchFile[] }) {
  const items = lanes
    .map((l) => ({ l, us: l.r.model_us ?? l.set.data.frameworks[l.fw]?.model_us ?? null }))
    .filter((x) => x.us != null);
  if (!items.length) return null;
  return (
    <p className="lab-muted model-cost">
      <sup>*</sup> The simulated-model adapter alone, one call, measured in the same process (so the adapters are not what differs):{" "}
      {items.map((x, i) => (
        <span key={`${x.l.fw}-${x.l.set.key}`}>
          {i > 0 && " · "}
          {labelOf(files, x.l.fw)}
          {x.l.set.key === "yours" ? " (your machine)" : ""} {fmtMs(x.us! / 1000)}
        </span>
      ))}
    </p>
  );
}

function Footprint({ sets, fws, files }: { sets: DataSet[]; fws: string[]; files: BenchFile[] }) {
  const rows = fws.flatMap((fw) => sets.map((set) => ({ fw, set, f: set.data.footprint.find((x) => x.framework === fw) }))).filter((r) => r.f);
  if (!rows.length) return <p className="lab-muted">No footprint figures in the file.</p>;
  return (
    <div className="table-wrap">
      <table className="stats">
        <thead>
          <tr>
            <th scope="col">Framework</th>
            <th scope="col">Cold import</th>
            <th scope="col">Packages installed</th>
            <th scope="col">Install size</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(({ fw, set, f }) => (
            <tr key={`${fw}-${set.key}`}>
              <th scope="row">
                <FwName fw={fw} set={set} sets={sets} files={files} />
              </th>
              <td>{fmtMs(f!.import_ms)}</td>
              <td>{fmtNum(f!.packages, "", 6)}</td>
              <td>{fmtNum(f!.install_mb, "MB")}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export default function Lab({
  recorded,
  fixture,
  href,
  recordedErrors,
  runSection,
}: {
  recorded: BenchFile | null;
  fixture: boolean;
  href: string | null;
  recordedErrors: string[];
  runSection: ReactNode;
}) {
  const [yours, setYours] = useState<{ data: BenchFile; name: string } | null>(null);
  const [blobHref, setBlobHref] = useState<string | null>(null);

  // The fixture has no file in public/: offer it from memory.
  useEffect(() => {
    if (!recorded || href) return;
    const url = URL.createObjectURL(new Blob([JSON.stringify(recorded, null, 2)], { type: "application/json" }));
    setBlobHref(url);
    return () => URL.revokeObjectURL(url);
  }, [recorded, href]);

  const sets: DataSet[] = useMemo(() => {
    const out: DataSet[] = [];
    if (recorded) out.push({ key: "ours", tag: "recorded", data: recorded });
    if (yours) out.push({ key: "yours", tag: "your machine", data: yours.data });
    return out;
  }, [recorded, yours]);
  const files = sets.map((s) => s.data);
  const fws = frameworkIds(files);

  const scenarios: BenchScenario[] = useMemo(() => {
    const seen = new Map<string, BenchScenario>();
    for (const f of files) for (const s of f.scenarios) if (!seen.has(s.id)) seen.set(s.id, s);
    return [...seen.values()];
  }, [files]);
  const kinds = [...KIND_ORDER.filter((k) => scenarios.some((s) => s.kind === k)), ...[...new Set(scenarios.map((s) => s.kind))].filter((k) => !KIND_ORDER.includes(k))];

  const [kind, setKind] = useState<string | null>(null);
  const [pick, setPick] = useState<Record<string, string>>({});
  const activeKind = kind && kinds.includes(kind) ? kind : (kinds[0] ?? null);
  const inKind = scenarios.filter((s) => s.kind === activeKind);
  const scenario = inKind.find((s) => s.id === pick[activeKind ?? ""]) ?? inKind[0] ?? null;
  const lanes = scenario ? lanesFor(sets, fws, scenario.id) : [];

  const drop = <DropZone onLoad={(data, name) => setYours({ data, name })} loaded={yours?.name ?? null} onClear={() => setYours(null)} />;

  return (
    <>
      {fixture && (
        <div className="fixture-banner" role="note">
          <b>Fixture data.</b> This build shows <code>fixtures/bench-sample.json</code>: fake numbers for developing the page. They measure
          nothing. Never deploy this build.
        </div>
      )}
      {recordedErrors.length > 0 && (
        <div className="drop-errors" role="alert">
          <p>The recorded results file does not validate:</p>
          <ul>
            {recordedErrors.map((e) => (
              <li key={e}>{e}</li>
            ))}
          </ul>
        </div>
      )}

      {!recorded && (
        <section className="lab-card lab-empty" aria-labelledby="empty-title">
          <span className="empty-dot" aria-hidden="true" />
          <h2 id="empty-title">No results recorded yet</h2>
          <p>
            The comparison has not been run and committed yet, so there are no numbers to show here. When it is, this page shows the results
            file exactly as recorded: the machine, the versions, the command and every measurement.
          </p>
          <p>Until then you can run it yourself and load your own file below{yours ? "; yours is shown on this page now" : ""}.</p>
        </section>
      )}

      {sets.map((s) => (
        <RecordCard key={s.key} set={s} href={s.key === "ours" ? href : null} blobHref={s.key === "ours" ? blobHref : null} title={s.key === "ours" ? "The recorded run" : `Your machine: ${yours?.name ?? ""}`} />
      ))}

      {sets.map((set) => (
        <HeadToHead
          key={`h2h-${set.key}`}
          data={set.data}
          fws={fws.filter((fw) => set.data.frameworks[fw] || set.data.results.some((r) => r.framework === fw))}
          files={files}
          label={set.key === "ours" ? "the recorded run" : "your machine's run"}
        />
      ))}

      {sets.length > 0 && activeKind && (
        <section className="lab-card explorer" aria-labelledby="explore-title">
          <h2 id="explore-title">Scenarios</h2>
          <div className="kind-switch" role="group" aria-label="Kind of scenario">
            {kinds.map((k) => (
              <button key={k} type="button" aria-pressed={k === activeKind} onClick={() => setKind(k)}>
                {KIND_LABEL[k] ?? k}
              </button>
            ))}
          </div>
          {inKind.length > 1 && (
            <div className="scenario-chips" role="group" aria-label="Scenario">
              {inKind.map((s) => (
                <button key={s.id} type="button" aria-pressed={s.id === scenario?.id} onClick={() => setPick((p) => ({ ...p, [activeKind]: s.id }))}>
                  {s.title}
                </button>
              ))}
            </div>
          )}
          {scenario && (
            <div className="scenario">
              <div className="scenario-head">
                <h3>
                  {scenario.title} <code>{scenario.id}</code>
                </h3>
                {scenario.detail && <p className="lab-muted">{scenario.detail}</p>}
              </div>

              {lanes.length > 0 ? (
                <>
                  <h4 className="sub">The race</h4>
                  <Race lanes={lanes} scenario={scenario} sets={sets} files={files} />
                  <ModelCost lanes={lanes} files={files} />

                  <h4 className="sub">Every number</h4>
                  <StatsTable lanes={lanes} kind={scenario.kind} sets={sets} files={files} />

                  <h4 className="sub">Distribution</h4>
                  <Distribution lanes={lanes} sets={sets} files={files} kind={scenario.kind} />

                  <h4 className="sub">Memory and CPU</h4>
                  <Resources lanes={lanes} kind={scenario.kind} sets={sets} files={files} />
                </>
              ) : scenario.kind === "footprint" ? (
                <Footprint sets={sets} fws={fws} files={files} />
              ) : (
                <p className="lab-muted">No results for this scenario in the file.</p>
              )}

              {scenario.kind === "load" && <ConcurrencyCharts sets={sets} fws={fws} files={files} />}
            </div>
          )}
        </section>
      )}

      {sets.length > 0 && (
        <section className="lab-card" aria-labelledby="rel-title">
          <h2 id="rel-title">Reliability</h2>
          <p className="lab-muted">
            Each test injects the same fault through the shared model or tools, with each framework&apos;s defaults. Select a cell for the
            reason and the log.
          </p>
          <ul className="outcome-key" aria-label="Key">
            <li>
              <span className="outcome outcome-pass">✓ pass</span> as the test defines it
            </li>
            <li>
              <span className="outcome outcome-partial">◐ partial</span> part of it
            </li>
            <li>
              <span className="outcome outcome-fail">✕ fail</span> not at all
            </li>
          </ul>
          <Reliability sets={sets} fws={fws} files={files} />
        </section>
      )}

      {sets.length > 0 && (
        <section className="lab-card" aria-labelledby="foot-title">
          <h2 id="foot-title">Footprint</h2>
          <p className="lab-muted">What you install, and how long the import takes in a fresh interpreter.</p>
          <Footprint sets={sets} fws={fws} files={files} />
        </section>
      )}

      <section className="lab-card run-yourself" aria-labelledby="run-title">
        <h2 id="run-title">Check it on your machine</h2>
        {runSection}
        {drop}
      </section>
    </>
  );
}
