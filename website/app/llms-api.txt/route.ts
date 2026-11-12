// /llms-api.txt: the Python and TypeScript API reference in one file, for an
// AI assistant that writes code against CriogenAI. (llms-full.txt is the manual
// without it: the two together would be too much to read at once.)
import { docSources } from "@/lib/docs";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

export function GET() {
  const pages = docSources().filter((d) => /^reference\/api\/(python|typescript)\//.test(d.slug));
  const parts = pages.map((d) => `# ${d.title}\n\nSource: ${SITE.url}/docs/${d.slug}/\n\n${d.body.trim()}\n`);
  const text =
    `# CriogenAI API reference: Python and TypeScript\n\n> Every public class, function and type, generated from the source. ` +
    `Rust: ${SITE.url}/docs/reference/api/rust/. The manual: ${SITE.url}/llms-full.txt\n\n${parts.join("\n---\n\n")}`;
  return new Response(text, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
