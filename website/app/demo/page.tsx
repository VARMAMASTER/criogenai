import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/lab.css";
import "@/styles/playground.css";
import "@/styles/demo.css";
import Code from "@/components/Code";
import Chat from "@/components/demo/Chat";
import DemoTabs from "@/components/demo/DemoTabs";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { byGroup, COVERAGE, loadTour, valueText, type TourFeature, type TourFile } from "@/lib/tour";

export const metadata: Metadata = {
  title: "Demo",
  description:
    "Every Criogen feature on the status page, run for real: the code, what it printed and the checks it passed, with a simulated model and with a real one.",
};

const RUN_COMMANDS = [
  "# needs the criogen Python package (coming to PyPI)",
  "python demos/feature-tour/tour.py                 # simulated model: no network, no keys",
  "python demos/feature-tour/tour.py --live gemini   # a real model (GEMINI_API_KEY); or: --live openai",
].join("\n");

async function Card({ f }: { f: TourFeature }) {
  const passed = f.checks.filter((c) => c.ok).length;
  return (
    <details className="demo-card" id={f.id}>
      <summary>
        <span className={`demo-mark ${f.ok ? "demo-ok" : "demo-fail"}`} aria-label={f.ok ? "passed" : "failed"}>
          {f.ok ? "✓" : "✗"}
        </span>
        <span className="demo-title">{f.title}</span>
        <span className="demo-meta">
          {passed}/{f.checks.length} checks · {f.ms >= 1000 ? `${(f.ms / 1000).toFixed(1)} s` : `${f.ms} ms`}
          {f.attempts && f.attempts > 1 ? ` · needed ${f.attempts} attempts` : ""}
        </span>
      </summary>
      <div className="demo-body">
        <p className="demo-claim">{f.claim}</p>
        <h4>Checks</h4>
        <ul className="demo-checks">
          {f.checks.map((c) => (
            <li key={c.claim} className={c.ok ? "demo-ok" : "demo-fail"}>
              <span aria-hidden="true">{c.ok ? "✓" : "✗"}</span> {c.claim}
            </li>
          ))}
        </ul>
        {f.error && <p className="pg-error">{f.error}</p>}
        <h4>What it printed</h4>
        <dl className="demo-steps">
          {f.steps.map((s, i) => (
            <div key={i}>
              <dt>{s.label}</dt>
              <dd>
                <pre>{valueText(s.value)}</pre>
              </dd>
            </div>
          ))}
        </dl>
        <h4>The code that ran</h4>
        <div className="prose">
          <Code code={f.code} lang="python" />
        </div>
      </div>
    </details>
  );
}

async function Tour({ tour }: { tour: TourFile }) {
  const total = tour.features.length;
  const ok = tour.features.filter((f) => f.ok).length;
  return (
    <section aria-label={tour.model ? "Tour with a real model" : "Tour with a simulated model"}>
      <p className="lab-muted">
        <b>
          {ok} of {total} scenarios passed.
        </b>{" "}
        {tour.model
          ? `A real model (${tour.model.provider} ${tour.model.name}) made the decisions, so these check behaviour, never exact wording; a scenario that needed a second attempt says so.`
          : "A scripted model gives the same replies every time, so every scenario is exact and repeatable."}{" "}
        {tour.sdk.name} {tour.sdk.version} ({tour.sdk.language}), {tour.machine.os}, {tour.ran_at.slice(0, 10)}.
      </p>
      {byGroup(tour.features).map(([group, items]) => (
        <div key={group} className="demo-group">
          <h3>{group}</h3>
          {items.map((f) => (
            <Card key={f.id} f={f} />
          ))}
        </div>
      ))}
    </section>
  );
}

