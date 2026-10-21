"use client";
// The language a visitor picked, shared by every synced tab group on every
// page (localStorage), as an index into this group's labels.
import { useCallback, useLayoutEffect, useState } from "react";
import { onLang, setLang, storedLang } from "@/lib/prefs";

export function useSyncedLang(labels: string[], sync: boolean) {
  const [active, setActive] = useState(0);
  const [hydrated, setHydrated] = useState(false);
  const key = labels.join("|");
  useLayoutEffect(() => {
    setHydrated(true);
    if (!sync) return;
    const list = key.split("|");
    const stored = storedLang();
    const i = stored ? list.indexOf(stored) : -1;
    if (i >= 0) setActive(i);
    return onLang((label) => {
      const j = list.indexOf(label);
      if (j >= 0) setActive(j);
    });
  }, [key, sync]);
  const choose = useCallback(
    (i: number) => {
      setActive(i);
      if (sync) setLang(labels[i]);
    },
    [labels, sync],
  );
  return { active, choose, hydrated };
}

export function hasAttrs(labels: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const l of labels) out[`data-has-${l.toLowerCase()}`] = "";
  return out;
}
