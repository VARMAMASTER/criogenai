"use client";
// The install command in the hero follows the language chosen in the code
// tabs (and on any docs page).
import { hasAttrs, useSyncedLang } from "../useSyncedLang";

export default function InstallCommand({ items }: { items: { label: string; command: string }[] }) {
  const labels = items.map((i) => i.label);
  const { active, hydrated } = useSyncedLang(labels, true);
  return (
    <div className="install ab-synced" role="group" aria-label="Install" {...hasAttrs(labels)} data-hydrated={hydrated ? "" : undefined}>
      {items.map((item, i) => (
        <code
          key={item.label}
          className="glass"
          data-tab-panel=""
          data-tab-lang={item.label.toLowerCase()}
          data-active={i === active ? "" : undefined}
        >
          {item.command}
        </code>
      ))}
    </div>
  );
}
