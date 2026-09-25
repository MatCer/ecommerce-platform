/**
 * Generates the synthetic product photos used by the stub Storefront API and the performance
 * probe: `fixtures/media/p/<name>/<width>.avif`. Grain is added on purpose so the files weigh
 * about as much as real photos (a flat SVG would make LCP numbers look better than reality).
 *
 *   node apps/mocks/scripts/gen-media.ts
 */
import { mkdir } from "node:fs/promises";
import path from "node:path";
import sharp from "sharp";

const OUT = path.resolve(import.meta.dirname, "../../../fixtures/media");
const WIDTHS = [360, 480, 720, 1080];
const W = 1080;
const H = 1350; // 4:5 product photo

// name → [background, garment color, shape]
const PHOTOS: Record<string, [string, string, "tee" | "hoodie" | "cap" | "bag"]> = {
  "tee-sand": ["#efe9df", "#c9a978", "tee"],
  "tee-forest": ["#e8ece6", "#2f4a3a", "tee"],
  "tee-ink": ["#e9e9ee", "#23263a", "tee"],
  "hoodie-clay": ["#f1e7e1", "#b5643f", "hoodie"],
  "hoodie-ash": ["#ececec", "#8d8f93", "hoodie"],
  "cap-olive": ["#eceee4", "#6b6f3a", "cap"],
  "bag-natural": ["#f3efe6", "#d8c7a6", "bag"],
  hero: ["#e7e0d4", "#3b5446", "hoodie"],
};

const SHAPES = {
  tee: "M300 330 L430 270 Q540 330 650 270 L780 330 L900 520 L790 580 L760 520 L760 1080 L320 1080 L320 520 L290 580 L180 520 Z",
  hoodie:
    "M300 360 Q400 250 470 250 Q540 360 610 250 Q680 250 780 360 L900 700 L800 740 L760 600 L760 1110 L320 1110 L320 600 L280 740 L180 700 Z",
  cap: "M260 760 Q260 480 540 470 Q820 480 820 760 Z M180 760 L900 760 Q900 830 820 830 L260 830 Q180 830 180 760 Z",
  bag: "M300 520 L780 520 L820 1100 L260 1100 Z M420 520 Q420 330 540 330 Q660 330 660 520 L620 520 Q620 380 540 380 Q460 380 460 520 Z",
};

async function photo(name: string, [bg, fg, shape]: (typeof PHOTOS)[string]) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">
    <defs><radialGradient id="g" cx="50%" cy="40%" r="75%"><stop offset="0" stop-color="#fff" stop-opacity=".55"/><stop offset="1" stop-color="${bg}" stop-opacity="0"/></radialGradient></defs>
    <rect width="100%" height="100%" fill="${bg}"/><rect width="100%" height="100%" fill="url(#g)"/>
    <ellipse cx="540" cy="1170" rx="360" ry="40" fill="#000" opacity=".12"/>
    <path d="${SHAPES[shape]}" fill="${fg}"/>
    <path d="${SHAPES[shape]}" fill="#fff" opacity=".08" transform="translate(12 -10)"/>
  </svg>`;
  const noise = await sharp({
    create: { width: W, height: H, channels: 3, noise: { type: "gaussian", mean: 128, sigma: 50 } },
  })
    .png()
    .toBuffer();
  const base = await sharp(Buffer.from(svg))
    .composite([{ input: noise, blend: "soft-light" }])
    .blur(0.4)
    .png()
    .toBuffer();
  const dir = path.join(OUT, "p", name);
  await mkdir(dir, { recursive: true });
  for (const w of WIDTHS) {
    await sharp(base)
      .resize(w)
      .avif({ quality: 60, effort: 4 })
      .toFile(path.join(dir, `${w}.avif`));
  }
}

for (const [name, spec] of Object.entries(PHOTOS)) await photo(name, spec);
console.log(`wrote ${Object.keys(PHOTOS).length} photos × ${WIDTHS.length} widths to ${OUT}`);
