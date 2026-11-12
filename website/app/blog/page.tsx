import Link from "next/link";
import type { Metadata } from "next";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/blog.css";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { allPosts, longDate } from "@/lib/blog";
import { pageMeta } from "@/lib/seo";

export const metadata: Metadata = {
  ...pageMeta({
    title: "Blog",
    description: "Releases, guides and engineering notes from CriogenAI, the fast AI agent framework on a Rust engine.",
    path: "/blog/",
  }),
  alternates: { canonical: "/blog/", types: { "application/rss+xml": "/blog/rss.xml" } },
};

export default function BlogIndex() {
  const posts = allPosts();
  return (
    <div className="site blog">
      <SiteHeader current="blog" />
      <main id="main" className="blog-main">
        <header className="blog-hero">
          <h1>Blog</h1>
          <p>Releases, guides and engineering notes from the people building CriogenAI.</p>
          <a className="blog-rss" href="/blog/rss.xml">
            Subscribe with RSS
          </a>
        </header>
        {posts.length === 0 ? (
          <p className="blog-empty">The first post is on its way.</p>
        ) : (
          <ol className="blog-list">
            {posts.map((p) => (
              <li key={p.slug}>
                <Link href={`/blog/${p.slug}/`} className="blog-card glass">
                  <p className="blog-meta">
                    <time dateTime={p.date}>{longDate(p.date)}</time>
                    <span>{p.minutes} min read</span>
                  </p>
                  <h2>{p.title}</h2>
                  {p.description && <p className="blog-summary">{p.description}</p>}
                  {p.tags.length > 0 && (
                    <ul className="blog-tags" aria-label="Topics">
                      {p.tags.map((t) => (
                        <li key={t}>{t}</li>
                      ))}
                    </ul>
                  )}
                </Link>
              </li>
            ))}
          </ol>
        )}
      </main>
      <SiteFooter />
    </div>
  );
}
