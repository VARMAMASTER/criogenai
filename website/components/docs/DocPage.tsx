import type { Metadata } from "next";
import { articleJsonLd, pageMeta } from "@/lib/seo";
import { loadDoc } from "@/lib/docs";
import { mdxComponents } from "../mdx";
import DocsShell from "./DocsShell";

export async function docMetadata(slug: string): Promise<Metadata> {
  const page = await loadDoc(slug);
  return pageMeta({ title: page.seoTitle ?? page.title, description: page.description, path: slug ? `/docs/${slug}/` : "/docs/", type: "article" });
}

export default async function DocPage({ slug }: { slug: string }) {
  const page = await loadDoc(slug);
  const { Content } = page;
  const path = slug ? `/docs/${slug}/` : "/docs/";
  const trail = slug ? [{ name: "Docs", path: "/docs/" }, { name: page.title, path }] : [{ name: "Docs", path }];
  return (
    <DocsShell slug={slug} title={page.title} toc={page.toc}>
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: articleJsonLd({ title: page.title, description: page.description, path, trail }) }} />
      <Content components={mdxComponents} />
    </DocsShell>
  );
}
