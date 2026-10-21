"use client";
// Light / dark, remembered in localStorage. The label shown follows the
// <html data-theme> the head script set, so it is right before hydration.
import { useEffect, useState } from "react";
import { currentTheme, setTheme, type Theme } from "@/lib/prefs";
import { Moon, Sun } from "./icons";

export default function ThemeToggle({ compact = false }: { compact?: boolean }) {
  const [theme, setState] = useState<Theme | null>(null);
  useEffect(() => {
    setState(currentTheme());
    const on = (e: Event) => setState((e as CustomEvent<Theme>).detail);
    window.addEventListener("ab:theme", on);
    return () => window.removeEventListener("ab:theme", on);
  }, []);
  const next = theme === "light" ? "dark" : "light";
  return (
    <button
      type="button"
      className="theme-toggle"
      onClick={() => setTheme(currentTheme() === "light" ? "dark" : "light")}
      aria-label={theme ? `Theme: ${theme}. Switch to ${next}` : "Switch theme"}
      title={theme ? `Switch to the ${next} theme` : "Switch theme"}
    >
      <span className="when-dark">
        <Moon />
        {!compact && <span>Dark</span>}
      </span>
      <span className="when-light">
        <Sun />
        {!compact && <span>Light</span>}
      </span>
    </button>
  );
}
