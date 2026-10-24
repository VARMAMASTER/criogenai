// The landing page's and the lab's header: a floating glass pill.
import ThemeToggle from "./ThemeToggle";

export default function SiteHeader({ current }: { current?: "benchmarks" | "playground" | "demo" }) {
  return (
    <header className="top glass">
      <a className="brand" href="/">
        <img src="/favicon.svg" alt="" width="26" height="26" />
        <span>AgentBuild</span>
      </a>
      <div className="top-right">
        <nav aria-label="Main">
          <a href="/docs/">Docs</a>
          <a href="/docs/reference/api/" className="top-api">
            API
          </a>
          <a href="/benchmarks/" aria-current={current === "benchmarks" ? "page" : undefined}>
            Benchmarks
          </a>
          <a href="/playground/" aria-current={current === "playground" ? "page" : undefined}>
            Playground
          </a>
          <a href="/demo/" aria-current={current === "demo" ? "page" : undefined}>
            Demo
          </a>
        </nav>
        <ThemeToggle compact />
      </div>
    </header>
  );
}

export function SiteFooter() {
  return (
    <footer className="bottom glass">
      <span>AgentBuild, free to use</span>
      <nav aria-label="Footer">
        <a href="/docs/">Docs</a>
        <a href="/docs/status/">Status</a>
        <a href="/docs/reference/errors/">Error codes</a>
        <a href="/llms.txt">llms.txt</a>
      </nav>
    </footer>
  );
}
