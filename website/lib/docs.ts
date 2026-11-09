// Docs pages: MDX files in content/docs, compiled at build time. Headings get
// ids and anchor links, and the h2/h3 headings become the page's table of
// contents.
import fs from "node:fs";
import path from "node:path";
import { evaluate } from "@mdx-js/mdx";
import type { ComponentType, ReactElement } from "react";
import * as runtime from "react/jsx-runtime";
import remarkGfm from "remark-gfm";
import { LAYERS } from "./errors";
import { ALL_PAGES } from "./nav";

export interface TocEntry {
  depth: 2 | 3;
  id: string;
  text: string;
}

// What an evaluated MDX page is: a component that takes its components by name.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type MDXContent = (props: { components?: Record<string, ComponentType<any>> }) => ReactElement;

export interface DocPage {
  slug: string;
  title: string;
  description: string;
  toc: TocEntry[];
  Content: MDXContent;
}

const CONTENT = path.join(process.cwd(), "content", "docs");

function fileOf(slug: string): string {
  const direct = path.join(CONTENT, `${slug || "index"}.mdx`);
  if (fs.existsSync(direct)) return direct;
  return path.join(CONTENT, slug, "index.mdx");
}

export function allSlugs(): string[] {
  return ALL_PAGES.map((p) => p.slug);
}

export function frontmatter(source: string): { data: Record<string, string>; body: string } {
  const text = source.replace(/\r\n/g, "\n");
  const match = /^---\n([\s\S]*?)\n---\n?/.exec(text);
  if (!match) return { data: {}, body: text };
  const data: Record<string, string> = {};
  for (const line of match[1].split("\n")) {
    const kv = /^(\w+):\s*(.*)$/.exec(line);
    if (!kv) continue;
    let value = kv[2].trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    data[kv[1]] = value;
  }
  return { data, body: text.slice(match[0].length) };
}

/** Escape what MDX would read as code (`{`, `<`) outside code spans and fences. */
function escapeMarkdownForMdx(md: string): string {
  let fenced = false;
  return md
    .split("\n")
    .map((line) => {
      if (/^\s*```/.test(line)) {
        fenced = !fenced;
        return line;
      }
      if (fenced) return line;
      return line
        .split(/(`[^`]*`)/)
        .map((part, i) => (i % 2 ? part : part.replace(/[{}]/g, (c) => `\\${c}`).replace(/<(?![a-zA-Z/])/g, "&lt;")))
        .join("");
    })
    .join("\n");
}

/** docs/benchmarks.md, as the Benchmarks page shows it (its own h1 dropped). */
function benchmarksMarkdown(): string {
  const file = path.join(process.cwd(), "..", "docs", "benchmarks.md");
  const md = fs.readFileSync(file, "utf8").replace(/\r\n/g, "\n");
  const withoutTitle = md.replace(/^# [^\n]*\n+/, "");
  // Links relative to docs/ point into the (private) repository: keep their text only.
  const linked = withoutTitle.replace(/\[([^\]]+)\]\((?!https?:|\/|#)[^)]+\)/g, "$1");
  return escapeMarkdownForMdx(linked);
}

// --- a small hast walk (no extra dependency) -------------------------------

interface HastNode {
  type: string;
  tagName?: string;
  name?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
}

function textOf(node: HastNode): string {
  if (node.type === "text") return node.value ?? "";
  return (node.children ?? []).map(textOf).join("");
}

/** GitHub-style heading slugs, as Starlight makes them. */
export function slugger() {
  const seen = new Map<string, number>();
  return (text: string) => {
    const base = text
      .toLowerCase()
      .trim()
      .replace(/[^\p{L}\p{M}\p{N}\p{Pc}\- ]/gu, "")
      .replace(/ /g, "-");
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    return n ? `${base}-${n}` : base;
  };
}

export function rehypeHeadings(toc: TocEntry[]) {
  return () => (tree: HastNode) => {
    const slug = slugger();
    const walk = (node: HastNode) => {
      for (const child of node.children ?? []) {
        if (child.type === "element" && /^h[2-4]$/.test(child.tagName ?? "")) {
          const text = textOf(child).trim();
          const id = slug(text);
          child.properties = { ...(child.properties ?? {}), id };
          child.children = [
            ...(child.children ?? []),
            {
              type: "element",
              tagName: "a",
              properties: { className: ["anchor"], href: `#${id}`, ariaLabel: `Link to this section: ${text}`, dataPagefindIgnore: "" },
              children: [{ type: "text", value: "#" }],
            },
          ];
          const depth = Number(child.tagName![1]);
          if (depth === 2 || depth === 3) toc.push({ depth, id, text });
        } else if (child.type === "mdxJsxFlowElement" && child.name === "ErrorTable") {
          for (const [layer, title] of LAYERS) toc.push({ depth: 2, id: `layer-${layer}`, text: title });
        } else {
          walk(child);
        }
      }
    };
    walk(tree);
  };
}

/** Each doc's slug, title, description and Markdown source (no MDX compile): for the sitemap and llms.txt. */
export function docSources(): { slug: string; title: string; description: string; body: string }[] {
  return allSlugs().map((slug) => {
    const { data, body } = frontmatter(fs.readFileSync(fileOf(slug), "utf8"));
    const text = body.replace(/^<Benchmarks \/>$/m, () => benchmarksMarkdown());
    return { slug, title: data.title ?? slug, description: data.description ?? "", body: text };
  });
}

export async function loadDoc(slug: string): Promise<DocPage> {
  const source = fs.readFileSync(fileOf(slug), "utf8");
  const { data, body } = frontmatter(source);
  const text = body.replace(/^<Benchmarks \/>$/m, () => benchmarksMarkdown());
  const toc: TocEntry[] = [];
  const { default: Content } = await evaluate(text, {
    ...runtime,
    development: false,
    remarkPlugins: [remarkGfm],
    rehypePlugins: [rehypeHeadings(toc)],
  } as Parameters<typeof evaluate>[1]);
  return { slug, title: data.title ?? "", description: data.description ?? "", toc, Content: Content as unknown as MDXContent };
}
