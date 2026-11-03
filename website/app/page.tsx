import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/landing.css";
import "@/styles/trace.css";
import "@/styles/scene.css";
import BenchBars from "@/components/landing/BenchBars";
import Reveal from "@/components/motion/Reveal";
import InstallCommand from "@/components/landing/InstallCommand";
import InstallCounter from "@/components/landing/InstallCounter";
import ModelMarquee from "@/components/landing/ModelMarquee";
import Providers from "@/components/landing/Providers";
import RunTrace from "@/components/landing/RunTrace";
import Scene from "@/components/landing/Scene";
import { crash, gateway, workflow } from "@/components/landing/scenes";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import TabsClient from "@/components/TabsClient";
import { highlight } from "@/lib/highlight";
import { installs } from "@/lib/installs";
import { facts, faqJsonLd, jsonLd, ms, pageMeta } from "@/lib/seo";
import { LANG_ORDER, LANGS, snippet, type Lang } from "@/lib/snippets";

const f = facts();

export const metadata: Metadata = pageMeta({
  title: "Criogen: the fast AI agent framework and LangChain alternative (Rust engine, Python, TypeScript)",
  absolute: true,
  description: `AI agents and workflows on a Rust engine, from Python or TypeScript: tools, human approval, durable threads, memory, an audit trail and an LLM gateway. ${f.vsLangGraph} less framework overhead per agent turn than LangGraph, ${f.vsCrewAI} less than CrewAI (benchmark of ${f.date}).`,
  path: "/",
});

// Python and TypeScript are published first; the Rust crates come when
// the code opens.
const install: Partial<Record<Lang, string>> = {
  python: "pip install criogen",
  typescript: "npm install criogen",
};

