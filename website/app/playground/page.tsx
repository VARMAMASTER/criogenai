import fs from "node:fs";
import path from "node:path";
import type { Metadata } from "next";
import { pageMeta } from "@/lib/seo";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/lab.css";
import "@/styles/playground.css";
import Code from "@/components/Code";
import Playground from "@/components/playground/Playground";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import type { Recorded } from "@/lib/playground";

export const metadata: Metadata = pageMeta({
  title: "Playground: Criogen vs LangChain, side by side",
  description:
    "Type one message and watch Criogen and LangChain handle it, side by side, against the same fake model server and the same tools, with CPU and memory measured from outside.",
  path: "/playground/",
});

/** The recordings are made by playground/record.py into public/playground/. */
function loadRecorded(): Recorded | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(process.cwd(), "public", "playground", "recorded.json"), "utf-8")) as Recorded;
  } catch {
    return null;
  }
}

const RUN_COMMANDS = [
  "python benches/compare/setup.py --only criogen   # once",
  "python benches/compare/setup.py --only langgraph    # once",
  "target/compare/venvs/criogen/bin/python playground/server.py   # Windows: Scripts/python.exe",
  "# serves on http://127.0.0.1:4600; reload this page",
].join("\n");

export default function PlaygroundPage() {
  return (
    <div className="site lab">
      <a className="skip" href="#main">
        Skip to content
      </a>
      <SiteHeader current="playground" />
      <main id="main" className="lab-main">
        <header className="lab-hero">
          <p className="lab-eyebrow">Playground</p>
          <h1>Compare Criogen and LangChain on one job</h1>
          <p className="lab-lede">
            Send a message through the same agent in both frameworks and compare latency, throughput, CPU and memory. Each framework runs in its
            own process against one scripted model server, and the measurements are taken from outside by the same code.
          </p>
          <p className="lab-muted">
            For exploration on a single machine. The <a href="/benchmarks/">benchmark lab</a> holds the reference results.
          </p>
        </header>
        <Playground recorded={loadRecorded()} />
        <section className="lab-card" aria-labelledby="pg-method">
          <h2 id="pg-method">Method</h2>
          <dl className="pg-method">
            <dt>Model</dt>
            <dd>
              One local server speaks the OpenAI wire format and gives both frameworks identical scripted replies. Each framework builds requests and
              parses responses with its own client. No real model is called.
            </dd>
            <dt>Agents</dt>
            <dd>
              Each framework&apos;s standard tool-calling agent with default settings: <code>Agent</code> in Criogen, <code>create_agent</code> in
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
              Defaults differ. Inspecting the requests the server received shows Criogen streaming the reply (<code>stream: true</code>) and LangChain
              requesting a complete one (<code>stream: false</code>). This adds parsing work on the Criogen side. Defaults also differ in what each
              framework does for you, such as argument validation and retries; this compares defaults, not feature sets.
            </li>
            <li>
              With realistic model and tool latency, both spend most of a run waiting, so median run time converges. The differences that remain are
              memory, CPU and behaviour under concurrent load.
            </li>
            <li>One machine, one measurement per click. Repeat before drawing conclusions from small differences.</li>
          </ul>
        </section>
        <section className="lab-card" aria-labelledby="pg-run">
          <h2 id="pg-run">Run it locally</h2>
          <p className="lab-muted">
            The server binds to 127.0.0.1 and accepts requests only from this site&apos;s pages. Set up the two Python environments once, then start it
            and reload this page; the source switches to Live.
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
