// /md/docs/<slug>.md and /md/blog/<slug>.md: each page as plain Markdown for
// AI agents and assistants (smaller than the HTML, no scripts, code samples
// inline). Pages link here with <link rel="alternate" type="text/markdown">,
// and Vercel serves these files to clients that ask for text/markdown on the
// page's own URL (vercel.json rewrites).
import { allPosts, postSource } from "@/lib/blog";
import { docSources } from "@/lib/docs";
import { plain } from "@/lib/markdown";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";
export const dynamicParams = false;

function docPath(slug: string): string[] {
  return slug ? ["docs", ...slug.split("/").slice(0, -1), `${slug.split("/").pop()}.md`] : ["docs", "index.md"];
}

export function generateStaticParams() {
  return [
    ...docSources().map((d) => ({ path: docPath(d.slug) })),
    ...allPosts().map((p) => ({ path: ["blog", `${p.slug}.md`] })),
  ];
}

/** Site-relative links become absolute, so they still work out of context. */
function absolute(md: string): string {
  return md.replace(/\]\(\/(?!\/)/g, `](${SITE.url}/`);
}

function page(title: string, url: string, description: string, body: string, extra = ""): Response {
  const head = `# ${title}\n\nSource: ${url}${extra}\n${description ? `\n> ${description}\n` : ""}`;
  const footer = `\n\n---\n\nCriogenAI: ${SITE.description}\nDocs: ${SITE.url}/docs/ · Full manual for AI assistants: ${SITE.url}/llms-full.txt\n`;
  return new Response(absolute(`${head}\n${plain(body)}${footer}`), {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}

export async function GET(_req: Request, { params }: { params: Promise<{ path: string[] }> }) {
  const { path } = await params;
  const [section, ...rest] = path;
  const last = (rest.pop() ?? "").replace(/\.md$/, "");
  if (section === "blog") {
    const meta = allPosts().find((p) => p.slug === last);
    if (!meta) return new Response("Not found", { status: 404 });
    return page(meta.title, `${SITE.url}/blog/${meta.slug}/`, meta.description ?? "", postSource(meta.slug), `\nPublished: ${meta.date}`);
  }
  const slug = last === "index" && rest.length === 0 ? "" : [...rest, last].join("/");
  const doc = docSources().find((d) => d.slug === slug);
  if (!doc) return new Response("Not found", { status: 404 });
  return page(doc.title, `${SITE.url}/docs/${slug ? `${slug}/` : ""}`, doc.description, doc.body);
}
