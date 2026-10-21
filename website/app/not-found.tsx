import "@/styles/site.css";
import "@/styles/lab.css";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";

export default function NotFound() {
  return (
    <div className="site lab">
      <SiteHeader />
      <main id="main" className="lab-main">
        <section className="lab-card lab-empty" style={{ marginTop: "3rem" }}>
          <span className="empty-dot" aria-hidden="true" />
          <h2>This page does not exist</h2>
          <p>
            It may have moved. Try <a href="/docs/">the docs</a> or <a href="/">the home page</a>.
          </p>
        </section>
      </main>
      <SiteFooter />
    </div>
  );
}
