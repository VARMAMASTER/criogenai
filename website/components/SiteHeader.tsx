// The landing page's and the lab's header: a floating glass pill.
import Link from "next/link";
import CopyButton from "./CopyButton";
import LogoMark from "./LogoMark";
import ThemeToggle from "./ThemeToggle";

export default function SiteHeader({ current }: { current?: "benchmarks" | "playground" | "demo" | "blog" }) {
  return (
    <header className="top glass">
      <Link className="brand" href="/">
        <LogoMark size={28} />
        <span>criogen</span>
      </Link>
      <div className="top-right">
        <nav aria-label="Main">
          <Link href="/docs/">Docs</Link>
          <Link href="/docs/reference/api/" className="top-api">
            API
          </Link>
          <Link href="/benchmarks/" aria-current={current === "benchmarks" ? "page" : undefined}>
            Benchmarks
          </Link>
          <Link href="/playground/" aria-current={current === "playground" ? "page" : undefined}>
            Playground
          </Link>
          <Link href="/demo/" aria-current={current === "demo" ? "page" : undefined}>
            Demo
          </Link>
          <Link href="/blog/" aria-current={current === "blog" ? "page" : undefined}>
            Blog
          </Link>
        </nav>
        <ThemeToggle compact />
      </div>
    </header>
  );
}

const footerColumns = [
  {
    title: "Get started",
    links: [
      { label: "What Criogen is", href: "/docs/" },
      { label: "Install", href: "/docs/install/" },
      { label: "Quickstart", href: "/docs/quickstart/" },
      { label: "Playground", href: "/playground/" },
      { label: "Demo", href: "/demo/" },
      { label: "Benchmarks", href: "/benchmarks/" },
      { label: "Blog", href: "/blog/" },
    ],
  },
  {
    title: "Guides",
    links: [
      { label: "Agents and tools", href: "/docs/guides/agents/" },
      { label: "Human approval", href: "/docs/guides/approvals/" },
      { label: "MCP servers' tools", href: "/docs/guides/mcp/" },
      { label: "Collections and retrieval", href: "/docs/guides/retrieval/" },
      { label: "Evals and quality gates", href: "/docs/guides/evals/" },
      { label: "Tracing", href: "/docs/guides/telemetry/" },
    ],
  },
  {
    title: "Reference",
    links: [
      { label: "API reference", href: "/docs/reference/api/" },
      { label: "Error codes", href: "/docs/reference/errors/" },
      { label: "Status: built and planned", href: "/docs/status/" },
      { label: "llms.txt", href: "/llms.txt" },
    ],
  },
];

// Every colour on the site is a run state; the footer says which.
const states = [
  { state: "running", label: "running" },
  { state: "waiting", label: "waiting for a person" },
  { state: "done", label: "done" },
  { state: "failed", label: "failed" },
];

const installs = ["pip install criogen", "npm install criogen"];

export function SiteFooter() {
  return (
    <footer className="bottom glass">
      <div className="bottom-main">
        <div className="bottom-brand">
          <Link className="brand" href="/">
            <LogoMark size={26} />
            <span>criogen</span>
          </Link>
          <p>AI agents and workflows on a Rust engine, from Python or TypeScript.</p>
          <ul className="bottom-install" aria-label="Install">
            {installs.map((command) => (
              <li key={command}>
                <code>{command}</code>
                <CopyButton code={command} />
              </li>
            ))}
          </ul>
          <ul className="bottom-states" aria-label="What the colours on this site mean">
            {states.map((s) => (
              <li key={s.state}>
                <span className={`state state-${s.state}`} aria-hidden="true" />
                {s.label}
              </li>
            ))}
          </ul>
        </div>
        {footerColumns.map((column) => (
          <nav className="bottom-column" aria-label={column.title} key={column.title}>
            <h2>{column.title}</h2>
            <ul>
              {column.links.map((link) => (
                <li key={link.href}>
                  {link.href.endsWith(".txt") ? <a href={link.href}>{link.label}</a> : <Link href={link.href}>{link.label}</Link>}
                </li>
              ))}
            </ul>
          </nav>
        ))}
      </div>
      <div className="bottom-fine">
        <p>Free to use, also commercially. The source code opens under MIT or Apache-2.0 at 10,000 installs.</p>
        <nav aria-label="Packages">
          <a href="https://www.npmjs.com/package/criogen" rel="noopener">
            npm
          </a>
          <a href="https://pypi.org/project/criogen/" rel="noopener">
            PyPI
          </a>
        </nav>
      </div>
    </footer>
  );
}
