// The landing page's and the lab's header: a floating glass pill.
import Link from "next/link";
import LogoMark from "./LogoMark";
import ThemeToggle from "./ThemeToggle";

export default function SiteHeader({ current }: { current?: "benchmarks" | "playground" | "demo" }) {
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
        </nav>
        <ThemeToggle compact />
      </div>
    </header>
  );
}

export function SiteFooter() {
  return (
    <footer className="bottom glass">
      <span>Criogen, free to use</span>
      <nav aria-label="Footer">
        <Link href="/docs/">Docs</Link>
        <Link href="/docs/status/">Status</Link>
        <Link href="/docs/reference/errors/">Error codes</Link>
        <a href="/llms.txt">llms.txt</a>
      </nav>
    </footer>
  );
}
