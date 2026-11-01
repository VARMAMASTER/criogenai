"use client";
// The docs sidebar: groups that fold, the current page highlighted.
import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { SIDEBAR, hrefOf } from "@/lib/nav";
import { Chevron } from "../icons";

export default function SidebarNav({ current, onNavigate, idPrefix }: { current: string; onNavigate?: () => void; idPrefix: string }) {
  const [closed, setClosed] = useState<Record<string, boolean>>({});
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    // Bring the current page into view in a long sidebar.
    const el = ref.current?.querySelector<HTMLElement>('[aria-current="page"]');
    const pane = ref.current?.closest<HTMLElement>(".d-sidebar-scroll");
    if (el && pane) {
      const item = el.getBoundingClientRect();
      const view = pane.getBoundingClientRect();
      if (item.bottom > view.bottom - 16) pane.scrollTop += item.bottom - view.bottom + view.height / 2;
    }
  }, [current]);
  return (
    <nav className="sidebar-nav" aria-label="Docs" ref={ref}>
      <ul className="sb-groups">
        {SIDEBAR.map((group, gi) => {
          const open = !closed[group.label];
          const listId = `${idPrefix}-group-${gi}`;
          return (
            <li key={group.label} className="sb-group">
              <button
                type="button"
                className="sb-group-title"
                aria-expanded={open}
                aria-controls={listId}
                onClick={() => setClosed((c) => ({ ...c, [group.label]: open }))}
              >
                <span>{group.label}</span>
                <Chevron className="sb-chevron" />
              </button>
              <ul id={listId} className="sb-items" hidden={!open}>
                {group.items.map((item) => (
                  <li key={item.slug}>
                    <Link href={hrefOf(item.slug)} aria-current={item.slug === current ? "page" : undefined} onClick={onNavigate}>
                      {item.label}
                    </Link>
                  </li>
                ))}
              </ul>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
