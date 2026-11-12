// The blog: one MDX file per post in content/blog, compiled at build time
// like the docs (same Markdown, same components). A post's front matter:
//
//   ---
//   title: CriogenAI 0.1.5
//   description: One or two sentences for the list, search results and link cards.
//   date: 2026-11-09
//   author: The CriogenAI team
//   tags: release, mcp, evals
//   draft: false
//   ---
//
// The file name is the address: content/blog/criogen-0-1-5.mdx is
// /blog/criogen-0-1-5/. Posts with `draft: true`, and files starting with `_`,
// are not published.
import fs from "node:fs";
import path from "node:path";
import { evaluate } from "@mdx-js/mdx";
import * as runtime from "react/jsx-runtime";
import remarkGfm from "remark-gfm";
import { frontmatter, rehypeHeadings, type MDXContent, type TocEntry } from "./docs";

export interface PostMeta {
  slug: string;
  title: string;
  description: string;
  /** YYYY-MM-DD. */
  date: string;
  author: string;
  tags: string[];
  /** Minutes, at 220 words a minute. */
  minutes: number;
}

export interface Post extends PostMeta {
  toc: TocEntry[];
  Content: MDXContent;
}

const CONTENT = path.join(process.cwd(), "content", "blog");

function read(file: string): { meta: PostMeta; body: string; draft: boolean } {
  const slug = path.basename(file, ".mdx");
  const { data, body } = frontmatter(fs.readFileSync(path.join(CONTENT, file), "utf8"));
  if (!data.title || !data.date || !/^\d{4}-\d{2}-\d{2}$/.test(data.date)) {
    throw new Error(`content/blog/${file}: a post needs a title and a date (YYYY-MM-DD)`);
  }
  const words = body.replace(/```[\s\S]*?```/g, " ").split(/\s+/).filter(Boolean).length;
  return {
    meta: {
      slug,
      title: data.title,
      description: data.description ?? "",
      date: data.date,
      author: data.author || "The CriogenAI team",
      tags: (data.tags ?? "")
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
      minutes: Math.max(1, Math.round(words / 220)),
    },
    body,
    draft: data.draft === "true",
  };
}

function files(): string[] {
  if (!fs.existsSync(CONTENT)) return [];
  return fs.readdirSync(CONTENT).filter((f) => f.endsWith(".mdx") && !f.startsWith("_"));
}

/** Every published post, newest first. */
export function allPosts(): PostMeta[] {
  return files()
    .map(read)
    .filter((p) => !p.draft)
    .map((p) => p.meta)
    .sort((a, b) => (a.date === b.date ? a.slug.localeCompare(b.slug) : b.date.localeCompare(a.date)));
}

/** A post's Markdown source, for llms-full.txt. */
export function postSource(slug: string): string {
  return read(`${slug}.mdx`).body;
}

export async function loadPost(slug: string): Promise<Post> {
  const { meta, body } = read(`${slug}.mdx`);
  const toc: TocEntry[] = [];
  const { default: Content } = await evaluate(body, {
    ...runtime,
    development: false,
    remarkPlugins: [remarkGfm],
    rehypePlugins: [rehypeHeadings(toc)],
  } as Parameters<typeof evaluate>[1]);
  return { ...meta, toc, Content: Content as unknown as MDXContent };
}

/** "9 November 2026". */
export function longDate(date: string): string {
  return new Date(`${date}T12:00:00Z`).toLocaleDateString("en-GB", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
}
