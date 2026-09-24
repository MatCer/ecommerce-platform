**Architecture decision report — 24 September 2026**

**Recommendation:** build a Rust modular monolith using **axum + SQLx + managed EU Postgres**, with **one shared Astro storefront using Solid islands** on Cloudflare Workers. Let AI edit a **versioned, constrained theme schema** composed from trusted components. Use Postgres-backed jobs, Meilisearch provisionally, and Better Auth in a small EU-hosted TypeScript service.

The central product decision is the customization contract. **Reliable one-shot generation of arbitrary storefront code is not an established capability.** No verified evidence supports guaranteeing it for SolidStart, Astro, templates, or Leptos. Constrained generation makes reliability measurable and substantially reduces security and deployment complexity.

Documentation-backed capabilities are cited below. Performance rankings are architectural judgments, not benchmark results. Prices are verified where identified; budgets are estimates. Unverified compatibility and legal details are explicitly marked.

**1. Core API and data access**

| Framework | Advantages | Costs and limitations |
|---|---|---|
| **axum** | Explicit composition; Tokio/Tower ecosystem; suitable for a modular monolith and separate worker executable. | You assemble migrations, jobs, configuration and application conventions. |
| **actix-web** | Mature, capable HTTP framework; reasonable alternative for an experienced Actix developer. | Different middleware conventions; switching offers no demonstrated business advantage here. |
| **Loco** | Integrated application conventions and generators; potentially faster CRUD development. | Additional framework conventions and upgrade coupling; less attractive if you replace its preferred persistence/auth patterns. |

