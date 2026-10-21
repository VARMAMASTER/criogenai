import DocPage, { docMetadata } from "@/components/docs/DocPage";
import { allSlugs } from "@/lib/docs";

export const dynamicParams = false;

export function generateStaticParams() {
  return allSlugs()
    .filter(Boolean)
    .map((s) => ({ slug: s.split("/") }));
}

type Props = { params: Promise<{ slug: string[] }> };

export async function generateMetadata({ params }: Props) {
  return docMetadata((await params).slug.join("/"));
}

export default async function Page({ params }: Props) {
  return <DocPage slug={(await params).slug.join("/")} />;
}
