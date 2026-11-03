import type { Metadata } from "next";
import { pageMeta } from "@/lib/seo";
import { loadDoc } from "@/lib/docs";
import { mdxComponents } from "../mdx";
import DocsShell from "./DocsShell";

export async function docMetadata(slug: string): Promise<Metadata> {
  const page = await loadDoc(slug);
  return pageMeta({ title: page.title, description: page.description, path: slug ? `/docs/${slug}/` : "/docs/", type: "article" });
}

export default async function DocPage({ slug }: { slug: string }) {
  const page = await loadDoc(slug);
  const { Content } = page;
  return (
    <DocsShell slug={slug} title={page.title} toc={page.toc}>
      <Content components={mdxComponents} />
    </DocsShell>
  );
}
