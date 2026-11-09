import type { MetadataRoute } from "next";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

/** /manifest.webmanifest: the name and icons phones and browsers use when the
 * site is saved to a home screen. The icons come from scripts/icons.mjs. */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Criogen: fast AI agent framework",
    short_name: "Criogen",
    description: SITE.description,
    start_url: "/",
    display: "standalone",
    background_color: "#0b0d10",
    theme_color: "#0b0d10",
    icons: [
      { src: "/icon-192.png", sizes: "192x192", type: "image/png" },
      { src: "/icon-512.png", sizes: "512x512", type: "image/png" },
      { src: "/icon-maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
