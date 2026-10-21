// Callouts (Starlight's Aside): note, tip, caution, danger.
import type { ReactNode } from "react";
import { Bulb, Info, Warn } from "./icons";

const DEFAULT_TITLE = { note: "Note", tip: "Tip", caution: "Caution", danger: "Danger" };

export default function Aside({
  type = "note",
  title,
  children,
}: {
  type?: "note" | "tip" | "caution" | "danger";
  title?: string;
  children?: ReactNode;
}) {
  const Icon = type === "tip" ? Bulb : type === "note" ? Info : Warn;
  return (
    <aside className={`callout callout-${type}`} aria-label={title ?? DEFAULT_TITLE[type]}>
      <p className="callout-title">
        <Icon />
        {title ?? DEFAULT_TITLE[type]}
      </p>
      <div className="callout-body">{children}</div>
    </aside>
  );
}
