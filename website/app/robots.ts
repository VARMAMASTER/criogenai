import type { MetadataRoute } from "next";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

/** Every crawler is welcome, AI assistants' too: being quoted is the point. */
export default function robots(): MetadataRoute.Robots {
  return {
    rules: [{ userAgent: "*", allow: "/" }],
    sitemap: `${SITE.url}/sitemap.xml`,
    host: SITE.url,
  };
}