export default async function DemoPage() {
  const sim = loadTour("tour.json");
  const live = loadTour("live.json");
  const status = (id: string, tour: TourFile | null) => tour?.features.find((f) => f.id === id)?.ok;
  return (
    <div className="site lab">
      <a className="skip" href="#main">
        Skip to content
      </a>
      <SiteHeader current="demo" />
      <main id="main" className="lab-main">
        <header className="lab-hero">
          <p className="lab-eyebrow">Demo</p>
          <h1>Every feature, run for real</h1>
          <p className="lab-lede">
            Each scenario below is ordinary Criogen code. It was run, and the page shows that exact code, what it printed and the checks it had to pass.
            Chat with the agent yourself, or read the recorded tours: one with a scripted model, so it is exact and repeatable, and one where a real model decides.
          </p>
          <p className="lab-muted">
            Nothing here is mocked at the SDK level: the Rust engine, storage, approvals, hooks and audit trail are the real ones. Results are recorded files
            you can regenerate yourself (see the end of the page).
          </p>
        </header>

        {sim ? (
          <>
            <section className="lab-card" aria-labelledby="demo-tour">
              <h2 id="demo-tour">Try it</h2>
              <DemoTabs
                initial="chat"
                tabs={[
                  { id: "chat", label: "Try it: chat", node: <Chat /> },
                  { id: "simulated", label: `Scripted model (${sim.features.length})`, node: <Tour tour={sim} /> },
                  ...(live ? [{ id: "live", label: `Real model: ${live.model?.name} (${live.features.length})`, node: <Tour tour={live} /> }] : []),
                ]}
              />
            </section>

            <section className="lab-card" aria-labelledby="demo-cov">
              <h2 id="demo-cov">Feature list and what shows it</h2>
              <div className="pg-scroll">
                <table className="pg-table demo-cov">
                  <thead>
                    <tr>
                      <th scope="col">Feature (from the status page)</th>
                      <th scope="col">Scripted model</th>
                      <th scope="col">Real model</th>
                    </tr>
                  </thead>
                  <tbody>
                    {COVERAGE.map((row) => (
                      <tr key={row.feature}>
                        <th scope="row">{row.feature}</th>
                        <td>
                          {row.sim.map((id) => (
                            <a key={id} href={`#${id}`} className={status(id, sim) ? "demo-ok" : "demo-fail"}>
                              {status(id, sim) ? "✓" : "✗"} {id}
                            </a>
                          ))}
                        </td>
                        <td>
                          {row.live.length === 0 && <span className="lab-muted">–</span>}
                          {row.live.map((id) => (
                            <a key={id} href={`#${id}`} className={status(id, live) ? "demo-ok" : "demo-fail"}>
                              {status(id, live) ? "✓" : "✗"} {id}
                            </a>
                          ))}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <h3 className="pg-sub">Not shown in this tour</h3>
              <ul className="pg-small lab-muted">
                <li>Postgres storage and cross-process locks: their tests need a database server and run in CI.</li>
                <li>TypeScript-only Zod / Standard Schema tool input, and the Rust API: the same engine; see the docs for each language.</li>
                <li>The LLM gateway server (OpenAI- and Anthropic-compatible endpoints, rate limits, budgets): see the gateway guide.</li>
                <li>Langfuse itself: the tour exports to a local OpenTelemetry collector, which Langfuse accepts.</li>
              </ul>
            </section>

          </>
        ) : (
          <section className="lab-card">
            <p className="lab-muted">The tour has not been recorded yet. Run it as shown below, then rebuild the site.</p>
          </section>
        )}

        <section className="lab-card" aria-labelledby="demo-run">
          <h2 id="demo-run">Run it yourself</h2>
          <p className="lab-muted">
            The tour is one Python file. It writes the files this page reads; keys for the real-model tour are read from <code>~/.criogen-keys.env</code>{" "}
            and never written anywhere.
          </p>
          <div className="prose">
            <Code code={RUN_COMMANDS} lang="sh" />
          </div>
        </section>
      </main>
      <SiteFooter />
    </div>
  );
}
