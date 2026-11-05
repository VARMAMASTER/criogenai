import type { MetadataRoute } from "next";
import { allSlugs } from "@/lib/docs";
import { SITE } from "@/lib/nav";
import { PATTERNS } from "@/lib/patterns";

export const dynamic = "force-static";

/** Every page, for search engines (built with the site). */
export default function sitemap(): MetadataRoute.Sitemap {
  const built = new Date();
  const top: MetadataRoute.Sitemap = [
    { url: `${SITE.url}/`, lastModified: built, changeFrequency: "weekly", priority: 1 },
    { url: `${SITE.url}/benchmarks/`, lastModified: built, changeFrequency: "monthly", priority: 0.9 },
    { url: `${SITE.url}/docs/`, lastModified: built, changeFrequency: "weekly", priority: 0.9 },
    { url: `${SITE.url}/playground/`, lastModified: built, changeFrequency: "monthly", priority: 0.8 },
    ...PATTERNS.map((p) => ({ url: `${SITE.url}/playground/${p.id}/`, lastModified: built, changeFrequency: "monthly" as const, priority: 0.6 })),
    { url: `${SITE.url}/demo/`, lastModified: built, changeFrequency: "monthly", priority: 0.6 },
  ];
  const docs = allSlugs()
    .filter(Boolean)
    .map((slug) => ({
      url: `${SITE.url}/docs/${slug}/`,
      lastModified: built,
      changeFrequency: "weekly" as const,
      priority: slug === "quickstart" || slug === "install" ? 0.8 : 0.6,
    }));
  return [...top, ...docs];
}