Verified documentation currently exposes **axum 0.8.9** and **actix-web 4.15.0**. Loco’s documentation states that **1.0 shipped in July 2026** and identifies Axum and SeaORM as foundations. It should no longer be dismissed simply as a pre-1.0 project. [axum](https://docs.rs/axum/0.8.9/axum/), [actix-web](https://docs.rs/actix-web/4.15.0/actix_web/), [Loco FAQ](https://loco.rs/docs/resources/faq/).

| Data layer | Advantages | Costs and limitations |
|---|---|---|
| **SQLx** | Direct SQL; clear transactions; good fit for RLS, price calculations, reporting and queue operations. | More explicit mapping and query maintenance; dynamic queries need their own verification. |
| **SeaORM** | Convenient entity CRUD, relationships and migrations. | ORM conventions add another abstraction around complex commerce queries. |
| **Diesel** | Strong typed query construction; attractive when its query DSL suits the application. | Learning/compile-time costs; async integration adds a choice to maintain. |

SQLx documentation currently exposes **0.9.0**. SeaORM documents an async ORM; Diesel emphasizes typed query construction. Exact current SeaORM/Diesel release pins were not verified. [SQLx](https://docs.rs/sqlx/0.9.0/sqlx/), [SeaORM](https://www.sea-ql.org/SeaORM/), [Diesel](https://diesel.rs/).

**Decision: axum + SQLx. Confidence: high.** Use explicit SQL and application services; avoid a generic repository abstraction over every table.

**Change the decision if:** a short Loco prototype demonstrably removes substantial work without replacing its conventions, or you already have considerably greater Actix/Diesel expertise. HTTP microbenchmarks would not change this recommendation.

**2. Storefront rendering: the key fork**

| Architecture | Performance and caching | AI, safety and deployment | Assessment |
|---|---|---|---|
| **A. SolidStart SSR on Workers; merchant-specific code** | SSR supports early HTML. Hydration/router code depends on composition. Cache misses still wait for the Rust API. | Natural TSX authoring, but unrestricted code requires isolated builds and execution. Each code revision needs an artifact and deployment. | Choose when unrestricted Solid application customization is essential. |
| **B. Rust templates + Solid islands** | Very small JS is achievable. Rendering beside Postgres avoids an edge-to-origin API round trip. CDN hits can be equally fast. | MiniJinja supports runtime templates; Askama compiles templates with the application. You must own island integration and template restrictions. | Strong simplicity alternative when customization is mostly HTML/layout. |
| **C. Leptos** | SSR and islands are available; interactive components introduce Wasm download/initialization costs that must be measured. | Rust throughout, but not TSX. More compilation/toolchain work per generated theme; AI reliability unverified. | Poor fit for this owner’s theme-authoring priorities. |
| **D. Astro + Solid islands** | Explicitly limits hydration to interactive components. Public product/category HTML is cacheable. Edge misses still need origin data. | Trusted TSX components, one shared deployment, configuration-only merchant edits. Arbitrary Astro/TSX remains executable code. | **Best v1 fit.** |

Current sources verify **SolidStart 2.0.5**, with v2 deployment through Vite deployment plugins including Cloudflare. **Astro 6.0** shipped on 10 March 2026; its documentation provides Cloudflare rendering and the Solid integration, currently documented as **7.0.2**. Leptos documents islands. These are supported capabilities, not a verified compatibility matrix for a complete commerce application. [SolidStart releases](https://github.com/solidjs/solid-start/releases), [SolidStart v2 deployment](https://docs.solidjs.com/solid-start/v2/guides/deployment-plugins), [Astro 6](https://astro.build/blog/astro-6/), [Astro Solid integration](https://docs.astro.build/en/guides/integrations-guide/solid-js/), [Leptos islands](https://book.leptos.dev/islands.html).

**Decision: D, using a shared renderer and constrained themes. Confidence: medium-high.** Keep Astro route/layout files platform-owned. Write reusable visual sections and interactive controls in Solid TSX. Render informational sections without hydration; hydrate variant selectors, cart controls and genuinely interactive widgets.

This preserves most of the Solid preference while making minimal JS an architectural default. It does **not** deliver unrestricted merchant-authored TSX in v1; that is a deliberate product boundary.

For option B, **MiniJinja 2.24.0** is better suited to runtime merchant templates than **Askama 0.16.1**, whose templates compile into Rust. Neither HTML escaping nor compile-time checking establishes safe execution of arbitrary merchant content. Also, Solid cannot simply hydrate arbitrary Jinja-generated HTML: the interactive subtree must have compatible Solid-rendered markup, or mount separately. [MiniJinja](https://docs.rs/minijinja/2.24.0/minijinja/), [Askama](https://docs.rs/askama/0.16.1/askama/).

**Performance contract**

Use these as initial acceptance targets, not forecasts:

- CZ/SK public-page warm-cache TTFB: p75 below **150 ms**.
- Uncached public-page TTFB: p75 below **500 ms**.
- Mobile field performance: LCP below **2.5 s**, INP below **200 ms**.
- Initial product-page application JS: target **≤50 KB compressed**, measured with actual controls and consent tooling.
- Product information, price, primary image and navigation available in initial HTML.

Fine-grained reactivity alone does not deliver these results. Image sizing, font loading, cache hit rate, API waterfalls and third-party scripts are likely to matter more.

**Caching and personalization**

Cache only explicit public GET routes. A cache identity should include the canonical tenant, normalized path/query, market, locale, currency, public price-list identity and published theme revision.

Personalized recommendations load separately. Customer-specific B2B prices, carts, accounts and checkout use private responses. Never put a customer-specific price into shared HTML.

Start with short, bounded public TTLs and publish-triggered invalidation. Treat stock and prices in cached HTML as display data; checkout always recalculates authoritative availability and totals. Cloudflare’s Cache API is local to a data center, and `cache.delete` is not a global purge. Its `cache.put` path does not provide Tiered Cache. [Cloudflare Cache API](https://developers.cloudflare.com/workers/runtime-apis/cache/).

**i18n, currency and SEO**

All four choices can support these; none solves commerce semantics automatically:

- Separate language from market, currency, tax mode and price list.
- Use locale-specific URLs, translated slugs, canonical URLs, `hreflang`, sitemaps and server-rendered structured data.
- Keep arbitrary facet combinations out of the index; provide curated indexable category/landing pages.
- Calculate monetary values and tax in Rust; store currency and rounding rules with order snapshots.
- Give the renderer a complete page view model in one API call.

**Change the decision if:** customization studies show that the schema cannot express the layouts merchants actually purchase. Then trial SolidStart on Workers for Platforms. If Astro adds more complexity than its island tooling saves, use Rust/MiniJinja with trusted Solid widgets.

**3. AI theme/customization model**

| Model | Strength | Weakness |
|---|---|---|
| **Configuration only** | Easiest to validate; no code execution or merchant builds. | Quickly feels restrictive. |
| **Sections/blocks + design tokens** | Useful composition freedom with enforceable contracts. | Requires an intentionally designed component library. |
| **Arbitrary components/templates** | Maximum flexibility. | Hardest safety, compatibility, accessibility and one-shot reliability problem. |

**Decision: sections/blocks + tokens. Confidence: high for safety and operations; medium for merchant satisfaction.**

Borrow Shopify’s separation of JSON composition from section implementation. Shopify documents JSON templates containing section references and configurable blocks; this pattern is useful independently of Liquid. [JSON templates](https://shopify.dev/docs/storefronts/themes/architecture/templates/json-templates), [sections](https://shopify.dev/docs/storefronts/themes/architecture/sections).

**Exactly what the AI edits**

A typed theme document containing:

- Page-to-template assignments and ordered section/block instances.
- Approved component identifiers and validated props.
- Typography, spacing, colors, borders and layout tokens.
- Asset references and locale-keyed copy.
- Bounded catalog references such as category IDs or collection IDs.

The AI cannot edit checkout, authentication, API authorization, pricing, scripts, dependencies, arbitrary URLs or database queries. Avoid turning the schema into a programming language with expressions and unrestricted loops.

Use trusted Tailwind classes in component implementations and CSS variables for merchant tokens. Runtime-generated utility strings otherwise create build/extraction complications.

**Publication flow**

1. Generate a patch against a specific theme revision.
2. Validate schema, component references, assets, URLs, nesting and size limits.
3. Render representative products, empty categories, long Czech/Slovak text and missing-image cases.
4. Run accessibility, interaction and visual checks.
5. Publish an immutable theme manifest and atomically update the shop’s active revision.
6. Roll back by restoring the previous manifest pointer.

Preview runs on a separate origin without production customer/admin cookies. Use fixtures or limited catalog access, `noindex`, no public caching and non-production checkout behavior. Retain the renderer/component version needed by each revision; otherwise “rollback” can silently change appearance.

**If arbitrary code arrives later**

Workers for Platforms explicitly supports untrusted customer/AI code in isolated Workers. Still restrict bindings, egress, CPU and tenant-scoped API access. Build in an ephemeral environment without production credentials or unrestricted package installation. [Workers for Platforms](https://developers.cloudflare.com/cloudflare-for-platforms/workers-for-platforms/).

Server isolation does **not** prevent generated browser JavaScript from reading page data or acting as the customer on that storefront. Protect checkout/account surfaces separately; static analysis and CSP are supplementary controls.

**Change the decision if:** measured merchant demand requires new behavior rather than layout composition, and the business can fund a secure code-execution product. Until then, add trusted section capabilities based on repeated demand.

**4. Multi-tenancy and data**

| Model | Advantages | Drawbacks |
|---|---|---|
| **Shared schema + tenant_id + RLS** | One migration stream; efficient pooling; lowest operating cost. | Requires rigorous tenant context, constraints and noisy-neighbor controls. |
| **Schema per tenant** | Namespace separation; some customization flexibility. | Migration/object proliferation; shared failure domain remains; awkward pooling/search paths. |
| **Database per tenant** | Stronger operational isolation and independent restores. | Provisioning, migrations, monitoring and connection overhead. |

**Decision: shared schema, with dedicated databases available later. Confidence: high.**

Every tenant-owned table has `tenant_id`. Tenant-scoped unique constraints and foreign keys include it. Set tenant context transaction-locally on pooled connections; missing context must fail closed.

Runtime roles must not own tables or have `BYPASSRLS`; migration roles are separate. Use RLS read/write policies and test them with the actual runtime role. PostgreSQL documents owner/superuser bypass behavior and `FORCE ROW LEVEL SECURITY`. RLS is defense in depth, not protection against a compromised privileged application. [PostgreSQL RLS](https://www.postgresql.org/docs/17/ddl-rowsecurity.html).

Keep authentication tables in a separate schema with separate permissions. Do not expose the operational database directly to browsers.

**Large-shop path**

A shop with 10,000 products does not inherently need a dedicated database. Variant count, order rate, price-list complexity, imports and retention determine load.

Preserve an explicit tenant context in services. Later, a tenant directory can select a database pool or tenant group. Keep tenants free of cross-tenant business foreign keys; implement a tested export/import procedure before selling dedicated isolation.

Add bounded ERP import batches, per-tenant queue fairness, statement timeouts and query budgets before adding shards.

**EU residency**

Keep primary data, backups, search indexes, email processing choices and logs in selected EU regions. Global CDN processing is a separate issue: an EU database does not make the entire system EU-only. Cloudflare documents Regional Services for geographically restricted Worker execution, with trigger-specific limitations. Commercial eligibility and the complete data boundary need verification. [Cloudflare Workers localization](https://developers.cloudflare.com/data-localization/how-to/workers/).

**Change the decision if:** contracts demand independent recovery/isolation, a tenant repeatedly harms shared latency, or data-placement obligations require it. Schema-per-tenant is not the preferred intermediate step.

**5. Search and facets**

| Option | Fit | Main concern |
|---|---|---|
| **Meilisearch** | Convenient typo-tolerant product search, filters and facets; straightforward initial service. | CZ/SK morphology quality and large multi-tenant sizing require tests. |
| **Typesense** | Strong field configuration, facets, typo controls and explicit stemming dictionaries. | Memory/resource sizing and language behavior need benchmarking. |
| **Postgres FTS / ParadeDB** | SQL integration; fewer independently synchronized systems. | Native FTS needs assembled typo/facet behavior; ParadeDB adds extension/provider compatibility and OLTP contention considerations. |
| **Embedded Tantivy** | Rust integration and tokenizer control. | You own serving, persistence, recovery, replication, resource isolation and relevance tooling. |

**Decision: Meilisearch provisionally; benchmark Typesense before committing. Confidence: medium.** Avoid embedded Tantivy for v1.

Current references show **Meilisearch 1.54.0**, **Typesense 30.2** documentation and **Tantivy 0.26.2**. Typesense documents both algorithmic stemming and custom dictionaries. Postgres provides `unaccent` and `pg_trgm`, but those are building blocks rather than a complete commerce search experience. [Meilisearch releases](https://github.com/meilisearch/meilisearch/releases), [Typesense search](https://typesense.org/docs/30.2/api/search.html), [Typesense stemming](https://typesense.org/docs/30.2/api/stemming.html), [Tantivy](https://docs.rs/tantivy/0.26.2/tantivy/), [unaccent](https://www.postgresql.org/docs/17/unaccent.html), [pg_trgm](https://www.postgresql.org/docs/17/pgtrgm.html).

**Important language finding:** Snowball added its merged Czech stemmer in **3.1.0 during 2026**. That does not establish adoption by Typesense, Postgres or a particular Rust dependency. Slovak is absent from the cited Snowball algorithm list. Built-in production-quality Czech **and** Slovak stemming in the shortlisted engines remains **unverified**. [Czech stemmer](https://snowballstem.org/algorithms/czech/stemmer.html), [Snowball algorithms](https://snowballstem.org/algorithms/).

Therefore:

- Preserve original text; test accent-insensitive matching separately.
- Rank exact SKU/EAN/brand matches above fuzzy matches.
- Disable typo/stemming behavior for identifiers where inappropriate.
- Test inflections independently of diacritics and typos.
- If required, add a vetted language normalization/dictionary stage to both indexing and queries. Its coverage and licensing are unverified until selected.

Start with one index per shop and language, allowing independent synonyms and facets. Benchmark the overhead before reaching thousands of indexes. Query through Rust, which chooses the index; never trust a browser-supplied tenant filter.

Index variant relationships carefully: “red” and “XL” must match an actually purchasable variant, not attributes from two different variants. Keep negotiated B2B prices outside public indexes.

Use the transactional outbox to update search; support full rebuilds and versioned index swaps. Search must remain a disposable projection.

**Change the decision if:** Typesense wins the CZ/SK relevance benchmark materially, or ParadeDB passes the same benchmark and runs on the selected managed database without harming checkout latency. ParadeDB’s exact current extension/provider compatibility was not verified.

**6. Recommendations and personalization**

| Option | Advantages | Drawbacks |
|---|---|---|
| **Popularity + merchandising + co-occurrence** | Explainable, inexpensive, useful with limited data. | Limited individual personalization; popularity bias. |
| **Content similarity using pgvector** | Useful for new products and sparse purchase histories. | Embeddings add generation/versioning costs; similarity is not purchase intent. |
| **Dedicated recommender/ranking service** | More sophisticated candidate generation and ranking. | Data, experimentation and operational requirements are premature. |

**Decision: rules and aggregate statistics first; embeddings later. Confidence: high.**

V1 should provide:

- Recent bestsellers per tenant/category/market, with time decay.
- Merchant-scheduled seasonal collections and explicit boosts.
- “Frequently bought together” from qualifying orders, with minimum support.
- Consented category/brand affinity where enough history exists.
- Availability, market, B2B eligibility and merchandising filters on every result.

For newsletters, select a small personalized product set with popularity fallback. Generate campaign copy once per segment rather than invoking an LLM for every recipient.

Add pgvector for content-based alternatives when cold-start recommendations are visibly weak. Begin with exact similarity within a tenant’s modest candidate set. Approximate indexes plus tenant filters need recall tests; pgvector documents filtering limitations and iterative scans introduced in **0.8.0**. [pgvector](https://github.com/pgvector/pgvector).

**Change the decision if:** sufficient traffic supports controlled experiments and the simple approach demonstrably underperforms. Evaluate incremental conversion/revenue, not recommendation clicks alone. Do not pool customer behavior across merchants by default.

**7. Tracking and analytics**

| Storage | Advantages | Drawbacks |
|---|---|---|
| **Postgres partitions + rollups** | Few moving parts; easy joins and deletion workflows. | Event writes and analytical scans can contend with commerce. |
| **Timescale** | Retention and time-series aggregation conveniences. | Extension/provider coupling; not automatically necessary. |
| **Managed ClickHouse in the EU** | Appropriate for large event volumes and analytical scans. | Another bill, schema lifecycle and deletion pipeline. |

**Decision: Postgres for early pilots; managed ClickHouse when measured load justifies it. Confidence: high.** ClickHouse documents EU cloud regions including Frankfurt and Ireland; Timescale documents retention policies. [ClickHouse regions](https://clickhouse.com/docs/cloud/reference/supported-regions), [Timescale retention](https://docs.timescale.com/use-timescale/latest/data-retention/create-a-retention-policy/).

**Pipeline**

Authoritative business mutations produce outbox events in the same transaction. Optional browser behavior goes through a first-party ingestion endpoint. Both produce a versioned envelope containing tenant, event ID, timestamps, type, schema version and relevant consent/purpose metadata.

Deduplicate, batch, enforce payload limits and restrict retention. Keep financial order state in Postgres even after adopting ClickHouse.

A server cannot infer an actual browser interaction from an API call alone. Cached page delivery also bypasses the Rust origin. Use a small consent-aware browser collector where behavioral measurement is required; distinguish bot/CDN requests from human behavior.

**Consent interplay**

Server-side collection is not a general consent exemption. Separate:

- Processing necessary to fulfil an order or secure the service.
- Operational aggregates with appropriately minimized data.
- Behavioral analytics, advertising attribution and personalized marketing.

The latter should be disabled until the applicable permission exists. Store purpose-specific consent evidence; withdrawal must stop subsequent processing. Do not treat hashed email addresses as anonymous. EDPB’s final **Guidelines 2/2023, version 2.0** address the technical scope of ePrivacy Article 5(3). Exact CZ/SK implementation and exemption rules were not verified. [EDPB guidelines](https://www.edpb.europa.eu/documents/guideline/guidelines-22023-on-technical-scope-of-art-53-of-eprivacy-directive_en).

**Change the decision if:** event retention, ingestion or dashboard queries exceed their resource budget or affect transaction latency. Under the 1,000-shop workload below, budget for separate analytics.

**8. Jobs, workflows and email**

| Queue | Advantages | Drawbacks |
|---|---|---|
| **Postgres-backed: PGMQ / Apalis** | Can share transactional boundaries; no separate broker. | Requires backlog/vacuum management; scheduling semantics still belong to the application. |
| **NATS JetStream** | Durable messaging and multiple consumers; useful beyond a monolith. | Additional service and outbox-to-broker delivery boundary. |
| **Redis-backed** | Useful if Redis and a mature job ecosystem already exist. | Another persistence system; plain Pub/Sub is insufficient for durable jobs. |

**Decision: PGMQ with a Rust worker. Confidence: medium-high.**

PGMQ **1.11.1** documents PostgreSQL 14–18 support and a SQL-only installation option. The exact installation on the chosen managed provider must be tested. Apalis is viable, but its current `latest` Postgres documentation resolves to **1.0.0-rc.9**; do not select a release candidate unintentionally. [PGMQ](https://pgmq.github.io/pgmq/1.11.1/), [Apalis Postgres](https://docs.rs/apalis-postgres/latest/apalis_postgres/).

Keep workflow state in business tables. Jobs advance it; jobs are not the sole record of it.

For example, an abandoned-cart job checks that the cart is still abandoned, the customer remains eligible and contact permission remains valid. Watchdogs respond to inventory/price transitions and maintain notification history to prevent repeated sends.

Use retry limits, backoff, leases, dead-letter handling, per-tenant fairness and idempotency keys. PGMQ’s visibility-window guarantee does **not** establish exactly-once email delivery. If a provider accepts a message and the worker crashes before recording success, duplicates remain possible without provider idempotency or reconciliation.

**Email options**

| Provider approach | Best reason to choose | Concern |
|---|---|---|
| **SES in an EU region** | Low marginal cost; platform controls templates and automation. | Domain onboarding, reputation, abuse and suppression handling remain your responsibility. |
| **Brevo** | EU-hosted service and marketing tooling can reduce implementation work. | Verify reseller/multi-tenant terms, required APIs and campaign economics. |

**Recommendation: SES for the built-in sending engine**, provided multi-tenant onboarding is accepted; Brevo if it meaningfully reduces launch work.

SES currently lists **$0.16/1,000 emails** for Essentials in the first volume tier; new-account defaults changed in July 2026. Brevo states that its primary hosting uses France and Germany. Neither establishes that every subprocessor or recipient mailbox is EU-only. [SES pricing](https://aws.amazon.com/ses/pricing/), [Brevo storage locations](https://help.brevo.com/hc/en-us/articles/360001005510-Data-storage-location).

Require merchant-domain verification, DKIM/SPF/DMARC, complaint/bounce processing, unsubscribe handling and sending quotas. Separate transactional and marketing streams.

Treat abandoned-cart promotions and newsletters as marketing by default. Article 13’s existing-customer exception is conditional; do not assume that entering an email at checkout qualifies. Requested watchdog messages need their own explicit subscription semantics. National CZ/SK details remain unverified. [ePrivacy Directive, Article 13](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX%3A32002L0058).

**Change the decision if:** independent services need durable fan-out/replay, or queue load impairs Postgres. Then introduce JetStream through the existing outbox. Change email provider if platform terms, delivery quality or operational workload fail the pilot.

**9. Modules and the eventual marketplace**

| Model | Appropriate use | Main cost |
|---|---|---|
| **Internal Rust modules** | V1 business capabilities with shared transactions. | Boundaries require discipline. |
| **Webhook + API applications** | ERP, fulfilment, marketing and third-party integrations. | Permissions, retries, versioning and developer onboarding. |
| **Wasm extensions** | Bounded synchronous calculations such as specialized discount/shipping rules. | You own runtime security, capability APIs, compatibility and resource limits. |

**Decision: internal modules now; webhook/API apps first; Wasm only for a demonstrated synchronous requirement. Confidence: high.**

Suggested boundaries: catalog, inventory, pricing/promotions, cart/checkout, orders/payments, customers/B2B, content/themes, engagement and integrations.

Use direct typed service calls for synchronous operations and an outbox for asynchronous side effects. Avoid microservices, a general plugin kernel and native dynamic-library loading.

Public extension points should expose versioned commands/events, scoped installation credentials, idempotency and cursor-based synchronization. Apps should not receive database credentials. ERP integrations need explicit ownership rules for inventory, prices and customer records.

Wasmtime provides a sandbox and capability-oriented host access. Extism supplies a plugin model with exported/imported functions; **it should not be assumed interchangeable with a WIT-based Component Model contract**. Verify the exact runtime/ABI if adopting it. [Wasmtime security](https://docs.wasmtime.dev/security.html), [Extism plugins](https://extism.org/docs/concepts/plug-in/).

If introduced, run Wasm with CPU/fuel, memory and deadline limits; expose narrow host functions and no default network/filesystem access. A sandbox cannot make an overprivileged host function safe.

The AI admin assistant should invoke these same authorized commands with audit trails and explicit approval for consequential actions. Agent storefront clients should use scoped APIs and checkout quotes, rather than privileged UI automation.

**Change the decision if:** a real integration needs low-latency synchronous computation that webhooks cannot provide. Do not introduce Wasm simply to advertise a marketplace.

**10. Hosting, operations and cost**

| Hosting approach | Advantages | Drawbacks |
|---|---|---|
| **Cloudflare + Hetzner compute + managed EU Postgres** | Good balance of edge delivery, modest compute cost and outsourced database recovery. | Several vendors; API-to-database latency must be measured. |
| **Cloudflare + entirely self-managed Hetzner** | Lowest infrastructure bill and more control. | Database backups, failover and recovery become the solo developer’s job. |
| **EU-region managed application platform + managed Postgres** | Less OS/deployment maintenance. | Higher cost; region, bandwidth and commercial constraints need verification. |

**Decision: first option. Confidence: medium-high.**

Use:

- One shared Astro Worker plus Cloudflare for SaaS custom hostnames.
- Hetzner EU containers for Rust API, Rust worker and Better Auth.
- Managed Postgres in a nearby EU region; **Neon Frankfurt is the first candidate**.
- Meilisearch on separate modest compute initially, or a managed EU offering if its current terms fit.
- R2 for assets, with EU-jurisdiction configuration where required.
- Fixed image variants generated on upload; consider Cloudflare Images when its convenience outweighs variable delivery/transformation cost.

Neon documents Frankfurt availability. Its exact selected plan, recovery retention, extensions and current total price must be verified at procurement. Supabase is a reasonable alternative, but its $25 entry price excludes important production choices: its documented seven-day PITR add-on is approximately **$100/month**. [Neon regional availability](https://neon.com/blog/category/changelog), [Supabase pricing](https://supabase.com/pricing), [Supabase PITR](https://supabase.com/docs/guides/platform/manage-your-usage/point-in-time-recovery).

Keep production database compute warm; do not optimize checkout around scale-to-zero savings.

**Verified price anchors**

| Item | Current documented price |
|---|---|
| Workers Standard | $5/month minimum; 10M requests and 30M CPU-ms included; usage overages apply. |
| Workers for Platforms | $25/month; 20M requests, 60M CPU-ms and 1,000 scripts included. |
| Cloudflare for SaaS | 100 hostnames included; $0.10/additional hostname/month on listed self-service plans. |
| R2 Standard | $0.015/GB-month plus operations; Internet egress free. |
| Hetzner CX33, Germany/Finland | €8.49/month excluding IPv4 and VAT after June 2026 adjustment. |
| SES Essentials | $0.16/1,000 emails in the first tier. |

Sources: [Workers](https://developers.cloudflare.com/workers/platform/pricing/), [Workers for Platforms](https://developers.cloudflare.com/cloudflare-for-platforms/workers-for-platforms/reference/pricing/), [custom hostnames](https://developers.cloudflare.com/cloudflare-for-platforms/cloudflare-for-saas/plans/), [R2](https://developers.cloudflare.com/r2/pricing/), [Hetzner June 2026 prices](https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/), [SES](https://aws.amazon.com/ses/pricing/).

Workers for Platforms is **not needed** for the recommended shared, configuration-driven renderer. Domain onboarding still needs an apex-domain strategy; do not assume every merchant’s DNS provider supports apex CNAME flattening.

**Illustrative monthly budget**

Assumptions, not forecasts:

| Workload | 10 shops | 1,000 shops |
|---|---:|---:|
| Average products/shop | 1,000 | 1,000 |
| Total products, excluding variants | 10,000 | 1M |
| Sessions/month | 100,000 | 10M |
| Page views/month | 500,000 | 50M |
| Worker invocations, including APIs | 1M | 100M |
| Public HTML cache hit assumption | 90% | 90% |
| Retained behavioral events/month | 200,000 | 20M |
| Emails/month | 20,000 | 2M |
| Asset storage | 100 GB | 2 TB |

Estimated budgets below are **EUR planning allowances**, not vendor quotes or currency conversions.

| Cost category | 10 shops | 1,000 shops |
|---|---:|---:|
| API, worker and auth compute | €20–50 | €150–400 |
| Managed Postgres, recovery and storage | €80–160 | €400–1,200 |
| Search | €20–60 | €250–1,000 |
| Separate analytics | €0–20 | €150–600 |
| Workers | €5–15 | €40–100 |
| Custom hostnames, one per shop | €0 | ≈€90 |
| Assets, operations, image processing | €5–20 | €50–200 |
| Email allowance | €5–30 | €320–600 |
| Monitoring, backups, miscellaneous | €20–60 | €100–300 |
| **Total** | **€155–415** | **€1,650–4,490** |

At 100M Worker invocations averaging 5 ms CPU, the published Standard formula gives approximately **$41.40/month** before other products. At 1,000 hostnames, the additional-hostname line is **$90/month**. Two million SES Essentials emails cost approximately **$320** before add-ons.

Budget separately for AI generation, payment processing, tax, domains, support and development labor. AI expense should have per-merchant quotas and explicit usage pricing. A 1,000-shop platform where every shop has 10,000 products is a **different, 10M-product workload**, before variants and translations.

**Deployment/ops**

Use reproducible containers, automated migrations, health checks and a simple deployment pipeline; Kubernetes is unnecessary. Add a second API instance as uptime requirements justify it. Test database restoration, not merely backup creation.

Proposed initial recovery objectives: **RPO ≤5 minutes, RTO ≤1 hour**. These are acceptance targets requiring a verified provider plan and restore drill, not guarantees.

**Change the decision if:** cross-provider latency hurts uncached requests, strict EU-only processing changes CDN feasibility, or OS operations consume substantial time. Then colocate more infrastructure or pay for managed application hosting.

**11. Repository structure**

| Option | Advantages | Drawbacks |
|---|---|---|
| **Cargo + pnpm monorepo** | Atomic API/client/schema changes; shared CI and fixtures; easiest solo workflow. | Requires clear package boundaries and targeted CI. |
| **Multiple repositories** | Independent ownership and release/access policies. | Contract coordination and duplicated tooling become immediate overhead. |

**Decision: monorepo. Confidence: high.**

Suggested organization:

```text
apps/
  api/                 Rust HTTP executable
  worker/              Rust background executable
  storefront/          Astro + Solid
  admin/               Solid + TypeScript
  auth/                Better Auth service
crates/
  commerce/            Business modules
  infrastructure/      Database and external adapters
packages/
  api-client/          Generated from OpenAPI
  theme-schema/
  storefront-components/
migrations/
infra/
```

Start with Rust modules inside a small number of crates. Avoid a crate for every capability. Keep merchant theme revisions in application storage, not separate Git repositories.

Generate the TypeScript API client from the Rust API contract; use runtime validation at external boundaries. Shared types do not replace authorization or validation.

**Change the decision if:** separate teams, external contributors or access restrictions create real ownership boundaries.

**12. Merchant, staff, customer and agent authentication**

| Option | Advantages | Drawbacks |
|---|---|---|
| **Better Auth in TypeScript** | Matches preference; established session/plugin model; avoids assembling auth features in Rust. | Additional runtime and a cross-service trust boundary. |
| **Rust-native identity provider, e.g. Rauthy** | Rust deployment; OIDC/OAuth identity boundary. | Separate identity product and customer-domain integration work. |
| **Managed identity provider** | Outsources more identity operations. | Cost, residency, customer tenancy and customization constraints. |

**Decision: Better Auth in a small EU-hosted Node/TypeScript service. Confidence: medium-high for merchant/staff auth; medium for multi-domain customer auth until prototyped.**

The release page currently lists **Better Auth 1.7.6**. Its JWT plugin documents tokens and JWKS for external services and explicitly distinguishes them from the primary session cookie. Rauthy documents OIDC/OAuth identity services as a Rust-native alternative. [Better Auth releases](https://github.com/better-auth/better-auth/releases), [JWT plugin](https://better-auth.com/docs/plugins/jwt), [Rauthy](https://github.com/sebadob/rauthy).

**Responsibility split**

- Better Auth owns credentials, authentication sessions, MFA/passkeys and identity flows.
- Rust owns tenant membership, object authorization, B2B company roles and commerce permissions.
- Auth has separate database credentials and schema.
- Rust validates short-lived, audience-restricted tokens using JWKS or uses server-side session introspection. Never trust a browser-supplied identity header.
- Sensitive operations recheck current permissions; cached tokens otherwise delay revocation.

Keep browser sessions in secure HttpOnly cookies. Tokens used between services should not become long-lived local-storage credentials.

**Customer identity needs an explicit product choice.** The same email in two shops should not automatically expose a shared customer account or profile. Use tenant-scoped customer membership and verify Better Auth’s selected account model supports the intended isolation. This exact configuration remains unverified.

Use host-only storefront cookies and server-side routing to the central auth service. Unrelated merchant domains cannot share ordinary cookies. Test magic links, OAuth callbacks, CSRF, verified-domain allowlists and account recovery across custom domains. Platform-admin authentication must stay separate.

For future agents, use an established OAuth implementation with scoped delegation, short expiry and spending/action limits. Keep public browsing unauthenticated; require explicit authority for purchases. Rust remains authoritative for quote expiry, prices, inventory and idempotent order creation.

**Change the decision if:** custom-domain customer isolation becomes brittle, enterprise federation dominates, or managed identity substantially reduces operational burden. Do not build authentication protocols from scratch to keep the backend “all Rust.”

**Recommended v1 reference architecture**

```text
Customer browser / authorized shopping agent
                    |
       Cloudflare DNS, TLS, WAF, SaaS hostnames
                    |
          Shared Astro storefront Worker
          + trusted Solid components/islands
          + public HTML cache
             |                  |
             |                  +---- R2 assets / fixed image variants
             |
      public page models / private commerce requests
             |
        EU Rust API — axum
             |
      Commerce modules + SQLx
             |
       Managed EU Postgres
       ├─ tenant data + RLS
       ├─ orders, inventory, authoritative pricing
       ├─ theme revisions and active pointers
       ├─ consent and workflow state
       ├─ transactional outbox
       └─ PGMQ
             |
        EU Rust worker
       ├─ search projections → Meilisearch
       ├─ recommendations / rollups
       ├─ newsletters and notifications → EU-region email provider
       ├─ ERP/webhook processing
       └─ analytics → Postgres initially; ClickHouse later

Merchant admin — Solid/TS
       |
       +---- EU Better Auth service → isolated auth schema
       |
       +---- Rust API → constrained AI theme generation
                           |
                     validation + preview
                           |
                    immutable theme revision
```

**Principal data flows**

1. **Browse:** verified hostname resolves to tenant; public cache hit returns HTML. A miss obtains one page view model from Rust, renders trusted sections and caches the public result.
2. **Customize:** merchant prompt produces a schema patch; validation and preview precede publication. Routine edits require no per-merchant compilation or Worker deployment.
3. **Purchase:** private cart/checkout calls calculate current prices, tax and inventory. Idempotent order/payment handling commits business state and outbox events. Use a PSP’s hosted payment UI; themes never handle card details.
4. **React to changes:** worker jobs update search, invalidate affected content, maintain recommendation tables and advance notification workflows.
5. **Observe:** authoritative business events and permitted behavioral events remain distinguishable; analytics is eventually consistent and never the order ledger.

**Top five risks**

1. **Customization disappoints merchants.** A safe schema may not express their requests; unrestricted generation may not be reliable. Validate with real prompts before building a large editor.
2. **Tenant or private-price leakage.** Cache keys, auth context, indexes and background jobs all cross isolation boundaries. RLS alone is insufficient.
3. **Commerce correctness and local scope.** VAT/rounding, B2B eligibility, reservations, refunds and ERP reconciliation can consume more effort than the storefront. Keep one authoritative pricing/order model.
4. **CZ/SK search quality.** Diacritic handling can look successful while inflection matching remains poor. Validate linguistic behavior, not vendor language lists.
5. **Solo-dev operational load and variable economics.** Deliverability, consent, restore procedures, AI usage and noisy tenants can overwhelm cheap infrastructure. Meter and constrain them early.

**Five spikes to run first**

| Spike | Prototype | Decision gate |
|---|---|---|
| **1. AI customization reliability** | Use 50 representative CZ/SK merchant requests against the schema model and a constrained TSX alternative. Render and test each output. | Target ≥95% first-attempt validity and separately score visual/functional acceptance. No publishable safety failures. Measure cost and latency; revise the customization promise if it fails. |
| **2. Storefront performance and publishing** | Same 10k-product shop in Astro/Solid and SolidStart; include variants, consent, locale/currency changes and a private B2B price. | Measure warm/cold TTFB, LCP, INP, JS, build time and rollback. Verify cache separation and target global publication convergence under 60 seconds. |
| **3. CZ/SK search bake-off** | Meilisearch and Typesense over real catalogs, using 200–500 judged queries with diacritics, inflections, typos, SKUs and combined variant facets. | Compare nDCG@10, zero-result rate, facet correctness, p95 latency, index lag and RAM. Record exactly which stemmer/dictionary versions ran. |
| **4. Tenant and auth isolation** | Two custom-domain shops, overlapping emails/product IDs, distinct B2B prices, pooled SQL connections and background jobs. | Cross-tenant reads/writes/search/cache access must fail. Verify customer isolation, CSRF, revoked staff access, preview isolation and magic-link routing. |
| **5. Failure and recovery** | Commit order + outbox, crash workers at delivery boundaries, duplicate payment webhooks, interrupt an ERP import, then restore the database. | No lost committed events; idempotent order/payment outcomes; email ambiguity handled explicitly; search rebuild succeeds; measured RPO/RTO meets the chosen service promise. |