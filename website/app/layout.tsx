import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import "@fontsource-variable/nunito";
import "@fontsource-variable/jetbrains-mono";
import "@/styles/tokens.css";
import "@/styles/glass.css";
import "@/styles/base.css";
import { SITE } from "@/lib/nav";

export const metadata: Metadata = {
  metadataBase: new URL(SITE.url),
  title: { default: "AgentBuild", template: "%s | AgentBuild" },
  description: SITE.description,
  icons: { icon: { url: "/favicon.svg", type: "image/svg+xml" } },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#070e1c",
};

// Before the first paint: the theme (saved, else the system's) and the
// remembered code language, so neither flashes. Same storage keys as the
// previous (Starlight) site, so visitors keep their choices.
const BOOT = `(function(){var d=document.documentElement;try{var t=localStorage.getItem("starlight-theme");if(t!=="light"&&t!=="dark"){t=window.matchMedia("(prefers-color-scheme: light)").matches?"light":"dark"}d.dataset.theme=t;var m={Python:"python",TypeScript:"typescript",Rust:"rust"};var l=m[localStorage.getItem("starlight-synced-tabs__lang")||""];if(l)d.dataset.lang=l}catch(e){d.dataset.theme=d.dataset.theme||"dark"}})();`;

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme="dark" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: BOOT }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
