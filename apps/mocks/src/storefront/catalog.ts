/**
 * Deterministic demo catalog for the stub Storefront API (WP2). Realistic enough for the
 * performance probe: 3 categories, 30 products, size × color variants, Omnibus reductions.
 */

export interface Market {
  id: string;
  locale: string;
  currency: "CZK" | "EUR";
  /** Minor units per CZK minor unit (EUR prices are derived, rounded to 10 cents). */
  rate: number;
}

export const MARKETS: Record<string, Market> = {
  "m-cz": { id: "m-cz", locale: "cs-CZ", currency: "CZK", rate: 1 },
  "m-sk": { id: "m-sk", locale: "sk-SK", currency: "EUR", rate: 1 / 25 },
};

export interface ImageRef {
  key: string;
  alt: string;
}

export interface VariantData {
  id: string;
  sku: string;
  options: Record<string, string>;
  stock: "in_stock" | "low_stock" | "out_of_stock";
  image_index: number;
}

export interface ProductData {
  id: string;
  slug: string;
  name: string;
  brand: string;
  category: string;
  /** Base price in CZK minor units. */
  price: number;
  /** Omnibus reference price (lowest 30-day price before the reduction), CZK minor units. */
  reference: number | null;
  images: ImageRef[];
  sizes: string[];
  colors: string[];
  variants: VariantData[];
  created: number;
}

export const CATEGORIES = [
  {
    id: "cat-trika",
    slug: "trika",
    name: "Trička",
    description: "Trička z organické bavlny, šitá v Česku.",
  },
  { id: "cat-mikiny", slug: "mikiny", name: "Mikiny", description: "Teplé mikiny s kapucí i bez." },
  {
    id: "cat-doplnky",
    slug: "doplnky",
    name: "Doplňky",
    description: "Kšiltovky, tašky a drobnosti.",
  },
];

const PHOTOS: Record<string, string[]> = {
  trika: ["tee-sand", "tee-forest", "tee-ink"],
  mikiny: ["hoodie-clay", "hoodie-ash"],
  doplnky: ["cap-olive", "bag-natural"],
};

const NAMES: Record<string, string[]> = {
  trika: [
    "Tričko Basic",
    "Tričko Heavy",
    "Tričko Oversize",
    "Tričko Pocket",
    "Tričko Raglan",
    "Tričko Henley",
    "Tričko Merino",
    "Tričko Stripe",
    "Tričko Linen",
    "Tričko Boxy",
    "Tričko Slim",
    "Tričko Long",
    "Tričko Crop",
    "Tričko Vintage",
    "Tričko Sport",
    "Tričko Kids",
    "Tričko Logo",
    "Tričko Plain",
    "Tričko Relax",
    "Tričko Classic",
    "Tričko Tall",
    "Tričko Organic",
    "Tričko Hemp",
    "Tričko Travel",
  ],
  mikiny: ["Mikina Hood", "Mikina Crew", "Mikina Zip", "Mikina Fleece"],
  doplnky: ["Kšiltovka Six", "Taška Tote"],
};

const COLORS: Record<string, string> = {
  "tee-sand": "Písková",
  "tee-forest": "Lesní zelená",
  "tee-ink": "Inkoustová",
  "hoodie-clay": "Cihlová",
  "hoodie-ash": "Popelavá",
  "cap-olive": "Olivová",
  "bag-natural": "Přírodní",
};

const slugify = (s: string) =>
  s
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/(^-|-$)/g, "");

function build(): ProductData[] {
  const out: ProductData[] = [];
  let n = 0;
  for (const cat of CATEGORIES) {
    const photos = PHOTOS[cat.slug] ?? [];
    for (const [i, name] of (NAMES[cat.slug] ?? []).entries()) {
      n++;
      const colors = [0, 1, 2]
        .map((k) => photos[(i + k) % photos.length] ?? "")
        .filter((v, idx, a) => a.indexOf(v) === idx);
      const sizes = cat.slug === "doplnky" ? ["UNI"] : ["XS", "S", "M", "L", "XL"];
      const price =
        (cat.slug === "mikiny" ? 1490 : cat.slug === "doplnky" ? 590 : 490) * 100 + (i % 4) * 5000;
      const id = `prod-${String(n).padStart(3, "0")}`;
      const variants: VariantData[] = [];
      colors.forEach((photo, ci) => {
        for (const [si, size] of sizes.entries()) {
          variants.push({
            id: `${id}-${ci}${si}`,
            sku: `${cat.slug.slice(0, 3).toUpperCase()}-${n}-${ci}${si}`,
            options: { Barva: COLORS[photo] ?? photo, Velikost: size },
            stock:
              (ci + si + i) % 7 === 0
                ? "out_of_stock"
                : (ci + si) % 5 === 0
                  ? "low_stock"
                  : "in_stock",
            image_index: ci,
          });
        }
      });
      out.push({
        id,
        slug: slugify(name),
        name,
        brand: "Lnen & Co.",
        category: cat.slug,
        price,
        reference: i % 3 === 0 ? price + 20000 : null,
        images: [...colors, ...photos.filter((p) => !colors.includes(p))].map((key, k) => ({
          key: `p/${key}`,
          alt: `${name} – ${COLORS[key] ?? key}, pohled ${k + 1}`,
        })),
        sizes,
        colors: colors.map((c) => COLORS[c] ?? c),
        variants,
        created: n,
      });
    }
  }
  return out;
}

export const PRODUCTS = build();
export const productBySlug = new Map(PRODUCTS.map((p) => [p.slug, p]));
export const variantById = new Map(
  PRODUCTS.flatMap((p) => p.variants.map((v) => [v.id, { product: p, variant: v }] as const)),
);
