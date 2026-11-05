import Link from "next/link";
import { notFound } from "next/navigation";
import type { Metadata } from "next";
import Sample from "@/components/Sample";
import Story from "@/components/playground/Story";
import { pageMeta } from "@/lib/seo";
import { PATTERNS, byId } from "@/lib/patterns";

export const dynamicParams = false;

export function generateStaticParams() {
  return PATTERNS.map((p) => ({ id: p.id }));
}

export async function generateMetadata({ params }: { params: Promise<{ id: string }> }): Promise<Metadata> {
  const p = byId((await params).id);
  if (!p) return {};
  return pageMeta({
    title: `${p.title}: ${p.group === "IBM agent types" ? "AI agent type" : "agent pattern"}, explained and runnable`,
    description: `${p.short} ${p.what[0]} Run it in your browser step by step and see the Criogen code in Python and TypeScript.`,
    path: `/playground/${p.id}/`,
    type: "article",
  });
}

export default async function PatternPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const p = byId(id);
  if (!p) notFound();
  const i = PATTERNS.indexOf(p);
  const prev = PATTERNS[i - 1];
  const next = PATTERNS[i + 1];
  return (
    <>
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
