import "@/styles/site.css";
import "@/styles/lab.css";
import type { Metadata } from "next";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";

// Its own title, and one robots rule (the root layout's "index, follow" would otherwise also apply).
export const metadata: Metadata = {
  title: { absolute: "Page not found | CriogenAI" },
  robots: { index: false, follow: true },
};

export default function NotFound() {
  return (
    <div className="site lab">
      <SiteHeader />
      <main id="main" className="lab-main">
        <section className="lab-card lab-empty" style={{ marginTop: "3rem" }}>
          <span className="empty-dot" aria-hidden="true" />
          <h1>This page does not exist</h1>
          <p>
            It may have moved. Try <a href="/docs/">the docs</a> or <a href="/">the home page</a>.
          </p>
        </section>
      </main>
      <SiteFooter />
    </div>
  );
}
