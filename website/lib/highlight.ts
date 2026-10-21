// Syntax highlighting at build time (Shiki), with a dark and a light theme
// in the same HTML: CSS picks one by the page's theme (styles/code.css).
import { createHighlighter, createJavaScriptRegexEngine, type Highlighter } from "shiki";

const LANGS = ["python", "typescript", "javascript", "rust", "toml", "shellscript", "json", "jsx", "tsx"] as const;
const ALIASES: Record<string, string> = {
  py: "python",
  ts: "typescript",
  js: "javascript",
  mjs: "javascript",
  rs: "rust",
  sh: "shellscript",
  bash: "shellscript",
  shell: "shellscript",
};

let highlighter: Promise<Highlighter> | null = null;

function get(): Promise<Highlighter> {
  highlighter ??= createHighlighter({
    themes: ["github-dark-default", "github-light-default"],
    langs: [...LANGS],
    engine: createJavaScriptRegexEngine(),
  });
  return highlighter;
}

/** Highlighted `<pre class="shiki">…</pre>` HTML for `code`. */
export async function highlight(code: string, lang = "text"): Promise<string> {
  const h = await get();
  const name = ALIASES[lang] ?? lang;
  const known = (h.getLoadedLanguages() as string[]).includes(name);
  return h.codeToHtml(code, {
    lang: known ? name : "text",
    themes: { dark: "github-dark-default", light: "github-light-default" },
    defaultColor: false,
  });
}
