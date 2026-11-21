import type { MetadataRoute } from "next";
import { SITE } from "@/lib/nav";

export const dynamic = "force-static";

/** Every crawler is welcome, AI assistants' too: being quoted is the point. */
export default function robots(): MetadataRoute.Robots {
  return {
    rules: [
      { userAgent: "*", allow: "/" },
      // Named so there is no doubt: AI search and assistant crawlers are welcome.
      {
        userAgent: [
          "GPTBot", "OAI-SearchBot", "ChatGPT-User", "ClaudeBot", "Claude-SearchBot", "Claude-User",
          "PerplexityBot", "Perplexity-User", "Google-Extended", "Applebot-Extended", "Amazonbot",
          "meta-externalagent", "DuckAssistBot", "MistralAI-User", "cohere-ai", "Bytespider", "CCBot",
        ],
        allow: "/",
      },
    ],
    sitemap: `${SITE.url}/sitemap.xml`,
    host: SITE.url,
  };
}
