"use client";
// Switches between the demo's tabs. The tours are rendered on the server; this
// only chooses which one shows (and keeps the choice in the URL hash).
import { useEffect, useState, type ReactNode } from "react";

export interface DemoTab {
  id: string;
  label: string;
  node: ReactNode;
}

export default function DemoTabs({ tabs, initial }: { tabs: DemoTab[]; initial: string }) {
  const [tab, setTab] = useState(initial);
  useEffect(() => {
    const hash = window.location.hash.slice(1);
    if (tabs.some((t) => t.id === hash)) setTab(hash);
  }, [tabs]);
  const pick = (id: string) => {
    setTab(id);
    history.replaceState(null, "", `#${id}`);
  };
  return (
    <div>
      <div className="pg-modes" role="tablist" aria-label="Demo">
        {tabs.map((t) => (
          <button key={t.id} type="button" role="tab" id={`tab-${t.id}`} aria-controls={`panel-${t.id}`} aria-selected={tab === t.id} aria-pressed={tab === t.id} className="lab-button" onClick={() => pick(t.id)}>
            {t.label}
          </button>
        ))}
      </div>
      {tabs.map((t) => (
        <div key={t.id} role="tabpanel" id={`panel-${t.id}`} aria-labelledby={`tab-${t.id}`} hidden={tab !== t.id}>
          {t.node}
        </div>
      ))}
    </div>
  );
}
