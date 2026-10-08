# Scope (draft, input for the design spec)

Status: agreed in brainstorming 2026-09-24. Architecture not decided yet (pending `docs/research/architecture.md`).

## Product

EU-focused e-commerce SaaS (Shopify competitor). Launch market CZ + SK; architecture ready for the whole EU.
Target: small/medium B2C shops first, B2B in v2, larger shops later.
Differentiators: extremely fast mobile storefront, AI-customizable storefront, EU/CZ/SK-native (payments, carriers, feeds, law), agent-ready.

Builder: solo, almost full-time, ASAP. Onboards own clients/contacts; no public self-signup in v1.
Business model: undecided. Architecture must support subscription tiers, Stripe Connect application fee and % of revenue (billing not automated in v1).

## Stack preferences (to be confirmed by architecture research)

Rust core. SolidJS (fine-grained reactivity, no VDOM) for storefront and admin. Storefront theme code must be something AI one-shots reliably (TSX/Solid). TypeScript, pnpm, Tailwind. Cloudflare where it fits. Monorepo (cargo + pnpm workspaces).

## Performance budget (acceptance criteria, mobile-first)

Measured on mid-range Android (Moto G class), throttled 4G, p75 real users (CrUX/RUM):

- LCP < 1.5 s target (lab gate: fails above 2.0 s, warns above 1.5 s), INP < 100 ms, CLS < 0.05
- JS <= 30 kB gz on category and product pages
- 0 third-party scripts in the browser (tracking is server-side)
- Speculation Rules prerender + View Transitions for instant navigation
- AVIF, responsive image sizes, priority hint on LCP image
- Enforced in CI (Lighthouse/WebPageTest budgets). AI theme edits must pass the same performance and accessibility budget before publish.

## v1

Delivered in milestones:

- **M1: first pilot client live.** Catalog, search, checkout + payments, Packeta, invoices, default theme, import, analytics, feeds, plus everything in "Platform".
- **M2: marketing.** Newsletter, abandoned cart, watchdog, recommendations, reviews, ad-platform tracking.
- **M3: AI.** AI theme editing + AI admin helpers. Spike AI theme editing early, in the first weeks, to de-risk the differentiator.

### Catalog

- Categories, products, variants, stock
- GPSR product-safety fields
- Omnibus 30-day lowest price history for discounts
- Unit price (per kg / l)

### Search

Fulltext + facets, handling CZ/SK diacritics, stemming and typos.

### Checkout and payments

- One-page checkout. Guest checkout + optional account (magic link / password).
- Stripe via Connect: direct charges, Standard-like accounts (Stripe carries losses), optional application fee.
- Bank transfer with QR (SPAYD / Pay by square) + automatic payment matching.
- Cash on delivery.

### Shipping

Packeta (pickup points + home delivery) + one courier (PPL or DPD). Labels via carrier API.

### Orders

- Statuses, manual orders and order edits in admin, packing slips
- Refunds, withdrawal flow
- Built-in invoices + credit notes (CZ/SK requirements), VAT incl. OSS

### Markets

One store, multiple domains/countries (shop.cz + shop.sk), each with its own currency (CZK/EUR), language (cs/sk/en), prices and VAT.

### Marketing

- Sales and coupons
- Abandoned cart flows
- Watchdog (back in stock, price drop)
- Built-in newsletter: own sending via EU ESP, segments, personalized content

### Recommendations

Bestsellers, bought together, seasonal, recently viewed, personalization by browsing/purchase history.

### Tracking and analytics

- Own server-side analytics
- Server-side Meta CAPI, GA4, Google Ads enhanced conversions, Sklik
- Consent-aware throughout

### Reviews

Built-in product reviews from verified buyers, with Omnibus disclosure.

### Feeds

Google Merchant, Heureka, Zboží.

### Import

- Products, categories, images, variants from any platform via Heureka / Google Merchant XML feed
- Customers and orders via CSV with column mapping
- 301 redirects from old URLs

### Content and SEO

- CMS pages, simple blog, legal page templates, menus
- Sitemaps, hreflang, canonicals, redirect management

### Emails

Transactional emails (order, shipment + tracking, invoice). DKIM/SPF setup on merchant domain.

### Images

Upload → resize → AVIF/WebP → CDN.

### Themes and conversion

- One very polished default theme
- Free-shipping bar, cart cross-sell, trust elements, express checkout

### AI

- Storefront editing by prompt with preview and rollback
- Admin helpers: product descriptions, translations, bulk edits, SEO texts
- AI Act transparency for AI features

### Agent-ready (cheap version)

- schema.org JSON-LD
- Public storefront API with an OpenAPI spec
- llms.txt

No MCP server.

### Admin

SolidJS SPA, cs/sk/en. Staff accounts with basic roles, audit log.

### Platform

- Multi-tenant, custom domains + TLS
- Webhooks + admin API (needed for v2 integrations)
- Accessible-by-default storefront (EAA), GDPR consent
- Backups, merchant data export, error tracking, uptime monitoring, status page

## v2

- **B2B:** price lists per customer/company, pay by invoice (net terms) + VIES validation, quick order / repeat order / CSV, company accounts with multiple buyers + approvals. The v1 data model must stay B2B-ready (customer groups, price lists as a concept).
- **AI:** chat assistant for merchants.
- **Agent commerce:** ACP / UCP adapters, MCP only if it makes sense, auth.md (WorkOS, May 2026).
- **Payments:** Comgate / GoPay.
- **Shipping:** more carriers (DPD/PPL/GLS/Balíkovna).
- **Integrations:**
  - Ecomail sync
  - Fakturoid / iDoklad / SuperFaktúra, Pohoda / Money export
  - Heureka Ověřeno zákazníky
  - Shopify / Woo API importers
- **Compliance (Jan 2027):** CZ EET 2.0, SK B2B e-invoicing. May be pulled into v1 depending on the final law.
- **Business:** self-signup + automated billing.
- **Storefront:** social login, 3-5 vertical starter themes.
- **Marketing:** ML/embedding recommendations, A/B testing, gift cards, loyalty.
- **Catalog:** digital products, subscription products, multiple warehouses.

## Later

- Third-party module marketplace (WASM sandbox)
- Poland (BLIK / P24)
- Broader EU rollout
- Larger shops: ERP sync, big catalogs

## Non-code prerequisites

- Platform terms of service
- GDPR DPA with every merchant, sub-processor list
- Legal entity
- Name, brand, domain
