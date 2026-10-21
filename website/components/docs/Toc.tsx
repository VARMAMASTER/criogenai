"use client";
// "On this page": a column on wide screens, a dropdown on narrow ones. The
// heading being read is marked as the page scrolls.
import { useEffect, useRef, useState } from "react";
import type { TocEntry } from "@/lib/docs";
import { Chevron } from "../icons";

const OVERVIEW: TocEntry = { depth: 2, id: "_top", text: "Overview" };

function useCurrent(entries: TocEntry[]): string {
  const [current, setCurrent] = useState("_top");
  useEffect(() => {
    let frame = 0;
    const update = () => {
      frame = 0;
      const line = 110; // below the sticky header
      let id = "_top";
      for (const e of entries) {
        const el = document.getElementById(e.id);
        if (el && el.getBoundingClientRect().top <= line) id = e.id;
      }
      setCurrent(id);
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    update();
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
      cancelAnimationFrame(frame);
    };
  }, [entries]);
  return current;
}

function List({ entries, current, onPick }: { entries: TocEntry[]; current: string; onPick?: () => void }) {
  return (
    <ul>
      {[OVERVIEW, ...entries].map((e) => (
        <li key={e.id} className={e.depth === 3 ? "toc-sub" : undefined}>
          <a href={`#${e.id}`} aria-current={e.id === current ? "true" : undefined} onClick={onPick}>
            {e.text}
          </a>
        </li>
      ))}
    </ul>
  );
}

export function TocSide({ entries }: { entries: TocEntry[] }) {
  const current = useCurrent(entries);
  if (!entries.length) return null;
  return (
    <nav className="toc" aria-labelledby="toc-title">
      <h2 id="toc-title">On this page</h2>
      <List entries={entries} current={current} />
    </nav>
  );
}

export function TocMobile({ entries }: { entries: TocEntry[] }) {
  const current = useCurrent(entries);
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        box.current?.querySelector("button")?.focus();
      }
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  if (!entries.length) return null;
  const label = [OVERVIEW, ...entries].find((e) => e.id === current)?.text ?? "Overview";
  return (
    <div className="toc-mobile" ref={box} data-pagefind-ignore="">
      <button type="button" aria-expanded={open} aria-controls="toc-mobile-list" onClick={() => setOpen((o) => !o)}>
        <span className="toc-mobile-label">On this page</span>
        <Chevron className={open ? "toc-chev open" : "toc-chev"} />
        <span className="toc-mobile-current">{label}</span>
      </button>
      <nav id="toc-mobile-list" className="toc toc-mobile-list" aria-label="On this page" hidden={!open}>
        <List entries={entries} current={current} onPick={() => setOpen(false)} />
      </nav>
    </div>
  );
}
