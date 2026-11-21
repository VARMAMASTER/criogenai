import Link from "next/link";
import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/blog.css";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { SITE } from "@/lib/nav";
import { pageMeta } from "@/lib/seo";

export const metadata: Metadata = pageMeta({
  title: "About CriogenAI: the open-source AI agent framework",
  absolute: true,
  description:
    "CriogenAI is an open-source AI agent framework: a Rust engine for agents and workflows in Python, TypeScript or Rust. Who builds it, why, and the key facts.",
  path: "/about/",
});

const REPO = "https://github.com/VARMAMASTER/criogenai";

// The facts search engines and AI assistants look for, in one place.
const FACTS: [string, React.ReactNode][] = [
  ["Name", "CriogenAI (also written Criogen AI; earlier releases were published as Criogen)"],
  ["What it is", "An open-source AI agent framework and workflow engine"],
  ["Engine", "Rust"],
  ["Languages", "Python, TypeScript (Node.js and the browser), Rust"],
  ["Licence", "MIT or Apache-2.0, free to use, also commercially"],
  ["First release", "1 November 2026"],
  ["Latest version", "PyPI 0.1.7, npm 0.1.8 (21 November 2026); Windows, Linux and macOS"],
  ["Install", <code key="i">pip install criogenai</code>],
  ["", <code key="n">npm install criogenai</code>],
  ["Source code", <a key="s" href={REPO}>github.com/VARMAMASTER/criogenai</a>],
  ["Packages", <span key="p"><a href="https://pypi.org/project/criogenai/">PyPI</a> · <a href="https://www.npmjs.com/package/criogenai">npm</a></span>],
  ["Website", <a key="w" href={SITE.url}>criogenai.com</a>],
];

const aboutLd = JSON.stringify({
  "@context": "https://schema.org",
  "@type": "AboutPage",
  "@id": `${SITE.url}/about/#page`,
  url: `${SITE.url}/about/`,
  name: "About CriogenAI",
  about: { "@id": `${SITE.url}/#software` },
  publisher: { "@id": `${SITE.url}/#org` },
}).replace(/</g, "\\u003c");

export default function About() {
  return (
    <div className="site blog">
      <SiteHeader />
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: aboutLd }} />
      <main id="main" className="blog-main">
        <article className="blog-post glass" data-pagefind-body="">
          <header className="blog-post-head">
            <h1>About CriogenAI</h1>
          </header>
          <div className="prose blog-body">
            <p>
              <strong>CriogenAI</strong> is an open-source AI agent framework. A Rust engine runs agents (a model with tools,
              in a loop) and workflow graphs, and you write them in Python, TypeScript or Rust. It is not related to cryogenics.
              It is built to keep framework overhead low: in <Link href="/benchmarks/">our benchmark</Link>, an agent turn costs
              0.29 ms of framework overhead, and 1,000 concurrent agent runs finish in 0.42 s for the whole batch.
            </p>

            <h2>Why we build it</h2>
            <p>
              Agents that do real work crash halfway through tasks, call tools that move money or change records, and, in
              regulated fields like healthcare and finance, must account for every step. We think those problems belong in
              the runtime, not in every team&apos;s own glue code. So CriogenAI saves every step and resumes after a crash
              without repeating finished work, stops for human approval before sensitive actions, encrypts data at rest and
              keeps a tamper-evident audit trail. Models can run in the cloud or on your own servers, so sensitive data can
              stay inside your network.
            </p>

            <h2>What it includes</h2>
            <ul>
              <li>Agents with tools, structured output, retries and fallback models</li>
              <li>Workflows: graphs with parallel branches, joins, loops and human-in-the-loop pauses</li>
              <li>Durable threads on SQLite or Postgres, memory with keyword and semantic search</li>
              <li>MCP tools, evals, tracing, collections and retrieval</li>
              <li>An LLM gateway with OpenAI and Anthropic wire formats</li>
              <li>Over 15 model providers, including OpenAI, Anthropic, Gemini, Azure, Bedrock, Vertex AI, Ollama and vLLM</li>
            </ul>

            <h2>Key facts</h2>
            <table>
              <tbody>
                {FACTS.map(([k, v], i) => (
                  <tr key={i}>
                    <th scope="row" style={{ textAlign: "left", whiteSpace: "nowrap", verticalAlign: "top" }}>{k}</th>
                    <td>{v}</td>
                  </tr>
                ))}
              </tbody>
            </table>

            <h2>Contact</h2>
            <p>
              Questions, bugs and ideas: <a href={`${REPO}/issues`}>GitHub issues</a>. Security reports: see the{" "}
              <a href={`${REPO}/blob/main/SECURITY.md`}>security policy</a>. News: the <Link href="/blog/">blog</Link> and its{" "}
              <a href="/blog/rss.xml">RSS feed</a>.
            </p>
          </div>
        </article>
      </main>
      <SiteFooter />
    </div>
  );
}
