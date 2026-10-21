"use client";
// The reliability matrix: tests × frameworks, each cell the recorded
// outcome (pass / partial / fail in the run-state colours, with a word and a
// symbol, never colour alone). A cell opens to the reason and the log.
import { Fragment, useState } from "react";
import { labelOf, type BenchFile, type BenchReliability, type Outcome } from "@/lib/bench";
import type { DataSet } from "./shared";

// The tests and what passing means, from design 0016 §3 (definitions, not results).
const TESTS: Record<string, { fault: string; pass: string }> = {
  model_transient: { fault: "The model raises a rate-limit error once, then answers.", pass: "The run completes without the caller's code retrying." },
  tool_exception: { fault: "A tool raises.", pass: "The model sees the error and the run completes with an answer." },
  bad_arguments: {
    fault: "The model calls a tool with a wrong argument type.",
    pass: "The tool function never sees the bad arguments, and the run completes.",
  },
  crash_resume: {
    fault: "The process is killed while the 2nd of 2 tools runs (the 1st finished).",
    pass: "A new process continues the same conversation from storage, and the finished tool is not run again.",
  },
  runaway_loop: { fault: "The model asks for a tool forever.", pass: "The run stops by itself, with an error, within its default limit." },
};
const ORDER = Object.keys(TESTS);
const SYMBOL: Record<Outcome, string> = { pass: "✓", partial: "◐", fail: "✕" };

function Pill({ outcome, tag }: { outcome: Outcome; tag?: string }) {
  return (
    <span className={`outcome outcome-${outcome}`}>
      <span aria-hidden="true">{SYMBOL[outcome]}</span> {outcome}
      {tag && <span className="outcome-tag">{tag}</span>}
    </span>
  );
}

export default function Reliability({ sets, fws, files }: { sets: DataSet[]; fws: string[]; files: BenchFile[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const seen = new Set<string>();
  for (const s of sets) for (const r of s.data.reliability) seen.add(r.test);
  const tests = [...ORDER.filter((t) => seen.has(t)), ...[...seen].filter((t) => !ORDER.includes(t)).sort()];
  if (!tests.length) return <p className="lab-muted">No reliability results in the file.</p>;
  const cols = fws.filter((fw) => sets.some((s) => s.data.reliability.some((r) => r.framework === fw)));
  const entry = (set: DataSet, test: string, fw: string): BenchReliability | undefined =>
    set.data.reliability.find((r) => r.test === test && r.framework === fw);

  return (
    <div className="table-wrap rel-wrap">
      <table className="rel">
        <thead>
          <tr>
            <th scope="col">Test</th>
            {cols.map((fw) => (
              <th scope="col" key={fw}>
                {labelOf(files, fw)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {tests.map((test) => {
            const openFw = open?.startsWith(`${test}|`) ? open.split("|")[1] : null;
            return (
              <Fragment key={test}>
                <tr>
                  <th scope="row">
                    <code>{test}</code>
                    {TESTS[test] && (
                      <span className="rel-def">
                        {TESTS[test].fault} <em>Pass: {TESTS[test].pass}</em>
                      </span>
                    )}
                  </th>
                  {cols.map((fw) => {
                    const key = `${test}|${fw}`;
                    const found = sets.map((s) => ({ s, e: entry(s, test, fw) })).filter((x) => x.e);
                    return (
                      <td key={fw}>
                        {found.length ? (
                          <button
                            type="button"
                            className="rel-cell"
                            aria-expanded={open === key}
                            aria-controls={`rel-${test}`}
                            onClick={() => setOpen(open === key ? null : key)}
                            aria-label={`${labelOf(files, fw)}, ${test}: ${found.map((x) => `${x.e!.outcome}${sets.length > 1 ? ` (${x.s.tag})` : ""}`).join(", ")}. Show the reason and the log.`}
                          >
                            {found.map((x) => (
                              <Pill key={x.s.key} outcome={x.e!.outcome} tag={sets.length > 1 ? x.s.tag : undefined} />
                            ))}
                          </button>
                        ) : (
                          <span className="lab-muted">not run</span>
                        )}
                      </td>
                    );
                  })}
                </tr>
                {openFw && (
                  <tr className="rel-detail" id={`rel-${test}`}>
                    <td colSpan={cols.length + 1}>
                      <div className="rel-detail-inner">
                      {sets.map((s) => {
                        const e = entry(s, test, openFw);
                        if (!e) return null;
                        return (
                          <div key={s.key} className="rel-entry">
                            <p>
                              <b>{labelOf(files, openFw)}</b>
                              {sets.length > 1 && <span className="set-tag">{s.tag}</span>} <Pill outcome={e.outcome} />
                            </p>
                            <p>
                              <span className="lab-muted">Reason: </span>
                              {e.reason ?? "none given in the file."}
                            </p>
                            {e.note && (
                              <p>
                                <span className="lab-muted">Note: </span>
                                {e.note}
                              </p>
                            )}
                            {e.log ? <pre className="rel-log">{e.log}</pre> : <p className="lab-muted">No log in the file.</p>}
                          </div>
                        );
                      })}
                      </div>
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
