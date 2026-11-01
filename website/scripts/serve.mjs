// Preview the static export: serves out/ on http://localhost:4400 (or PORT),
// the way a static host does (folder/index.html, 404.html). No dependencies.
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const out = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "out");
const port = Number(process.env.PORT ?? 4400);
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
};

if (!fs.existsSync(out)) {
  console.error("No out/ folder: run `npm run build` first.");
  process.exit(1);
}

http
  .createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    let file = path.join(out, decodeURIComponent(url.pathname));
    if (!file.startsWith(out)) {
      res.writeHead(403).end();
      return;
    }
    // Next asks for a page's segment data as `__next.docs.$c$slug.__PAGE__.txt`
    // and writes it as `__next.docs/$c$slug/__PAGE__.txt`: map one to the other.
    if (!fs.existsSync(file) && /^__next\.[^.]+\..+\.txt$/.test(path.basename(file))) {
      const dir = path.dirname(file);
      const [head, tail, ...rest] = path.basename(file, ".txt").split(".");
      const mapped = path.join(dir, `${head}.${tail}`, ...rest) + ".txt";
      if (fs.existsSync(mapped)) file = mapped;
    }
    if (fs.existsSync(file) && fs.statSync(file).isDirectory()) {
      if (!url.pathname.endsWith("/")) {
        res.writeHead(308, { Location: `${url.pathname}/${url.search}` }).end();
        return;
      }
      file = path.join(file, "index.html");
    }
    let status = 200;
    if (!fs.existsSync(file)) {
      status = 404;
      file = path.join(out, "404.html");
    }
    res.writeHead(status, { "Content-Type": TYPES[path.extname(file)] ?? "application/octet-stream" });
    fs.createReadStream(file).pipe(res);
  })
  .listen(port, "127.0.0.1", () => console.log(`Serving out/ on http://localhost:${port}`));
