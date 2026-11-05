// The playground: a library of agent patterns, one page each, like a
// component storybook. The layout stays mounted while moving between
// patterns, so a key typed once (tab memory only) carries over.
import type { ReactNode } from "react";
import "@/styles/site.css";
import "@/styles/docs.css";
import "@/styles/lab.css";
import "@/styles/patterns.css";
import SiteHeader, { SiteFooter } from "@/components/SiteHeader";
import { KeyProvider } from "@/components/playground/KeyProvider";
import Sidebar from "@/components/playground/Sidebar";

export default function PlaygroundLayout({ children }: { children: ReactNode }) {
  return (
    <div className="site lab">
      <a className="skip" href="#main">
        Skip to content
      </a>
      <SiteHeader current="playground" />
      <KeyProvider>
        <div className="sb-shell">
          <Sidebar />
          <main id="main" className="sb-main">
            {children}
          </main>
        </div>
      </KeyProvider>
      <SiteFooter />
    </div>
  );
}
