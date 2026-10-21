import type { Metadata } from "next";
import "@/styles/docs.css";
import { loadDoc } from "@/lib/docs";
import { mdxComponents } from "../mdx";
import DocsShell from "./DocsShell";

export async function docMetadata(slug: string): Promise<Metadata> {
  const page = await loadDoc(slug);
  return {
    title: page.title,
    description: page.description,
    alternates: { canonical: slug ? `/docs/${slug}/` : "/docs/" },
  };
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
