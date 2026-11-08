import Link from "next/link";
import type { Metadata } from "next";
import { pageMeta } from "@/lib/seo";
import Diagram from "@/components/playground/Diagram";
import { GROUPS, PATTERNS } from "@/lib/patterns";

export const metadata: Metadata = pageMeta({
  title: "Agent pattern playground: ReAct, Reflexion, ReWOO, supervisor, swarm and more",
  description:
    "Every common AI agent pattern, runnable in your browser: ReAct, Chain and Tree of Thoughts, Plan-and-Solve, ReWOO, Reflexion, router, supervisor, hierarchical teams, collaboration, debate, swarms, human in the loop, and IBM's five agent types. Watch each one step by step, read the Criogen code, or try it with your own key.",
  path: "/playground/",
});

export default function PlaygroundIndex() {
  return (
    <>
      <header className="sb-hero">
        <p className="lab-eyebrow">Playground</p>
        <h1>The simplest, fastest way to build AI agents.</h1>
        <p className="lab-lede">
          Every common agent pattern, one page each. Watch it run step by step, see the Criogen code that builds it, and change the task.
          No sign-up and no key needed. Add your own key if you want a real model to make the decisions; it stays in your browser.
        </p>
        <p className="sb-small">
          “Fastest” is measured: Criogen against LangGraph and CrewAI on the same work, in the <Link href="/benchmarks/">benchmark lab</Link>.
        </p>
      </header>
      {GROUPS.map((g) => (
        <section key={g.name} className="sb-group" aria-labelledby={`g-${g.name}`}>
          <h2 id={`g-${g.name}`}>{g.name}</h2>
          <p className="lab-muted">
            {g.about}
            {g.name === "IBM agent types" && (
              <>
                {" "}
                See <a href="https://www.ibm.com/think/topics/ai-agent-types">IBM: types of AI agents</a>.
              </>
            )}
          </p>
          <ul className="sb-grid">
            {PATTERNS.filter((p) => p.group === g.name).map((p) => (
              <li key={p.id}>
                <Link href={`/playground/${p.id}/`} className="sb-tile">
                  <span className="sb-tile-graph" aria-hidden="true">
                    <Diagram graph={p.graph} nodes={{}} edges={{}} title={p.title} uid={`t-${p.id}`} />
                  </span>
                  <b>{p.title}</b>
                  <span>{p.short}</span>
                  <em>{p.usesModel ? (p.criogen.kind === "helper" ? "ready-made in Criogen" : "built from Criogen primitives") : "no model needed"}</em>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      ))}
      <section className="sb-card sb-faq" aria-labelledby="sb-safe">
        <h2 id="sb-safe">About your key</h2>
        <ul>
          <li>It is optional. Without one, every pattern runs with scripted model replies.</li>
          <li>It is kept in this tab&apos;s memory only. It is never saved and never sent to this site; reload the page and it is gone. “Forget key” drops it at once.</li>
          <li>Your browser sends requests straight to the provider you chose (OpenAI, Anthropic, Google or OpenRouter). Each run makes at most 16 short calls.</li>
          <li>Tools are always simulated: nothing is booked, sent, refunded or deleted.</li>
        </ul>
      </section>
    </>
  );
}
