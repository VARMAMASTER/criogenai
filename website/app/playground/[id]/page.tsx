import Link from "next/link";
import { notFound } from "next/navigation";
import type { Metadata } from "next";
import Sample from "@/components/Sample";
import Story from "@/components/playground/Story";
import { articleJsonLd, fit, pageMeta } from "@/lib/seo";
import { PATTERNS, byId } from "@/lib/patterns";

export const dynamicParams = false;

export function generateStaticParams() {
  return PATTERNS.map((p) => ({ id: p.id }));
}

export async function generateMetadata({ params }: { params: Promise<{ id: string }> }): Promise<Metadata> {
  const p = byId((await params).id);
  if (!p) return {};
  return pageMeta({ ...seo(p), path: `/playground/${p.id}/`, type: "article" });
}

/** Title and description sized for search results (about 60 and 160 characters). */
function seo(p: NonNullable<ReturnType<typeof byId>>) {
  const kind = p.group === "IBM agent types" ? "AI agent type" : "agent pattern";
  return {
    title: fit([`${p.title}: ${kind}, explained and runnable | CriogenAI`, `${p.title}: ${kind}, explained and runnable`, `${p.title}: ${kind}, explained`, `${p.title} ${kind}`], 60),
    absolute: true,
    description: fit(
      [`${p.short} ${p.what[0]} Runnable in your browser.`, `${p.short} Run it in your browser and read the CriogenAI code.`, `${p.short} Runnable in your browser.`, p.short],
      160,
    ),
  };
}

export default async function PatternPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const p = byId(id);
  if (!p) notFound();
  const i = PATTERNS.indexOf(p);
  const prev = PATTERNS[i - 1];
  const next = PATTERNS[i + 1];
  const meta = seo(p);
  const path = `/playground/${p.id}/`;
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{
          __html: articleJsonLd({ title: meta.title, description: meta.description, path, trail: [{ name: "Playground", path: "/playground/" }, { name: p.title, path }] }),
        }}
      />
      <Story id={id} code={<Sample name="patterns" region={id} only={["python", "typescript"]} />} />
      <nav className="sb-pager" aria-label="More patterns">
        {prev ? (
          <Link href={`/playground/${prev.id}/`}>
            <span>Previous</span>
            {prev.title}
          </Link>
        ) : (
          <span />
        )}
        {next && (
          <Link href={`/playground/${next.id}/`} className="sb-next">
            <span>Next</span>
            {next.title}
          </Link>
        )}
      </nav>
    </>
  );
}
