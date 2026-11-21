// /llms-full.txt: every doc page's Markdown in one file, so an AI assistant
// can read the whole manual in one request.
import { docSources } from "@/lib/docs";
import { plain } from "@/lib/markdown";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

export function GET() {
  // The generated API pages are in /llms-api.txt: together they would double this file.
  const parts = docSources().filter((d) => !d.slug.startsWith("reference/api/")).map((d) => {
    const url = `${SITE.url}/docs/${d.slug ? `${d.slug}/` : ""}`;
    return `# ${d.title}\n\nSource: ${url}\n${d.description ? `\n> ${d.description}\n` : ""}\n${plain(d.body)}\n`;
  });
  const text = `# CriogenAI documentation\n\n> ${SITE.description}\n\n${parts.join("\n---\n\n")}`;
  return new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
