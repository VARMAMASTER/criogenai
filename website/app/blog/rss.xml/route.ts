import { allPosts } from "@/lib/blog";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

const escape = (s: string) => s.replace(/[<>&'"]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", "'": "&apos;", '"': "&quot;" })[c]!);

/** /blog/rss.xml: every published post, for feed readers and tools. */
export function GET() {
  const posts = allPosts();
  const items = posts
    .map((p) => {
      const url = `${SITE.url}/blog/${p.slug}/`;
      return [
        "    <item>",
        `      <title>${escape(p.title)}</title>`,
        `      <link>${url}</link>`,
        `      <guid isPermaLink="true">${url}</guid>`,
        `      <pubDate>${new Date(`${p.date}T09:00:00Z`).toUTCString()}</pubDate>`,
        `      <description>${escape(p.description)}</description>`,
        ...p.tags.map((t) => `      <category>${escape(t)}</category>`),
        "    </item>",
      ].join("\n");
    })
    .join("\n");
  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>Criogen blog</title>
    <link>${SITE.url}/blog/</link>
    <atom:link href="${SITE.url}/blog/rss.xml" rel="self" type="application/rss+xml"/>
    <description>Releases, guides and engineering notes from Criogen, the fast AI agent framework on a Rust engine.</description>
    <language>en</language>
${items}
  </channel>
</rss>
`;
  return new Response(xml, { headers: { "Content-Type": "application/rss+xml; charset=utf-8" } });
}
