"use client";
// The pattern list, grouped, with the current one marked. On a phone it
// folds into a <details> menu above the story.
import Link from "next/link";
import { usePathname } from "next/navigation";
import { GROUPS, PATTERNS } from "@/lib/patterns";

export default function Sidebar() {
  const path = usePathname();
  const current = path.replace(/^\/playground\/?/, "").replace(/\/$/, "");
  const list = (
    <nav aria-label="Patterns" className="sb-nav">
      <Link href="/playground/" className={`sb-nav-all${current === "" ? " sb-nav-on" : ""}`} aria-current={current === "" ? "page" : undefined}>
        All patterns
      </Link>
      {GROUPS.map((g) => (
        <div key={g.name} className="sb-nav-group">
          <p>{g.name}</p>
          <ul>
            {PATTERNS.filter((p) => p.group === g.name).map((p) => (
              <li key={p.id}>
                <Link href={`/playground/${p.id}/`} className={current === p.id ? "sb-nav-on" : ""} aria-current={current === p.id ? "page" : undefined}>
                  {p.title}
                </Link>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
  return (
    <aside className="sb-side">
      <details className="sb-side-fold">
        <summary>{PATTERNS.find((p) => p.id === current)?.title ?? "All patterns"} · browse patterns</summary>
        {list}
      </details>
      <div className="sb-side-full">{list}</div>
    </aside>
  );
}
