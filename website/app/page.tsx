import type { CSSProperties } from "react";
import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/landing.css";
import "@/styles/trace.css";
import "@/styles/scene.css";
import InstallCommand from "@/components/landing/InstallCommand";
import InstallCounter from "@/components/landing/InstallCounter";
import RunTrace from "@/components/landing/RunTrace";
import Scene from "@/components/landing/Scene";
import { crash, gateway, workflow } from "@/components/landing/scenes";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import TabsClient from "@/components/TabsClient";
import { highlight } from "@/lib/highlight";
import { installs } from "@/lib/installs";
import { LANG_ORDER, LANGS, snippet, type Lang } from "@/lib/snippets";

export const metadata: Metadata = {
  title: { absolute: "AgentBuild: AI agents that can wait for a person, survive a crash and show what they did" },
  description:
    "Run AI agents and workflows on a Rust engine from Python, TypeScript or Rust: tools, human approval, durable conversations, memory, encryption, an audit trail and an LLM gateway.",
};

// Python and TypeScript are published first; the Rust crates come when
// the code opens.
const install: Partial<Record<Lang, string>> = {
  python: "pip install agentbuild-sdk",
  typescript: "npm install agentbuild-sdk",
};

// The lifecycle of a run, in the order it happens; each row links to its guide.
const lifecycle = [
  {
    state: "running",
    title: "Define agents, tools and workflows in your language",
    text: "Python functions, TypeScript functions or Rust closures become tools. Their arguments are checked against a schema before your code runs, so a model's mistake never reaches it.",
    href: "/docs/guides/agents/",
    link: "Agents and tools",
  },
  {
    state: "running",
    title: "Run steps in parallel, safely",
    text: "Tool calls of one turn run at the same time. A step commits completely or not at all, and limits stop a run that loops.",
    href: "/docs/concepts/runs/",
    link: "How a run works",
  },
  {
    state: "waiting",
    title: "Pause for a person, for as long as it takes",
    text: "Mark the risky tools. The run stops before they run and waits in the database until someone approves, rejects or edits the call, from any process.",
    href: "/docs/guides/approvals/",
    link: "Human approval",
  },
  {
    state: "done",
    title: "Keep every step, survive every crash",
    text: "Conversations are saved after every step and every tool call, in SQLite or Postgres. After a crash, retry continues where the run stopped, without running finished tools again.",
    href: "/docs/guides/threads/",
    link: "Conversations",
  },
  {
    state: "done",
    title: "Prove who did what",
    text: "An audit trail records runs, model calls, tool calls and decisions with the person who made them, and no message content. Data can be encrypted at rest and deleted on a schedule.",
    href: "/docs/guides/compliance/",
    link: "Encryption, audit, retention",
  },
  {
    state: "running",
    title: "Use any model, including your own",
    text: "OpenAI, Claude, Gemini, OpenRouter, or models on your own hardware through Ollama and vLLM. A gateway puts all of them behind one OpenAI-compatible endpoint.",
    href: "/docs/guides/models/",
    link: "Models and the gateway",
  },
];

// The animated runs, each with what it shows and where to read more.
const scenes = [
  {
    data: workflow,
    title: "Branch, run in parallel, wait for a person",
    text: "A router picks the branches, three searches run at once, and the run stops before the refund. Approve or reject it yourself.",
    href: "/docs/guides/workflows/",
    link: "Workflows",
  },
  {
    data: crash,
    title: "Kill the process, lose nothing",
    text: "Every step and every finished tool call is saved. A retry, even from another machine, continues where the run stopped.",
    href: "/docs/guides/threads/",
    link: "Conversations and recovery",
  },
  {
    data: gateway,
    title: "One endpoint for every model",
    text: "Apps speak the OpenAI format to the gateway; routes decide which model answers, and fall back when one fails.",
    href: "/docs/guides/gateway/",
    link: "The LLM gateway",
  },
];

// Soft lights behind the glass, down the page (glass.css).
const CYAN = "rgba(127, 214, 232, 0.55)";
const INDIGO = "rgba(108, 92, 255, 0.5)";
const MINT = "rgba(139, 217, 160, 0.42)";
const AMBER = "rgba(242, 181, 68, 0.32)";
const orbs = [
  { top: "2%", side: "right", x: "-8vw", size: "46rem", color: CYAN, time: "20s", dx: "-5vw", dy: "4vw" },
  { top: "10%", side: "left", x: "-14vw", size: "40rem", color: INDIGO, time: "26s", dx: "6vw", dy: "-3vw" },
  { top: "24%", side: "right", x: "2vw", size: "38rem", color: MINT, time: "24s", dx: "-7vw", dy: "-4vw" },
  { top: "34%", side: "left", x: "8vw", size: "34rem", color: AMBER, time: "30s", dx: "8vw", dy: "3vw" },
  { top: "46%", side: "right", x: "-10vw", size: "44rem", color: INDIGO, time: "22s", dx: "-6vw", dy: "5vw" },
  { top: "58%", side: "left", x: "-6vw", size: "40rem", color: CYAN, time: "28s", dx: "7vw", dy: "-4vw" },
  { top: "72%", side: "right", x: "6vw", size: "36rem", color: MINT, time: "25s", dx: "-5vw", dy: "-3vw" },
  { top: "86%", side: "left", x: "20vw", size: "42rem", color: INDIGO, time: "27s", dx: "6vw", dy: "-5vw" },
];

