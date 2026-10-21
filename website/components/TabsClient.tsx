"use client";
// Tabs with the ARIA tabs pattern (arrow keys, Home, End). With sync, the
// choice is shared by every group with the same labels, on every page.
import { useId, useRef, type KeyboardEvent, type ReactNode } from "react";
import { hasAttrs, useSyncedLang } from "./useSyncedLang";

export default function TabsClient({
  labels,
  panels,
  sync = false,
  className = "ab-tabs",
  ariaLabel = "Language",
}: {
  labels: string[];
  panels: ReactNode[];
  sync?: boolean;
  className?: string;
  ariaLabel?: string;
}) {
  const { active, choose, hydrated } = useSyncedLang(labels, sync);
  const id = useId();
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent, i: number) => {
    const n = labels.length;
    let next = -1;
    if (e.key === "ArrowRight") next = (i + 1) % n;
    else if (e.key === "ArrowLeft") next = (i - 1 + n) % n;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    if (next < 0) return;
    e.preventDefault();
    choose(next);
    buttons.current[next]?.focus();
  };
  const slug = (l: string) => l.toLowerCase();
  return (
    <div className={`${className}${sync ? " ab-synced" : ""}`} {...(sync ? hasAttrs(labels) : {})} data-hydrated={hydrated ? "" : undefined}>
      <div className="ab-tablist" role="tablist" aria-label={ariaLabel}>
        {labels.map((label, i) => (
          <button
            key={label}
            ref={(el) => {
              buttons.current[i] = el;
            }}
            type="button"
            role="tab"
            id={`${id}-tab-${i}`}
            aria-controls={`${id}-panel-${i}`}
            aria-selected={i === active}
            tabIndex={i === active ? 0 : -1}
            data-tab-lang={slug(label)}
            onClick={() => choose(i)}
            onKeyDown={(e) => onKey(e, i)}
          >
            {label}
          </button>
        ))}
      </div>
      {panels.map((panel, i) => (
        <div
          key={labels[i]}
          role="tabpanel"
          id={`${id}-panel-${i}`}
          aria-labelledby={`${id}-tab-${i}`}
          data-tab-panel=""
          data-tab-lang={slug(labels[i])}
          data-active={i === active ? "" : undefined}
        >
          {panel}
        </div>
      ))}
    </div>
  );
}
