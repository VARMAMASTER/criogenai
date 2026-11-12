import fs from "node:fs";
import path from "node:path";
import type { Metadata } from "next";
import { pageMeta, speedLine } from "@/lib/seo";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/lab.css";
import "@/styles/playground.css";
import Lab from "@/components/bench/Lab";
import Code from "@/components/Code";
import Playground from "@/components/playground/Playground";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { loadRecorded } from "@/lib/bench-data";
import type { Recorded as CompareRecorded } from "@/lib/playground";

export const metadata: Metadata = pageMeta({
  title: "AI agent framework benchmarks: CriogenAI vs LangGraph vs CrewAI",
  description: `Which AI agent framework is fastest? CriogenAI, LangGraph and CrewAI on the same work with the same simulated model: speed, load, memory, reliability and footprint, from the raw results file. ${speedLine()}`,
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

/** The live comparison's recordings, made by playground/record.py into public/playground/. */
function loadCompare(): CompareRecorded | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(process.cwd(), "public", "playground", "recorded.json"), "utf-8")) as CompareRecorded;
  } catch {
    return null;
  }
}

const COMPARE_COMMANDS = [
  "python benches/compare/setup.py --only criogen   # once",
  "python benches/compare/setup.py --only langgraph    # once",
  "target/compare/venvs/criogen/bin/python playground/server.py   # Windows: Scripts/python.exe",
  "# serves on http://127.0.0.1:4600; reload this page",
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
          <h1>CriogenAI, LangGraph and CrewAI, on the same work</h1>
          <p className="lab-lede">
            All three in Python, in the way their own documentation shows, against one simulated model with the same replies and delays.
            Each scenario runs in a fresh process per framework. Everything below is read from one results file: the machine, the versions,
            the command and every measurement.
          </p>
          <p className="lab-muted">
            For what CriogenAI&apos;s Rust engine, SDK bridges and gateway cost on their own, see <a href="/docs/reference/benchmarks/">Benchmarks</a> in
            the docs.
          </p>
        </header>
        <Lab recorded={recorded.data} fixture={recorded.fixture} href={recorded.href} recordedErrors={recorded.errors} runSection={runSection} />
        <header className="lab-hero" id="live">
          <p className="lab-eyebrow">Live comparison</p>
          <h2 className="pg-title">CriogenAI and LangChain on one job, side by side</h2>
          <p className="lab-lede">
            Send a message through the same agent in both frameworks and compare latency, throughput, CPU and memory. Each framework runs in its
            own process against one scripted model server, and the measurements are taken from outside by the same code. For exploration on a
            single machine; the results above are the reference.
          </p>
        </header>
        <Playground recorded={loadCompare()} />
        <section className="lab-card" aria-labelledby="pg-method">
          <h2 id="pg-method">How the live comparison works</h2>
          <dl className="pg-method">
            <dt>Model</dt>
            <dd>
              One local server speaks the OpenAI wire format and gives both frameworks identical scripted replies. Each framework builds requests and
              parses responses with its own client. No real model is called.
            </dd>
            <dt>Agents</dt>
            <dd>
              Each framework&apos;s standard tool-calling agent with default settings: <code>Agent</code> in CriogenAI, <code>create_agent</code> in
              LangChain. Both use the same synchronous tools with the same names, arguments and delays.
            </dd>
            <dt>Measurement</dt>
            <dd>
              CPU and memory are sampled every 100 ms from outside each process with <code>psutil</code>, including import time. The model server runs
              in a separate process and is charged to neither framework.
            </dd>
            <dt>Order</dt>
            <dd>The benchmark runs A, B, B, A, each pass in a fresh process, and averages the two passes of each framework.</dd>
          </dl>
          <h3 className="pg-sub">Known differences and limits</h3>
          <ul>
            <li>
              Defaults differ. CriogenAI streams the reply (<code>stream: true</code>) and LangChain requests a complete one (<code>stream: false</code>),
              which adds parsing work on the CriogenAI side. This compares defaults, not feature sets.
            </li>
            <li>
              With realistic model and tool latency, both spend most of a run waiting, so median run time converges. The differences that remain are
              memory, CPU and behaviour under concurrent load.
            </li>
            <li>One machine, one measurement per click. Repeat before drawing conclusions from small differences.</li>
          </ul>
          <h3 className="pg-sub">Run it locally</h3>
          <p className="lab-muted">
            The server binds to 127.0.0.1 and accepts requests only from this site&apos;s pages. Set up the two Python environments once, then start it
            and reload this page; the source switches to Live.
          </p>
          <div className="prose">
            <Code code={COMPARE_COMMANDS} lang="sh" />
          </div>
        </section>
      </main>
      <SiteFooter />
    </div>
  );
}
