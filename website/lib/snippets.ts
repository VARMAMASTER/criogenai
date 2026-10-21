// Code samples live in snippets/ as real source files, compiled or
// type-checked by the project's scripts. Pages show the part between `[show]`
// and `[/show]` (or a named region: `[show sync]` … `[/show sync]`). Read at
// build time; never copied here.
import fs from "node:fs";
import path from "node:path";

export type Lang = "python" | "typescript" | "rust";

export const LANG_ORDER: Lang[] = ["python", "typescript", "rust"];

export const LANGS: Record<Lang, { label: string; ext: string; dir: string; comment: string }> = {
  python: { label: "Python", ext: "py", dir: "python", comment: "#" },
  typescript: { label: "TypeScript", ext: "ts", dir: "typescript", comment: "//" },
  rust: { label: "Rust", ext: "rs", dir: "rust/src", comment: "//" },
};

const ROOT = path.join(process.cwd(), "snippets");

function fileOf(lang: Lang, name: string): string {
  const { ext, dir } = LANGS[lang];
  return path.join(ROOT, dir, `${name}.${ext}`);
}

export function has(lang: Lang, name: string): boolean {
  return fs.existsSync(fileOf(lang, name));
}

/** The shown region of `snippets/<dir>/<name>.<ext>`, dedented. */
export function snippet(lang: Lang, name: string, region = ""): string {
  const file = fileOf(lang, name);
  if (!fs.existsSync(file)) throw new Error(`no snippet ${file}`);
  const text = fs.readFileSync(file, "utf8");
  const { comment } = LANGS[lang];
  const open = `${comment} [show${region ? ` ${region}` : ""}]`;
  const close = `${comment} [/show${region ? ` ${region}` : ""}]`;
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const start = lines.findIndex((l) => l.trim() === open);
  const end = lines.findIndex((l, i) => i > start && l.trim() === close);
  if (start < 0 || end < 0) throw new Error(`${file}: no ${open} … ${close} region`);
  const shown = lines.slice(start + 1, end);
  const indent = Math.min(...shown.filter((l) => l.trim()).map((l) => l.length - l.trimStart().length));
  return shown
    .map((l) => l.slice(indent))
    .join("\n")
    .trim();
}
