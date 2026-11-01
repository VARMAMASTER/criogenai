"use client";
// Site search over a static index (Pagefind, built from out/ after
// `next build`), so it works offline once the site is built. Opens with the
// button, Ctrl+K / Cmd+K or "/". A native modal dialog keeps focus inside and
// closes on Escape.
import { useCallback, useEffect, useRef, useState } from "react";
import Link from "next/link";
import { SearchIcon } from "./icons";

interface Hit {
  url: string;
  title: string;
  excerpt: string;
}
interface PagefindResult {
  data: () => Promise<{ url: string; meta: { title?: string }; excerpt: string }>;
}
interface Pagefind {
  debouncedSearch: (q: string) => Promise<{ results: PagefindResult[] } | null>;
  init?: () => Promise<void>;
}

let pagefind: Promise<Pagefind | null> | null = null;
function loadPagefind(): Promise<Pagefind | null> {
  // A runtime import the bundler must not touch: the file exists only in out/.
  const load = new Function("u", "return import(u)") as (u: string) => Promise<Pagefind>;
  pagefind ??= load("/pagefind/pagefind.js")
    .then(async (p) => {
      await p.init?.();
      return p;
    })
    .catch(() => null);
  return pagefind;
}

export default function Search({ compact = false }: { compact?: boolean }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Hit[]>([]);
  const [state, setState] = useState<"idle" | "loading" | "missing" | "done">("idle");

  const open = useCallback(() => {
    dialog.current?.showModal();
    input.current?.focus();
    void loadPagefind().then((p) => setState((s) => (p ? s : "missing")));
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = (e.target as HTMLElement | null)?.closest("input, textarea, [contenteditable]");
      if ((e.key === "k" && (e.ctrlKey || e.metaKey)) || (e.key === "/" && !typing)) {
        if (dialog.current?.open) return;
        e.preventDefault();
        open();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  useEffect(() => {
    let live = true;
    const q = query.trim();
    if (!q) {
      setHits([]);
      setState((s) => (s === "missing" ? s : "idle"));
      return;
    }
    setState((s) => (s === "missing" ? s : "loading"));
    void loadPagefind().then(async (p) => {
      if (!p) return live && setState("missing");
      const found = await p.debouncedSearch(q);
      if (!found || !live) return;
      const data = await Promise.all(found.results.slice(0, 8).map((r) => r.data()));
      if (!live) return;
      setHits(data.map((d) => ({ url: d.url, title: d.meta.title ?? d.url, excerpt: d.excerpt })));
      setState("done");
    });
    return () => {
      live = false;
    };
  }, [query]);

  return (
    <>
      <button type="button" className={compact ? "icon-button search-open" : "search-open search-pill"} onClick={open} aria-label="Search the docs" aria-keyshortcuts="Control+K Meta+K /">
        <SearchIcon />
        {!compact && (
          <>
            <span>Search</span>
            <kbd>Ctrl K</kbd>
          </>
        )}
      </button>
      <dialog
        ref={dialog}
        className="search-dialog"
        aria-label="Search"
        onClick={(e) => {
          if (e.target === dialog.current) dialog.current?.close();
        }}
      >
        <div className="search-panel">
          <div className="search-row">
            <SearchIcon />
            <input
              ref={input}
              type="search"
              placeholder="Search the docs"
              aria-label="Search the docs"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                // A search field would only clear itself on Escape: close instead.
                if (e.key === "Escape") {
                  e.preventDefault();
                  dialog.current?.close();
                }
              }}
              autoComplete="off"
              spellCheck={false}
            />
            <button type="button" className="search-close" onClick={() => dialog.current?.close()}>
              Close
            </button>
          </div>
          <div className="search-results" aria-live="polite">
            {state === "missing" && (
              <p className="search-note">The search index is built with the site (npm run build). It is not available in development.</p>
            )}
            {state === "done" && hits.length === 0 && <p className="search-note">No results for “{query.trim()}”.</p>}
            {hits.length > 0 && (
              <ul>
                {hits.map((h) => (
                  <li key={h.url}>
                    <Link href={h.url} onClick={() => dialog.current?.close()}>
                      <span className="search-hit-title">{h.title}</span>
                      {/* Excerpts come from our own pages, with <mark> around the match. */}
                      <span className="search-hit-excerpt" dangerouslySetInnerHTML={{ __html: h.excerpt }} />
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      </dialog>
    </>
  );
}
