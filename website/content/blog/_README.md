# Writing a blog post

One file per post: `content/blog/<slug>.mdx`. The file name is the address, so
`criogen-0-1-5.mdx` is published at `/blog/criogen-0-1-5/`. Files that start
with `_` (like this one) are never published.

```mdx
---
title: "The title, in quotes if it has a colon"
description: One or two sentences for the post list, search results, link cards and RSS.
date: 2026-11-09
author: The Criogen team
tags: release, mcp, evals
draft: true
---

The post, in Markdown (tables, task lists and footnotes work).

<Sample name="mcp" />
```

- `title` and `date` (YYYY-MM-DD) are required. The build fails without them.
- `author` defaults to "The Criogen team". `tags` is a comma list.
- `draft: true` keeps a post off the site until you remove the line.
- Posts are newest first. The list, RSS feed (`/blog/rss.xml`), sitemap and
  `llms.txt` update themselves when the site is built.
- The docs' components work here too. `<Sample name="…" />` shows the tested
  code from `snippets/` in Python, TypeScript and Rust; prefer it to pasting
  code, so samples stay correct.
- Link to docs with site paths (`/docs/guides/mcp/`).
- Quote performance numbers only from `docs/benchmarks.md`, with what was measured.

Check a post with `npm run build && npm run preview`, then open
http://localhost:4400/blog/.