// The lifecycle of a run, in the order it happens; each row links to its guide.
const lifecycle = [
  {
    state: "running",
    title: "Tools are plain functions",
    text: "Python functions, TypeScript functions or Rust closures become tools. Their arguments are checked against a schema before your code runs, so a model's mistake never reaches it.",
    href: "/docs/guides/agents/",
    link: "Agents and tools",
  },
  {
    state: "running",
    title: "Parallel steps that commit atomically",
    text: "Tool calls of one turn run at the same time. A step commits completely or not at all, and limits stop a run that loops.",
    href: "/docs/concepts/runs/",
    link: "How a run works",
  },
  {
    state: "waiting",
    title: "Approval gates that outlive the process",
    text: "Mark the risky tools. The run stops before they run and waits in the database until someone approves, rejects or edits the call, from any process.",
    href: "/docs/guides/approvals/",
    link: "Human approval",
  },
  {
    state: "done",
    title: "Crash recovery without repeated work",
    text: "Conversations are saved after every step and every tool call, in SQLite or Postgres. After a crash, retry continues where the run stopped, without running finished tools again.",
    href: "/docs/guides/threads/",
    link: "Conversations",
  },
  {
    state: "done",
    title: "An audit trail you can verify",
    text: "An audit trail records runs, model calls, tool calls and decisions with the person who made them, and no message content. Data can be encrypted at rest and deleted on a schedule.",
    href: "/docs/guides/compliance/",
    link: "Encryption, audit, retention",
  },
  {
    state: "running",
    title: "Any model, including your own",
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

const faq = [
  {
    q: "Is Criogen the fastest AI agent framework?",
    a: `It has the lowest framework overhead of the frameworks we measured. On one machine (benchmark of ${f.date}), an agent turn costs ${ms(f.turn.criogen)} in Criogen, ${ms(f.turn.langgraph)} in LangGraph and ${ms(f.turn.crewai)} in CrewAI, and 1,000 agent runs at once take ${ms(f.crowd.criogen)} against ${ms(f.crowd.langgraph)} and ${ms(f.crowd.crewai)}. With one agent and a real model, the model's own time dominates and all three are close; the gap matters for many steps, many tools and many users at once. The benchmarks page has every number and the kit to rerun it.`,
  },
  {
    q: "Is Criogen a LangChain or LangGraph alternative?",
    a: `Yes. It covers what most teams use LangChain and LangGraph for: agents with tool calling, workflow graphs with branches, joins and loops, human approval, saved conversations and memory, from Python or TypeScript. It installs as one package (${f.footprint.criogen.packages} against ${f.footprint.langgraph.packages} for LangGraph) and passed ${f.reliability.criogen} of ${f.reliability.tests} reliability tests by default, against ${f.reliability.langgraph} for LangGraph. It does not have LangChain's catalogue of document loaders and integrations.`,
  },
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
    a: "Criogen's engine is written in Rust and shared by its Python, TypeScript and Rust APIs, so a run behaves the same in all three. Its storage includes encryption at rest and a hash-chained audit trail. The measured cost per step, compared with LangGraph on the same workflows, is on the benchmarks page.",
  },
  {
    q: "What does it cost?",
    a: "Nothing: Criogen is free to use, also commercially. You pay your model provider, if you use one. The source code opens (MIT or Apache-2.0) once Criogen reaches 10,000 installs.",
  },
];

export default async function Home() {
  const code = await Promise.all(LANG_ORDER.map((lang) => highlight(snippet(lang, "quickstart"), LANGS[lang].ext)));
  return (
    <div className="site landing">
      <a className="skip" href="#main">
        Skip to content
      </a>
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: jsonLd() }} />
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: faqJsonLd(faq) }} />
      <SiteHeader />

      <main id="main">
        <section className="hero">
          <div className="hero-copy">
            <Reveal y={24}>
              <h1>
                <span className="hero-eyebrow">The fast AI agent framework · a LangChain alternative</span>{" "}
                Run AI agents like production software.
              </h1>
            </Reveal>
            <Reveal delay={0.1}>
              <p className="lede">
                A Rust engine with Python and TypeScript SDKs: {f.vsLangGraph} less framework overhead per agent turn than LangGraph in{" "}
                <a href="/benchmarks/">our benchmark</a>. Approval gates, durable conversations, encryption at rest and an audit
                trail are part of the runtime, so your code is only the tools and the logic.
              </p>
            </Reveal>
            <Reveal delay={0.2}>
              <InstallCommand items={LANG_ORDER.filter((l) => install[l]).map((l) => ({ label: LANGS[l].label, command: install[l] as string }))} />
            </Reveal>
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

        <ModelMarquee />

        <section className="watch" aria-labelledby="watch-title">
          <h2 id="watch-title">See a run, step by step</h2>
          <p className="section-lede">
            Each state has one colour, here and across the site: running, waiting for a person, done, failed. The first run waits for your decision.
          </p>
          {scenes.map((s) => (
            <Reveal className="watch-item" key={s.data.id}>
              <div className="watch-copy">
                <h3>{s.title}</h3>
                <p>{s.text}</p>
                <a href={s.href}>{s.link}</a>
              </div>
              <Scene {...s.data} />
            </Reveal>
          ))}
        </section>

        <section className="code" aria-labelledby="code-title">
          <h2 id="code-title">One engine, three languages</h2>
          <p className="section-lede">Scheduling, saving and limits live in the Rust engine. The SDKs only describe agents, so a run behaves the same in each.</p>
          <TabsClient
            sync
            className="tabs glass"
            labels={LANG_ORDER.map((l) => LANGS[l].label)}
            panels={code.map((html, i) => (
              <div key={LANG_ORDER[i]} className="code-panel" dangerouslySetInnerHTML={{ __html: html }} />
            ))}
          />
        </section>

        <Providers />

        <section className="lifecycle" aria-labelledby="life-title">
          <h2 id="life-title">What the runtime handles for you</h2>
          <dl className="glass">
            {lifecycle.map((item, i) => (
              <Reveal className="row" key={item.title} delay={Math.min(i * 0.06, 0.3)} y={12}>
                <dt>
                  <span className={`state state-${item.state}`} aria-hidden="true" />
                  {item.title}
                </dt>
                <dd>
                  <p>{item.text}</p>
                  <a href={item.href}>{item.link}</a>
                </dd>
              </Reveal>
            ))}
          </dl>
        </section>

        <section className="measured" aria-labelledby="measured-title">
          <h2 id="measured-title">Measured, with the method published</h2>
          <p className="section-lede">
            What the framework itself adds, with trivial steps and an instantly answering model, on one laptop (Ryzen 5 4600H, Windows
            11). A real model call takes far longer; these numbers matter for wide or long workflows.
          </p>
          <BenchBars
            rows={[
              { label: "Workflow step", ours: 21.4, theirs: 244, unit: "µs" },
              { label: "Parallel node", ours: 3.9, theirs: 334, unit: "µs" },
            ]}
          />
          <div className="table-glass glass">
            <table>
              <thead>
                <tr>
                  <th scope="col">Per</th>
                  <th scope="col">Criogen (Rust engine)</th>
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
          <h2 id="faq-title">Frequently asked</h2>
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
            <h2>Ship a working agent today</h2>
            <p>The quickstart takes about ten minutes, in Python or TypeScript.</p>
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
