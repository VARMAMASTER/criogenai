// A docs page: header, sidebar, the page on one glass sheet, the table of
// contents, previous / next and the footer's link columns.
import type { ReactNode } from "react";
import type { TocEntry } from "@/lib/docs";
import { ALL_PAGES, FOOTER_COLUMNS, hrefOf } from "@/lib/nav";
import DocsHeader from "./DocsHeader";
import SidebarNav from "./SidebarNav";
import { TocMobile, TocSide } from "./Toc";

export default function DocsShell({
  slug,
  title,
  toc,
  children,
}: {
  slug: string;
  title: string;
  toc: TocEntry[];
  children: ReactNode;
}) {
  const index = ALL_PAGES.findIndex((p) => p.slug === slug);
  const prev = index > 0 ? ALL_PAGES[index - 1] : null;
  const next = index >= 0 && index < ALL_PAGES.length - 1 ? ALL_PAGES[index + 1] : null;

  return (
    <div className="docs">
      <a className="skip" href="#_top">
        Skip to content
      </a>
      <DocsHeader slug={slug} />
      <div className="d-body">
        <aside className="d-sidebar" aria-label="Docs navigation">
          <div className="d-sidebar-scroll">
            <SidebarNav current={slug} idPrefix="side" />
          </div>
        </aside>
        <div className="d-main">
          <main className="d-sheet">
            <TocMobile entries={toc} />
            <div data-pagefind-body="">
              <div className="d-title">
                <h1 id="_top" tabIndex={-1}>
                  {title}
                </h1>
              </div>
              <div className="d-content prose">{children}</div>
            </div>
            <footer className="d-footer" data-pagefind-ignore="">
              <nav className="pager" aria-label="Previous and next pages">
                {prev ? (
                  <a href={hrefOf(prev.slug)} rel="prev">
                    <span className="pager-hint">Previous</span>
                    <span className="pager-title">{prev.label}</span>
                  </a>
                ) : (
                  <span />
                )}
                {next ? (
                  <a href={hrefOf(next.slug)} rel="next" className="pager-next">
                    <span className="pager-hint">Next</span>
                    <span className="pager-title">{next.label}</span>
                  </a>
                ) : (
                  <span />
                )}
              </nav>
              <div className="d-columns">
                {FOOTER_COLUMNS.map((c) => (
                  <div key={c.title}>
                    <p className="d-col-title">{c.title}</p>
                    <ul>
                      {c.links.map((l) => (
                        <li key={l.href}>
                          <a href={l.href}>{l.label}</a>
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
              </div>
              <p className="d-license">AgentBuild is free to use. Its code opens at 10,000 installs.</p>
            </footer>
          </main>
          <aside className="d-toc" aria-label="Table of contents">
            <div className="d-toc-inner">
              <TocSide entries={toc} />
            </div>
          </aside>
        </div>
      </div>
    </div>
  );
}
