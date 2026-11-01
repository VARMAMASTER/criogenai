import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import "@fontsource-variable/ibm-plex-sans";
import "@fontsource-variable/jetbrains-mono";
import "@/styles/tokens.css";
import "@/styles/glass.css";
import "@/styles/base.css";
import NextTopLoader from "nextjs-toploader";
import Watermark from "@/components/Watermark";
import { SITE } from "@/lib/nav";

export const metadata: Metadata = {
  metadataBase: new URL(SITE.url),
  title: { default: "Criogen", template: "%s | Criogen" },
  description: SITE.description,
  icons: { icon: { url: "/favicon.svg", type: "image/svg+xml" } },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#0b0d10",
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
        <noscript>
          <style>{"[data-reveal]{opacity:1!important;transform:none!important}"}</style>
        </noscript>
      </head>
      <body>
        <NextTopLoader color="#3ecf8e" height={2} showSpinner={false} shadow="0 0 8px #3ecf8e" crawlSpeed={150} />
        <Watermark />
        {children}
      </body>
    </html>
  );
}
