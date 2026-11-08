"use client";
// Light catching the glass: a soft highlight follows the pointer over the
// home page's panels (styles/landing.css, `--mx` / `--my`). One passive
// listener, at most one update per frame; nothing runs for touch, keyboards
// or reduced motion.
import { useEffect } from "react";

const PANELS = ".glass, .scene, .trace";

export default function GlassLight() {
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let frame = 0;
    let last: { x: number; y: number; target: EventTarget | null } | null = null;
    const update = () => {
      frame = 0;
      if (!last) return;
      const panel = (last.target as Element | null)?.closest?.<HTMLElement>(PANELS);
      if (!panel) return;
      const box = panel.getBoundingClientRect();
      panel.style.setProperty("--mx", `${last.x - box.left}px`);
      panel.style.setProperty("--my", `${last.y - box.top}px`);
    };
    const move = (e: PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      last = { x: e.clientX, y: e.clientY, target: e.target };
      if (!frame) frame = requestAnimationFrame(update);
    };
    document.addEventListener("pointermove", move, { passive: true });
    return () => {
      document.removeEventListener("pointermove", move);
      if (frame) cancelAnimationFrame(frame);
    };
  }, []);
  return null;
}
