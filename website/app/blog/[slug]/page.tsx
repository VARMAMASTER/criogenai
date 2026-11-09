import Link from "next/link";
import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/blog.css";
import { mdxComponents } from "@/components/mdx";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { allPosts, loadPost, longDate } from "@/lib/blog";
import { SITE } from "@/lib/nav";
import { OG_IMAGE, pageMeta } from "@/lib/seo";

export const dynamicParams = false;

export function generateStaticParams() {
  return allPosts().map((p) => ({ slug: p.slug }));
}

type Props = { params: Promise<{ slug: string }> };

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const post = await loadPost((await params).slug);
  const base = pageMeta({ title: post.title, description: post.description, path: `/blog/${post.slug}/`, type: "article" });
  return {
    ...base,
    authors: [{ name: post.author }],
    keywords: post.tags,
    openGraph: { ...base.openGraph, type: "article", publishedTime: post.date, authors: [post.author], tags: post.tags },
  };
}

export default async function PostPage({ params }: Props) {
  const post = await loadPost((await params).slug);
  const { Content } = post;
  // Newest first: the previous post is the older one.
  const posts = allPosts();
  const at = posts.findIndex((p) => p.slug === post.slug);
  const older = posts[at + 1];
  const newer = at > 0 ? posts[at - 1] : undefined;
  const ld = {
    "@context": "https://schema.org",
    "@type": "BlogPosting",
    headline: post.title,
    description: post.description,
    datePublished: post.date,
    author: { "@type": "Organization", name: post.author },
    publisher: { "@id": `${SITE.url}/#org`, "@type": "Organization", name: "Criogen", logo: { "@type": "ImageObject", url: `${SITE.url}/icon-512.png` } },
    image: `${SITE.url}${OG_IMAGE.url}`,
    mainEntityOfPage: `${SITE.url}/blog/${post.slug}/`,
    keywords: post.tags.join(", "),
  };
  return (
    <div className="site blog">
      <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(ld).replace(/</g, "\\u003c") }} />
      <SiteHeader current="blog" />
      <main id="main" className="blog-main">
        <article className="blog-post glass" data-pagefind-body="">
          <p className="blog-back" data-pagefind-ignore="">
            <Link href="/blog/">All posts</Link>
          </p>
          <header className="blog-post-head">
            <h1>{post.title}</h1>
            <p className="blog-meta">
              <span>{post.author}</span>
              <time dateTime={post.date}>{longDate(post.date)}</time>
              <span>{post.minutes} min read</span>
            </p>
            {post.tags.length > 0 && (
              <ul className="blog-tags" aria-label="Topics">
                {post.tags.map((t) => (
                  <li key={t}>{t}</li>
                ))}
              </ul>
            )}
          </header>
          <div className="prose blog-body">
            <Content components={mdxComponents} />
          </div>
          {(older || newer) && (
            <nav className="pager" aria-label="Older and newer posts" data-pagefind-ignore="">
              {older ? (
                <Link href={`/blog/${older.slug}/`} rel="prev">
                  <span className="pager-hint">Older</span>
                  <span className="pager-title">{older.title}</span>
                </Link>
              ) : (
                <span />
              )}
              {newer && (
                <Link href={`/blog/${newer.slug}/`} rel="next" className="pager-next">
                  <span className="pager-hint">Newer</span>
                  <span className="pager-title">{newer.title}</span>
                </Link>
              )}
            </nav>
          )}
        </article>
      </main>
      <SiteFooter />
    </div>
  );
}
