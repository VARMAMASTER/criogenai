// /llms-full.txt: every doc page's Markdown in one file, so an AI assistant
// can read the whole manual in one request.
import { docSources } from "@/lib/docs";
import { SITE } from "@/lib/nav";
import { has, LANG_ORDER, LANGS, snippet, type Lang } from "@/lib/snippets";

export const dynamic = "force-static";

/** `<Sample name="agents" region="zod" only={["typescript"]} />` as fenced code, one block per language. */
function samples(body: string): string {
  return body.replace(/<Sample\s+([^>]*?)\/>/g, (_, attrs: string) => {
    const name = /name="([^"]+)"/.exec(attrs)?.[1] ?? "";
    const region = /region="([^"]+)"/.exec(attrs)?.[1] ?? "";
    const only = /only=\{\[([^\]]*)\]\}/.exec(attrs)?.[1];
    const wanted = only ? (only.match(/"([a-z]+)"/g) ?? []).map((l) => l.replace(/"/g, "") as Lang) : LANG_ORDER;
    return LANG_ORDER.filter((l) => wanted.includes(l) && has(l, name))
      .map((l) => `${LANGS[l].label}:\n\n\`\`\`${LANGS[l].ext}\n${snippet(l, name, region)}\n\`\`\``)
      .join("\n\n");
  });
}

/** MDX components become plain text: a LinkCard keeps its title, text and link. */
function plain(body: string): string {
  return samples(body)
    .replace(/<LinkCard\s+title="([^"]*)"\s+description="([^"]*)"\s+href="([^"]*)"\s*\/>/g, (_, t, d, h) => `- [${t}](${SITE.url}${h}): ${d}`)
    .replace(/^import .*$/gm, "")
    .replace(/<\/?[A-Z][A-Za-z]*[^>]*>/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function GET() {
  // The generated API pages are in /llms-api.txt: together they would double this file.
  const parts = docSources().filter((d) => !d.slug.startsWith("reference/api/")).map((d) => {
    const url = `${SITE.url}/docs/${d.slug ? `${d.slug}/` : ""}`;
    return `# ${d.title}\n\nSource: ${url}\n${d.description ? `\n> ${d.description}\n` : ""}\n${plain(d.body)}\n`;
  });
  const text = `# Criogen documentation\n\n> ${SITE.description}\n\n${parts.join("\n---\n\n")}`;
  return new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
