// Every Criogen icon, drawn from the one mark (lib/mark.ts), so no place shows
// an older logo: favicon.ico (16, 32, 48), apple-touch-icon.png (180), the
// web app icons (192, 512, and a maskable 512 with a safe zone) and mark.svg.
// Run after changing the mark: node scripts/icons.mjs (writes to public/).
import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const site = join(here, "..");
const sharp = createRequire(join(site, "package.json"))("sharp");

// lib/mark.ts holds the shapes; read them without a TypeScript build.
const source = await readFile(join(site, "lib", "mark.ts"), "utf8");
const field = (name) => source.match(new RegExp(`${name}: "([^"]+)"`))[1];
const MARK = {
  body: field("body"),
  fins: field("fins"),
  flame: field("flame"),
  flameCore: field("flameCore"),
  green: field("green"),
  amber: field("amber"),
  core: { cx: 16, cy: 12.5, r: 2.7 },
};

const shapes = `
  <g stroke-linecap="round" stroke-linejoin="round">
    <path d="${MARK.flame}" fill="${MARK.amber}" fill-opacity=".4" stroke="${MARK.amber}" stroke-width="2"/>
    <path d="${MARK.flameCore}" fill="#fbd38a"/>
    <path d="${MARK.body}" fill="${MARK.green}" fill-opacity=".2" stroke="${MARK.green}" stroke-width="2.4"/>
    <path d="${MARK.fins}" stroke="${MARK.green}" stroke-width="2.2"/>
    <circle cx="${MARK.core.cx}" cy="${MARK.core.cy}" r="${MARK.core.r}" fill="${MARK.amber}"/>
  </g>`;

/** The mark at `scale` of the square, on the page colour or transparent. */
function svg({ background, scale, radius = 0 }) {
  const bg = background ? `<rect width="32" height="32" rx="${radius}" fill="${background}"/>` : "";
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" fill="none">${bg}
  <g transform="translate(16 16) scale(${scale}) translate(-16 -16)">${shapes}</g></svg>`;
}

const png = (markup, size) => sharp(Buffer.from(markup), { density: 72 * Math.ceil(size / 32) }).resize(size, size).png().toBuffer();

/** An .ico holding PNG images (every current browser and OS reads it). */
function ico(images) {
  const header = Buffer.alloc(6 + images.length * 16);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(({ size, data }, i) => {
    const at = 6 + i * 16;
    header.writeUInt8(size >= 256 ? 0 : size, at);
    header.writeUInt8(size >= 256 ? 0 : size, at + 1);
    header.writeUInt16LE(1, at + 4);
    header.writeUInt16LE(32, at + 6);
    header.writeUInt32LE(data.length, at + 8);
    header.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });
  return Buffer.concat([header, ...images.map((i) => i.data)]);
}

const page = "#0b0d10";
const out = (name) => join(site, "public", name);

// The favicon: the mark alone, as large as the square allows.
const tab = svg({ scale: 1 });
const sizes = [16, 32, 48];
await writeFile(out("favicon.ico"), ico(await Promise.all(sizes.map(async (size) => ({ size, data: await png(tab, size) })))));

// Home screens and app icons: the mark on the page colour. iOS rounds the
// corners itself; the maskable icon keeps the mark inside the safe circle.
await writeFile(out("apple-touch-icon.png"), await png(svg({ background: page, scale: 0.78 }), 180));
await writeFile(out("icon-192.png"), await png(svg({ background: page, scale: 0.8, radius: 7 }), 192));
await writeFile(out("icon-512.png"), await png(svg({ background: page, scale: 0.8, radius: 7 }), 512));
await writeFile(out("icon-maskable-512.png"), await png(svg({ background: page, scale: 0.62 }), 512));

// The plain mark, for anyone who links to it.
await writeFile(
  out("mark.svg"),
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" fill="none" role="img" aria-label="Criogen">${shapes}\n</svg>\n`,
);
console.log("icons: favicon.ico, apple-touch-icon.png, icon-192.png, icon-512.png, icon-maskable-512.png, mark.svg");