const faq = [
  {
    q: "Is it ready for production?",
    a: "It is 0.1: tested on Windows and Linux with simulated and real models, but young. The API may change before 1.0. Start with a pilot, and see exactly what is built on the status page.",
  },
  {
    q: "Does my data leave my servers?",
    a: "Only to the model provider you choose. With a self-hosted model (Ollama, vLLM) nothing leaves your network. Storage is your own SQLite file or Postgres database.",
  },
  {
    q: "How is it different from LangGraph or CrewAI?",
    a: "AgentBuild's engine is written in Rust and shared by its Python, TypeScript and Rust APIs, so a run behaves the same in all three. Its storage includes encryption at rest and a hash-chained audit trail. The measured cost per step, compared with LangGraph on the same workflows, is on the benchmarks page.",
  },
  {
    q: "What does it cost?",
    a: "Nothing: AgentBuild is free to use, also commercially. You pay your model provider, if you use one. The source code opens (MIT or Apache-2.0) once AgentBuild reaches 10,000 installs.",
  },
];

export default async function Home() {
  const code = await Promise.all(LANG_ORDER.map((lang) => highlight(snippet(lang, "quickstart"), LANGS[lang].ext)));
  return (
    <div className="site landing">
      <div className="orbs" aria-hidden="true">
        {orbs.map((o, i) => (
          <span
            key={i}
            className="orb"
            style={
              {
                top: o.top,
                [o.side]: o.x,
                "--s": o.size,
                "--c": o.color,
                "--t": o.time,
                "--dx": o.dx,
                "--dy": o.dy,
              } as CSSProperties
            }
          />
        ))}
      </div>
      <a className="skip" href="#main">
        Skip to content
      </a>
      <SiteHeader />

      <main id="main">
        <section className="hero">
          <div className="hero-copy">
            <h1>Agents that can wait for a person, survive a crash, and show what they did.</h1>
            <p className="lede">
              AgentBuild runs agents and workflows on a Rust engine. Write your tools in Python, TypeScript or Rust; it handles the
              steps, the pauses, the saving and the record.
            </p>
            <InstallCommand items={LANG_ORDER.filter((l) => install[l]).map((l) => ({ label: LANGS[l].label, command: install[l] as string }))} />
            <div className="actions">
              <a className="button primary" href="/docs/quickstart/">
                Start the quickstart
              </a>
              <a className="button" href="/docs/">
                Read the docs
              </a>
            </div>
            <p className="fine">Free to use, also commercially. Version 0.1, early.</p>
            <InstallCounter installs={await installs()} />
          </div>
          <RunTrace />
        </section>

        <section className="watch" aria-labelledby="watch-title">
          <h2 id="watch-title">Watch it run</h2>
          <p className="section-lede">
            Three runs, step by step, in the colours of a run: running, waiting for a person, done, failed. The first one waits for you.
          </p>
          {scenes.map((s) => (
            <div className="watch-item" key={s.data.id}>
              <div className="watch-copy">
                <h3>{s.title}</h3>
                <p>{s.text}</p>
                <a href={s.href}>{s.link}</a>
              </div>
              <Scene {...s.data} />
            </div>
          ))}
        </section>

        <section className="code" aria-labelledby="code-title">
          <h2 id="code-title">The same agent, in your language</h2>
          <p className="section-lede">One engine underneath, so the behaviour is identical in all three.</p>
          <TabsClient
            sync
            className="tabs glass"
            labels={LANG_ORDER.map((l) => LANGS[l].label)}
            panels={code.map((html, i) => (
              <div key={LANG_ORDER[i]} className="code-panel" dangerouslySetInnerHTML={{ __html: html }} />
            ))}
          />
        </section>

        <section className="lifecycle" aria-labelledby="life-title">
          <h2 id="life-title">What a run gives you</h2>
          <dl className="glass">
            {lifecycle.map((item) => (
              <div className="row" key={item.title}>
                <dt>
                  <span className={`state state-${item.state}`} aria-hidden="true" />
                  {item.title}
                </dt>
                <dd>
                  <p>{item.text}</p>
                  <a href={item.href}>{item.link}</a>
                </dd>
              </div>
            ))}
          </dl>
        </section>

        <section className="measured" aria-labelledby="measured-title">
          <h2 id="measured-title">Measured, with the method</h2>
          <p className="section-lede">
            What the framework itself adds, with trivial steps and an instantly answering model, on one laptop (Ryzen 5 4600H, Windows
            11). A real model call takes far longer; these numbers matter for wide or long workflows.
          </p>
          <div className="table-glass glass">
            <table>
              <thead>
                <tr>
                  <th scope="col">Per</th>
                  <th scope="col">AgentBuild (Rust engine)</th>
                  <th scope="col">LangGraph 1.2 (Python)</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <th scope="row">Workflow step</th>
                  <td>21.4 µs</td>
                  <td>244 µs</td>
                </tr>
                <tr>
                  <th scope="row">Parallel node</th>
                  <td>3.9 µs</td>
                  <td>334 µs</td>
                </tr>
              </tbody>
            </table>
          </div>
          <p className="fine">
            Also measured: the gateway adds 0.14 ms per request on localhost; a call from the Python SDK adds 0.2 ms.{" "}
            <a href="/docs/reference/benchmarks/">All numbers and how to reproduce them</a>.
          </p>
        </section>

        <section className="faq" aria-labelledby="faq-title">
          <h2 id="faq-title">Questions</h2>
          <div className="faq-list">
            {faq.map((item) => (
              <details className="glass" key={item.q}>
                <summary>{item.q}</summary>
                <p>{item.a}</p>
              </details>
            ))}
          </div>
        </section>

        <section className="closing">
          <div className="closing-card glass">
            <h2>Build your first agent</h2>
            <p>Ten minutes, in the language you already use.</p>
            <a className="button primary" href="/docs/quickstart/">
              Start the quickstart
            </a>
          </div>
        </section>
      </main>

      <SiteFooter />
    </div>
  );
}
