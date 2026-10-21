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

export function setTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
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
