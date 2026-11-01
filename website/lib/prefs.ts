// Visitor preferences kept in localStorage: the theme and the code language.
// Every access is guarded: storage can be missing or blocked.

export type Theme = "light" | "dark";
export type LangLabel = "Python" | "TypeScript" | "Rust";

const THEME_KEY = "starlight-theme";
const LANG_KEY = "starlight-synced-tabs__lang";
const LANG_EVENT = "ab:lang";

export function currentTheme(): Theme {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

/**
 * Switch the theme. With the View Transitions API the new theme spreads from
 * `origin` (the toggle) as a circle; without it, or when the visitor asks for
 * reduced motion, the colours fade (or just switch).
 */
export function setTheme(theme: Theme, origin?: { x: number; y: number }): void {
  const root = document.documentElement;
  const apply = () => {
    root.dataset.theme = theme;
  };
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const start = (document as Document & { startViewTransition?: (cb: () => void) => { ready: Promise<void> } }).startViewTransition;
  if (reduce) {
    apply();
  } else if (start && origin) {
    const radius = Math.hypot(Math.max(origin.x, innerWidth - origin.x), Math.max(origin.y, innerHeight - origin.y));
    const transition = start.call(document, apply);
    transition.ready
      .then(() =>
        root.animate(
          { clipPath: [`circle(0px at ${origin.x}px ${origin.y}px)`, `circle(${radius}px at ${origin.x}px ${origin.y}px)`] },
          { duration: 600, easing: "cubic-bezier(0.22, 1, 0.36, 1)", pseudoElement: "::view-transition-new(root)" },
        ),
      )
      .catch(() => {});
  } else {
    root.classList.add("theme-fade");
    apply();
    window.setTimeout(() => root.classList.remove("theme-fade"), 400);
  }
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    /* not saved; the page still switches */
  }
  window.dispatchEvent(new CustomEvent("ab:theme", { detail: theme }));
}

export function storedLang(): string | null {
  try {
    return localStorage.getItem(LANG_KEY);
  } catch {
    return null;
  }
}

const SLUG: Record<string, string> = { Python: "python", TypeScript: "typescript", Rust: "rust" };

/** Choose a language everywhere: every synced tab group follows. */
export function setLang(label: string): void {
  const slug = SLUG[label];
  if (slug) document.documentElement.dataset.lang = slug;
  try {
    localStorage.setItem(LANG_KEY, label);
  } catch {
    /* not saved */
  }
  window.dispatchEvent(new CustomEvent(LANG_EVENT, { detail: label }));
}

export function onLang(fn: (label: string) => void): () => void {
  const handler = (e: Event) => fn((e as CustomEvent<string>).detail);
  window.addEventListener(LANG_EVENT, handler);
  return () => window.removeEventListener(LANG_EVENT, handler);
}

export function langSlug(label: string): string {
  return SLUG[label] ?? label.toLowerCase();
}
