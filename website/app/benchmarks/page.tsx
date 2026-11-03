import type { Metadata } from "next";
import { pageMeta, speedLine } from "@/lib/seo";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/lab.css";
import Lab from "@/components/bench/Lab";
import Code from "@/components/Code";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { loadRecorded } from "@/lib/bench-data";

export const metadata: Metadata = pageMeta({
  title: "AI agent framework benchmarks: Criogen vs LangGraph vs CrewAI",
  description: `Which AI agent framework is fastest? Criogen, LangGraph and CrewAI on the same work with the same simulated model: speed, load, memory, reliability and footprint, from the raw results file. ${speedLine()}`,
  path: "/benchmarks/",
});

/** The kit is built by benches/compare/package.py into public/bench/. */
const KIT = "/bench/criogen-compare-kit.zip";
const KIT_COMMANDS = [
  "cd criogen-compare-kit",
  "python setup.py      # once: a virtual environment per framework (pinned versions)",
  "python run.py        # everything, about 25 minutes → bench/results/<date>-<machine>.json",
  "python run.py quick  # a short smoke test (not for comparing)",
].join("\n");

export default function BenchmarksPage() {
  const recorded = loadRecorded();
  const runSection = (
    <>
      <p className="lab-muted">
        Download the kit: the benchmark exactly as it ran here, with the code for each framework and the method (<code>METHOD.md</code>).
        It needs Python 3.10 to 3.13 and no API keys: every framework talks to the same scripted model.
      </p>
      <p>
        <a className="lab-button" href={KIT} download>
          Download the benchmark kit (.zip)
        </a>
      </p>
      <div className="prose">
        <Code code={KIT_COMMANDS} lang="sh" />
      </div>
      <p className="lab-muted">Then load the file it writes:</p>
    </>
  );
  return (
    <div className="site lab">
      <a className="skip" href="#main">
        Skip to content
      </a>
      <SiteHeader current="benchmarks" />
      <main id="main" className="lab-main">
        <header className="lab-hero">
          <p className="lab-eyebrow">Benchmark lab</p>
          <h1>Criogen, LangGraph and CrewAI, on the same work</h1>
          <p className="lab-lede">
            All three in Python, in the way their own documentation shows, against one simulated model with the same replies and delays.
            Each scenario runs in a fresh process per framework. Everything below is read from one results file: the machine, the versions,
            the command and every measurement.
          </p>
          <p className="lab-muted">
            For what Criogen&apos;s Rust engine, SDK bridges and gateway cost on their own, see <a href="/docs/reference/benchmarks/">Benchmarks</a> in
            the docs.
          </p>
        </header>
        <Lab recorded={recorded.data} fixture={recorded.fixture} href={recorded.href} recordedErrors={recorded.errors} runSection={runSection} />
      </main>
      <SiteFooter />
    </div>
  );
}
