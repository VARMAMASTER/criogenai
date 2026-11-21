// MDX source as plain Markdown for AI assistants and agents: code samples
// become fenced code per language, components become text or links.
import { SITE } from "@/lib/nav";
import { has, LANG_ORDER, LANGS, snippet, type Lang } from "@/lib/snippets";

/** `<Sample name="agents" region="zod" only={["typescript"]} />` as fenced code, one block per language. */
export function samples(body: string): string {
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
export function plain(body: string): string {
  return samples(body)
    .replace(/<LinkCard\s+title="([^"]*)"\s+description="([^"]*)"\s+href="([^"]*)"\s*\/>/g, (_, t, d, h) => `- [${t}](${SITE.url}${h}): ${d}`)
    .replace(/^import .*$/gm, "")
    .replace(/<\/?[A-Z][A-Za-z]*[^>]*>/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

