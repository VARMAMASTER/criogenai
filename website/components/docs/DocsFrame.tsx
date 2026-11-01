"use client";
// The docs' fixed frame: header and sidebar. It lives in the docs layout, so
// moving between pages swaps only the page; the sidebar keeps its scroll
// position and its open groups, and nothing flashes.
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";
import DocsHeader from "./DocsHeader";
import SidebarNav from "./SidebarNav";

export default function DocsFrame({ children }: { children: ReactNode }) {
  const slug = (usePathname() ?? "").replace(/^\/docs\/?/, "").replace(/\/$/, "");
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
        <div className="d-main">{children}</div>
      </div>
    </div>
  );
}
