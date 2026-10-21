"use client";
import { useState } from "react";
import { Check, Copy } from "./icons";

export default function CopyButton({ code }: { code: string }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setDone(true);
      window.setTimeout(() => setDone(false), 1600);
    } catch {
      /* clipboard blocked: nothing to do */
    }
  };
  return (
    <button type="button" className="copy" onClick={copy} aria-label={done ? "Copied" : "Copy code"} title={done ? "Copied" : "Copy"}>
      {done ? <Check /> : <Copy />}
      <span className="sr-only" aria-live="polite">
        {done ? "Copied" : ""}
      </span>
    </button>
  );
}
