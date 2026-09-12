// Render the marketplace icon from the site's favicon.
//
//     node scripts/make-icon.mjs
//
// **One source, two outputs.** The site's `favicon.svg` is the mark; this
// produces the 128x128 PNG the VS Code marketplace requires, because a gallery
// tile is a raster and an SVG is not accepted there. Generated rather than
// hand-drawn so the extension cannot drift from the site's own logo.
//
// The favicon draws on transparency and is read against a dark page. A
// marketplace tile sits on whatever background the gallery uses -- white in the
// light theme -- and the dark bar in the mark would vanish into it, so the
// canvas is filled with the site's own background colour first.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const here = dirname(fileURLToPath(import.meta.url));
const source = join(here, "..", "..", "..", "website", "public", "favicon.svg");
const out = join(here, "..", "icon.png");

// `#0b1120` is the site's page background, so the tile reads as a piece of the
// same design rather than as a sticker on top of one.
//
// **The bar is recoloured, and that is not a liberty.** In the favicon it is
// `#172033` -- a near-black upright read against the site's own dark page,
// where it is a quiet counterweight to the chevron. On a 128px tile at the
// same colour it disappears into the background entirely and the mark reads as
// a lone `>`, which is a different logo. It is lifted to the slate the site
// uses for muted text, so the shape survives being small.
const BACKGROUND = { r: 11, g: 17, b: 32, alpha: 1 };
const BAR_ON_DARK = "#334867";
const SIZE = 128;
// A little air around the mark: the favicon fills its viewBox to the edge,
// which looks cramped once the gallery rounds the corners.
const PADDING = 10;

const svg = readFileSync(source, "utf8").replace('fill="#172033"', `fill="${BAR_ON_DARK}"`);

const mark = await sharp(Buffer.from(svg), { density: 384 })
  .resize(SIZE - PADDING * 2, SIZE - PADDING * 2, {
    fit: "contain",
    background: { r: 0, g: 0, b: 0, alpha: 0 },
  })
  .png()
  .toBuffer();

const icon = await sharp({
  create: { width: SIZE, height: SIZE, channels: 4, background: BACKGROUND },
})
  .composite([{ input: mark, top: PADDING, left: PADDING }])
  .png()
  .toBuffer();

mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, icon);

const { width, height, format } = await sharp(icon).metadata();
console.log(`wrote ${out}: ${width}x${height} ${format}, ${icon.length} bytes`);
