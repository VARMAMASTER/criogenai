import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import type { MetadataRoute } from "next";
import { allPosts } from "@/lib/blog";
import { allSlugs } from "@/lib/docs";
import { SITE } from "@/lib/nav";
import { PATTERNS } from "@/lib/patterns";

export const dynamic = "force-static";

/**
 * When each file last changed in git (newest commit that touched it), read once.
 * A page's lastmod is its source's date, so search engines can trust it; without
 * git (a tarball build) every page falls back to the build time.
 */
function gitDates(): Map<string, Date> {
  const dates = new Map<string, Date>();
  try {
    const log = execFileSync("git", ["log", "--format=@%cI", "--name-only", "--", "."], {
      cwd: process.cwd(),
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    });
    let current: Date | undefined;
    for (const line of log.split("\n")) {
      if (line.startsWith("@")) current = new Date(line.slice(1));
      else if (line && current) {
        // Paths are relative to the repository root; key them relative to website/.
        const file = line.replace(/^website\//, "");
        if (!dates.has(file)) dates.set(file, current);
      }
    }
  } catch {
    /* not a git checkout */
  }
  return dates;
}

/** Website-relative paths of every doc page's source file. */
function docFile(slug: string): string {
  const direct = `content/docs/${slug || "index"}.mdx`;
  return fs.existsSync(path.join(process.cwd(), direct)) ? direct : `content/docs/${slug}/index.mdx`;
}

/** Every page, for search engines (built with the site). */
export default function sitemap(): MetadataRoute.Sitemap {
  const built = new Date();
  const dates = gitDates();
  /** The newest date among the files a page is built from. */
  const changed = (...files: string[]): Date => {
    const found = files.map((f) => dates.get(f)).filter((d): d is Date => Boolean(d));
    return found.length ? new Date(Math.max(...found.map((d) => d.getTime()))) : built;
  };
  const day = (d: string) => new Date(`${d}T12:00:00Z`);
  const posts = allPosts();
  const newestPost = posts.length ? day(posts.map((p) => p.updated ?? p.date).sort().at(-1) as string) : built;
  const patternsChanged = changed("lib/patterns/index.ts", "lib/patterns/reasoning.ts", "lib/patterns/multi.ts", "lib/patterns/ibm.ts", "app/playground/[id]/page.tsx");
  const top: MetadataRoute.Sitemap = [
    { url: `${SITE.url}/`, lastModified: changed("app/page.tsx", "public/bench/results.json") },
    { url: `${SITE.url}/benchmarks/`, lastModified: changed("app/benchmarks/page.tsx", "public/bench/results.json") },
    { url: `${SITE.url}/docs/`, lastModified: changed("content/docs/index.mdx") },
    { url: `${SITE.url}/playground/`, lastModified: changed("app/playground/page.tsx", "lib/patterns/index.ts") },
    ...PATTERNS.map((p) => ({ url: `${SITE.url}/playground/${p.id}/`, lastModified: patternsChanged })),
    { url: `${SITE.url}/demo/`, lastModified: changed("app/demo/page.tsx") },
    { url: `${SITE.url}/blog/`, lastModified: newestPost },
    { url: `${SITE.url}/about/`, lastModified: changed("app/about/page.tsx") },
    ...posts.map((p) => ({ url: `${SITE.url}/blog/${p.slug}/`, lastModified: day(p.updated ?? p.date) })),
  ];
  const docs = allSlugs()
    .filter(Boolean)
    .map((slug) => ({ url: `${SITE.url}/docs/${slug}/`, lastModified: changed(docFile(slug)) }));
  return [...top, ...docs];
}
