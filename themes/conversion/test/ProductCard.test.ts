import type { ProductCard as Card, ShopModel } from "@platform/storefront-sdk/types";
import { experimental_AstroContainer as AstroContainer } from "astro/container";
import { expect, test } from "vitest";
import ProductCard from "../src/components/ProductCard.astro";

const shop = {
  base_path: "",
  locale: "cs",
  messages: { "product.in_stock": "Skladem", "product.colors": "Barvy: {names}" },
} as unknown as ShopModel;

const image = (n: number) => ({
  src: `/media/photo-${n}-640.jpg`,
  srcset: `/media/photo-${n}-640.avif 640w`,
  srcset_webp: `/media/photo-${n}-640.webp 640w`,
  srcset_fallback: `/media/photo-${n}-640.jpg 640w`,
  width: 640,
  height: 800,
  alt: `Foto ${n}`,
});

const preSwatchCard = {
  id: "00000000-0000-0000-0000-000000000001",
  slug: "tricko",
  name: "Tričko",
  images: [image(1), image(2)],
  price: { amount: 49900, currency: "CZK", formatted: "499 Kč" },
  price_varies: false,
  stock: "in_stock",
  badges: [],
};

async function render(product: object) {
  const container = await AstroContainer.create();
  return container.renderToString(ProductCard, { props: { product: product as Card, shop } });
}

test("renders a card from the pre-swatch API shape without a colour row", async () => {
  const html = await render(preSwatchCard);
  expect(html).toContain('href="/p/tricko"');
  expect(html).toContain("Skladem");
  expect(html).not.toContain("Barvy:");
  expect(html.match(/<img [^>]*photo-\d-640\.jpg/g)?.length).toBe(3);
});

test("renders a card with no images at all", async () => {
  const html = await render({ ...preSwatchCard, images: undefined });
  expect(html).toContain("Tričko");
  expect(html).not.toContain("<img");
});

test("names every colour and counts the ones past four dots", async () => {
  const swatches = ["Červená", "Modrá", "Zelená", "Černá", "Bílá"].map((name) => ({
    name,
    color: "#000000",
  }));
  const html = await render({ ...preSwatchCard, swatches });
  expect(html).toContain("Barvy: Červená, Modrá, Zelená, Černá, Bílá");
  expect(html.match(/<circle /g)?.length).toBe(4);
  expect(html).toContain("+1</span>");
});

test("loads rail photos after the first one lazily and at low priority", async () => {
  const html = await render({ ...preSwatchCard, images: [image(1), image(2), image(3)] });
  const slide = html.match(/<img [^>]*photo-3-640\.jpg[^>]*>/)?.[0];
  expect(slide).toContain('loading="lazy"');
  expect(slide).toContain('fetchpriority="low"');
  expect(slide).toContain('decoding="async"');
});
