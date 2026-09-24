I would not approve this spec for implementation unchanged. The stack is viable; several security boundaries and commerce rules are not yet safe implementation contracts.

1. **[blocker] §5.5, §9.3–9.4, §12.3 — Reserved paths do not isolate checkout from merchant JavaScript.**  
   A theme script can fetch `/account` or `/_p/*` with the browser’s cookies and read responses. HttpOnly prevents reading the cookie itself, not authenticated requests. Same-origin scripts also pass the proposed Origin checks. Cookie stripping before SSR does not address this.

   **Change:** “Untrusted theme JavaScript and platform checkout/account run on different origins. Checkout uses host-only cookies and a short-lived, single-use cart handoff. Credentialed CORS does not permit theme origins.” Until that exists, permit only platform-reviewed theme code.

2. **[blocker] §8.2, §9.3 — Cookie absence is insufficient permission to cache HTML.**  
   A first-time guest can request an order-token confirmation without cookies. Login/magic-link responses and responses setting a cart cookie are also sensitive. The rule currently permits caching these, and does not explicitly reject `Set-Cookie`, `private`, `no-store` or authorization-bearing requests.

   **Change:** Cache only an explicit public-page allowlist. Always bypass checkout, account, platform routes, previews and capability-token URLs. Never cache responses containing `Set-Cookie` or private cache directives. Edge policy overrides theme-provided cache hints. Strip client-supplied routing headers before resolving tenant/market.

3. **[blocker] §7.2, §10.1 — OSS registration incorrectly determines the applicable VAT country.**  
   “When off, origin country rate applies” is wrong. Destination VAT can be owed without OSS registration; OSS is a reporting mechanism. The €10,000 exception has establishment, dispatch-country and current/prior-year conditions. [Commission OSS guidance](https://vat-one-stop-shop.ec.europa.eu/one-stop-shop_en).

   **Change:** Separate VAT liability, place-of-supply rules and reporting registration. M1 supports explicitly configured domestic exemption, eligible origin taxation and destination taxation, with effective dates and merchant/accountant confirmation. Block unsupported destinations. Replace universal `reduced` mappings with country-specific product classifications. Gross-price preservation can remain.

4. **[high] §5.5, §8.2, §10.3 — The private storefront transport and authorization contract is incomplete.**  
   Shop cookies cannot accompany direct browser calls to `api.localhost` as shop cookies. The mini-cart needs private access, but only checkout/account/platform routes forward cookies. Public tokens identify a tenant; they cannot authorize access to a particular cart, customer or order.

   **Change:** Define a trusted browser-facing gateway and endpoint authorization matrix. Public reads require tenant context; cart operations require an opaque cart capability; customer operations require a tenant-bound session; order links require narrowly scoped capabilities. Specify cookie paths, expiry, rotation, `Set-Cookie` propagation and cart-to-account merging.

5. **[high] §5.4, §8.2 — Guest checkout can become an account-takeover path.**  
   “Set a password after the order” does not say what proves ownership of the email. Knowing an order/cart token or entering somebody’s email must not authorize password creation or expose existing order history. Password-reset email is promised, but reset endpoints and semantics are absent.

   **Change:** Require verified-email magic-link authority or recent authenticated reauthentication to set/change credentials. Define recovery, session revocation, single-use atomic token consumption and safe redirect destinations. Guest email matching alone never authenticates or links historical orders.

6. **[high] §9.3, §12.3 — Runtime isolation does not secure the build pipeline.**  
   Astro source and `theme.config.ts` execute during builds. Locked dependencies do not prevent source from accessing the builder’s environment, filesystem or other revisions. A shared Docker service with AI credentials is insufficient isolation.

   **Change:** Build each revision in a disposable, unprivileged sandbox with no credentials, no network, read-only dependencies, a dedicated writable directory and CPU/memory/PID/time limits. Keep the AI controller outside it. Validate archive paths, symlinks and expanded size. Replace executable checkout design tokens with schema-validated JSON.

7. **[high] §9.3, §14 — The storefront binding is broader than its intended capability.**  
   A service binding “to the API origin” can potentially reach admin/internal paths. An origin allowlist does not constrain paths or prevent substituted tenant headers. `outboundService` controls global network calls; service bindings need their own restrictions. [Miniflare API](https://github.com/cloudflare/workers-sdk/blob/main/packages/miniflare/README.md).

   **Change:** Bind themes to a wrapper exposing only approved public catalog operations, injecting immutable tenant/market context and rejecting credentials. Default-deny global egress. Give checkout a separate capability. Authenticate edge purge and builder callbacks with distinct credentials.

8. **[high] §5.2–5.3, §7.7, §13 — RLS bootstrap and background-job access are unresolved.**  
   Staff membership is tenant-owned, but membership must authorize the requested tenant. Global job/outbox scans cannot run through ordinary tenant RLS without a defined mechanism. Nullable `jobs.tenant_id` also contradicts the blanket tenant-table rule.

   **Change:** Specify role grants and bootstrap paths: requested tenant context may be set solely for membership lookup, with no business access before membership succeeds. Use a narrowly privileged queue-claim function/role returning leased job identities; handlers then enter `TenantTx`. Prohibit tenant SQL through the raw pool. Test connection reuse after commit, rollback and cancellation. Transaction-local `set_config` itself is appropriate. [PostgreSQL documentation](https://www.postgresql.org/docs/17/functions-admin.html).

9. **[high] §5.3 — The Better Auth/Rust bridge lacks a complete verification and revocation contract.**  
   JWT/JWKS is supported, but the plugin defaults are not the specified five-minute `admin-api` tokens. “Validate against JWKS” leaves issuer, algorithms, expiry, key rotation, disabled users and MFA requirements open. Membership removal alone does not revoke a still-valid identity token. [Better Auth JWT documentation](https://better-auth.com/docs/plugins/jwt).

   **Change:** Pin explicit issuer/audience/expiry/algorithm configuration; keep browser JWTs in memory; define trusted origins and credentialed CORS. Specify bounded key refresh and revocation behavior. Require fresh authentication for staff/payment changes, and reject JWT issuance before required verification/MFA completes.

10. **[high] §10.3–10.4 — Payment creation has no crash-safe sequence.**  
    Order placement creates only a local payment record; Payment Element needs a provider intent/client secret. “All side effects via outbox” leaves that handoff undefined. A timeout can release stock while a payment succeeds.

    **Change:** Commit an immutable payable order/payment attempt plus outbox event first. Create the provider intent idempotently using that attempt ID; expose its status through a private endpoint. Define retries, cancellation and reconciliation. Late payment after stock release enters an exception/refund workflow and never silently restores fulfillment.

11. **[high] §10.4 — Stripe webhook handling is underspecified and contains an incorrect transition.**  
    The text routes both `succeeded` and `payment_failed` toward order confirmation. Signature verification does not establish the correct tenant, amount, currency or payment attempt. Direct-charge events belong to connected accounts and carry account context. [Stripe Connect webhooks](https://docs.stripe.com/connect/webhooks).

    **Change:** Only verified success confirms prepaid orders. Match connected account, provider object, environment, currency and expected amount; persist/deduplicate events before acknowledging. Handle out-of-order events without regressing paid state. Specify refund reconciliation, application-fee refund policy, account capability checks and disconnection handling.

12. **[high] §7.3, §8.1, §10.3 — Idempotency has conflicting identities and no concurrency semantics.**  
    HTTP keys, per-tenant order keys and `(tenant, cart_id)` are different contracts. They do not define changed-payload retries, concurrent placement, payment retries or concurrent refunds/coupon redemptions.

    **Change:** Use `(tenant, operation, key)` with request hash and stored result; conflicting reuse returns `409`. Enforce one order per converted cart separately. Lock/revalidate cart version, stock, coupon limits and refundable balance atomically. A failed payment creates a new attempt against the existing order.

13. **[high] §7.2–7.4, §10.3–10.6 — Inventory and order lifecycles are incomplete.**  
    Reservations are released on cancellation, but never explicitly consumed on shipment. COD has no explicit confirmation transition. `partially_returned` being terminal prevents a later return of the remaining items. Editing paid orders can change the debt without adjusting payment.

    **Change:** Define reservation → committed stock movement → release/restock transitions, with quantities and unique movement identities. Confirm COD when accepted. Model returns per line/quantity. Reconcile edits against captured/refunded amounts; for M1, prohibit financial edits after payment and use cancellation/refund plus replacement order.

14. **[high] §7.7, §13 — Worker crashes can strand jobs and lose emails.**  
    The claim query selects only queued jobs; no expired-running reclaim path is specified. Recording email as `sent` before SMTP can permanently suppress an email never transmitted. A deterministic Message-ID does not guarantee recipient deduplication.

    **Change:** Claim atomically with lease owner/token, reclaim expired leases, heartbeat long tasks and fence stale completions. Insert fan-out jobs and mark outbox dispatch atomically. Email states become pending/sending/accepted/uncertain; mark accepted only after SMTP acceptance. Retry uncertain sends under an explicit duplicate-tolerant policy.

15. **[high] §10.1–10.2 — The monetary algorithm stops before the difficult cases.**  
    The spec leaves cart-coupon allocation, shipping/COD-fee VAT, mixed rates, quantity rounding and partial-refund allocation to agents. All can produce different totals while satisfying simplistic “totals add up” tests.

    **Change:** Define an ordered algorithm: discounts allocated deterministically to eligible lines, tax calculated on discounted line totals, ancillary charges allocated under the selected jurisdiction’s policy, cash adjustment last. Persist allocation results. Refund original allocations with a deterministic residual rule; never reprice historical returns.

16. **[high] §10.1, §10.4 — COD is conflated with cash and delivery with payment.**  
    COD may be paid by card. Slovak cash rounding to €0.05 is missing, and collection by the carrier differs from remittance to the merchant. Slovak guidance explicitly distinguishes the collection method and rounding evidence. [Slovak tax authority](https://www.financnasprava.sk/sk/aktualne-dan-clo/faq/uctovnictvo), [VAT treatment of rounding](https://podpora.financnasprava.sk/362767-Z%C3%A1klad-dane-pri-dodan%C3%AD-tovaru-alebo-slu%C5%BEby).

    **Change:** Record actual tender and collector. Apply jurisdiction-specific cash rounding only where applicable. Separate delivered, collected and remitted states; reconcile carrier COD reports or require audited manual confirmation.

17. **[high] §7.4, §10.7 — Invoice issuance conflates payment, supply and final settlement.**  
    Prepayment can create a tax point before supply; a final settlement document must avoid taxing the advance twice. “Invoice on payment/shipment” does not define DUZP, advance documents, corrections or non-VAT merchants. ČNB also does not publish a new rate every calendar day. [Slovak tax-point guidance](https://www.financnasprava.sk/sk/podnikatelia/dane/dan-z-pridanej-hodnoty/danova-povinnost), [ČNB fixing calendar](https://www.cnb.cz/cs/financni-trhy/devizovy-trh/kurzy-devizoveho-trhu/kurzy-devizoveho-trhu/index.html).

    **Change:** Specify accountant-approved CZ/SK document scenarios, including advance receipt, final settlement, cancellation and partial credit. Persist tax-point basis, rate source/date and original allocations. Define weekend/holiday and unavailable-current-rate behavior. Separate SK DIČ from IČ DPH and document currency from statutory reporting currency.

18. **[high] §7.2, §10.2 — Omnibus history and display rules allow misleading discounts.**  
    Showing the lowest-price label does not make a percentage reduction against arbitrary `compare_at` lawful. History omits broadly available coupon reductions and tax-driven effective-price changes. Sale start/end is time-driven; ordinary write triggers will not capture it automatically. [CJEU Aldi Süd decision](https://infocuria.curia.europa.eu/tabs/redirect/juris/documents.jsf?num=C-330%2F23).

    **Change:** Calculate announced reductions against the legally applicable prior price. Persist effective-price intervals by variant and selling context, including scheduled transitions. Query intervals overlapping the lookback window. Block unsupported discount claims on imports lacking history; define new-product/progressive-reduction handling explicitly.

19. **[high] §9.4, §10.6 — Withdrawal requires more than an account/order-token form.**  
    The flow assumes refund only after goods arrive, omitting return evidence and statutory timing. It lacks a publicly discoverable guest entry, explicit confirmation and durable acknowledgment containing the submission. The EU withdrawal-function rules specify these interaction requirements. [Directive 2023/2673](https://eur-lex.europa.eu/legal-content/en/ALL/?uri=CELEX%3A32023L2673), [EU return guidance](https://europa.eu/youreurope/citizens/consumers/shopping/returns/index_en.htm).

    **Change:** Add a visible guest-capable withdrawal route, secure order identification, confirmation step and immediate durable receipt. Track delivery, declaration, evidence, goods receipt and refund deadlines separately. Include standard outbound delivery reimbursement and original-payment-method rules.

20. **[high] §7.3, §11.2–11.6 — Consent is too coarse and insufficiently enforced.**  
    One `marketing` purpose covers advertising disclosure and email campaigns. Enrollment-time consent can become stale before sending. “Recently viewed only in localStorage” does not itself exempt device storage from consent. Cookieless browser collection is not automatically exempt either. [EDPB technical-scope guidance](https://www.edpb.europa.eu/our-work-tools/our-documents/guidelines/guidelines-22023-technical-scope-art-53-eprivacy-directive_en).

    **Change:** Separate email marketing, advertising disclosure, analytics, personalization and review invitations. Resolve consent by tenant/controller and verified subject; recheck current permission and suppression at execution time. Never trust beacon-supplied purposes as proof. Before consent, restrict measurement to genuinely necessary/minimized server counters. Label consented-session funnel metrics accordingly.

21. **[high] §9.3, §12.3, §14 — SSRF and sensitive artifact access need enforceable rules.**  
    “Safe client” and private-IP rejection leave DNS rebinding, redirects, IPv6, credential forwarding and response expansion unspecified. Invoices, exports, labels and theme sources share the storage architecture, but only public asset access is described. Preview tokens exist in the model but not in the routing contract.

    **Change:** Separate public re-encoded media from private artifacts. Authorize tenant-scoped private downloads with short-lived URLs. Validate every resolved/connected destination and redirect; deny nonpublic addresses and credential forwarding; bound decompression. Require tenant/revision/expiry-bound preview authorization and sandbox its admin iframe.

22. **[high] §9.3, §17 WP4 — The Astro artifact contract is unproven, and asset routing is incomplete.**  
    Current Astro Cloudflare integration uses adapter entrypoints and generated deployment configuration; it is not safe to assume a standalone script plus `/_astro/*`. Public-directory assets need serving too. Checkout’s assets can fall into the theme route, and cached HTML can reference old assets after publication. [Astro Cloudflare documentation](https://docs.astro.build/en/guides/integrations-guide/cloudflare/).

    **Change:** Pin the full Astro/adapter/Miniflare/workerd version matrix. Add an early executable spike defining module manifest, compatibility flags and asset bindings. Namespace platform and revision assets separately; retain old content-addressed assets. Test restart, eviction, publication and rollback. “Generate workerd config” is an implementation option, not an independent fallback.

23. **[high] §11.1 — The combo-facet proposal does not define general variant-correct search.**  
    Meilisearch flattens nested structures. An exact `red|XL` token can solve that pair, but not arbitrary partial selections, three attributes, ranges, stock/price constraints or accurate facet counts. [Meilisearch data semantics](https://specs.meilisearch.dev/specifications/text/0121-data-types.html).

    **Change:** Use one document per sellable variant, filter before grouping by product, and define product-count facet semantics explicitly. Rehydrate current public data before responding. Require adversarial fixtures covering cross-variant false matches, three options, unavailable variants, market prices and multi-select filters before accepting WP5.

24. **[high] §17 — The WP order is not independently completable as written.**  
    WP4 promises full theme/performance before cart, search, consent and analytics exist. WP7 needs order state machines and transactional emails deferred to WP8. WP12 promises personalized campaign blocks before WP15. WP13 review invitations precede review storage/token handling. Parallel packages also share generated clients, migrations and theme routes.

    **Change:** Reorder/split:
    - WP0–1: foundation, identity, RLS and durable jobs.
    - Early spike: theme runtime, browser trust boundary, full-island performance and real AI-edit feasibility.
    - WP2 plus early WP6: catalog, money/tax/promotions/inventory contracts.
    - Split WP4 into runtime/SDK skeleton, then integrated default theme.
    - Before WP7: consent, customer sessions, order/payment state machines and minimal transactional mail.
    - Split WP7–8 into order placement, payment adapters, then fulfillment/invoicing/returns.
    - Move review contracts before invitations; personalized campaign blocks after recommendations.
    
    Each WP needs explicit prerequisites and acceptance tests possible at merge time. Security tests belong with those WPs, not only WP21.

25. **[medium] §10.4 — Bank-transfer correctness needs a protocol and reconciliation contract.**  
    PAY by square includes ordered fields, CRC32, compression parameters, headers and encoding details—not merely LZMA plus base32hex. “Test vectors” could otherwise mean self-generated round trips. Numeric order numbers also need variable-symbol limits and collision rules. [Official PAY by square specification](https://portal.bysquare.com/files/bysquare-PAYspecifications-1.2.0.pdf).

    **Change:** Pin the protocol version and use an audited implementation where suitable. Require independently sourced golden vectors and actual bank-app scans. Specify VS allocation, beneficiary account, SPAYD escaping, statement transaction identity, duplicate imports and late-payment handling. Scope matching to the receiving bank account and tenant.

26. **[medium] §9.1, §9.6, §16 — Performance acceptance can pass before the required page exists.**  
    Thirty kilobytes is not disproved by Solid, but it is unvalidated with all listed islands, consent, SDK and RUM. “First-load scripts” is ambiguous for idle/visible hydration. Strict `script-src 'self'` also needs an explicit strategy for required inline scripts/speculation rules.

    **Change:** Measure the complete product/category experience early, including deferred hydration and representative interaction. Count transferred executable JS consistently. Configure CSP hashes/nonces or externalization deliberately. Keep lab TBT and field INP distinct. Require manual keyboard/focus checks alongside axe, including payment and pickup selection.

27. **[medium] §2 D6, §11.1 — Search sizing and projection isolation need small explicit safeguards.**  
    There is no current universal 200-index ceiling; official documentation supports many small indexes while warning about frequent access across hundreds. The real risks are resource usage, rebuild duplication, stale visibility and collision-prone `tenant_short` naming. [Meilisearch limitations](https://www.meilisearch.com/docs/resources/help/known_limitations).

    **Change:** Use full tenant IDs, pin an exact image version/digest, restrict API versus indexing keys, enforce active/market-visible products and version indexing events. Benchmark representative tenant/locale counts with concurrent rebuilds. Keep per-tenant indexes for M1; do not prematurely build shared-index tenancy.

28. **[medium] §10.8 — Feed export/import promises exceed their defined semantics.**  
    Zboží is described as Heureka-compatible despite having its own namespace and field meanings. Importing a feed does not reliably supply complete stock, translations, options or historical prices. SKU/ITEM_ID precedence and duplicate old paths are unspecified. [Zboží specification](https://napoveda.zbozi.cz/xml-feed/specifikace/).

    **Change:** Implement separate channel serializers and semantic fixtures. Use `(tenant, import_source, external_id)` mappings with explicit update ownership. Default uncertain imports to draft and report missing fields; never invent price history. Define redirect collisions and archived-order imports that cannot trigger payments, stock movements or emails.

29. **[medium] §9.2, §14–17 — Several M1 obligations have no acceptance owner.**  
    GPSR is stored but its required storefront presentation is not an explicit gate. Customer erasure is promised but lacks a WP. Suppression is deferred to M2 although M1 sends mail. Backup restore is only a production runbook despite local persistent merchant data.

    **Change:** Assign M1 deliverables for visible safety data, legal-content validation, customer access/erasure with retention exceptions, transactional suppression, domain verification and a local DB/object-storage restore drill. Replace “pilot client live” with “local pilot acceptance”; list real-provider validation separately from mock acceptance.

30. **[medium] §9.7, §12, §17–18 — M1 carries premature infrastructure while postponing its highest-risk experiment.**  
    Per-tenant compilation of identical default themes and an LRU of 50 runtimes add work immediately. Meanwhile the agreed early AI spike is delayed to M3. Full readiness depending on Meilisearch also unnecessarily couples checkout availability to search.

    **Change:** Share one immutable default artifact with per-tenant runtime tokens while preserving revision pointers. Split tenant artifacts only when code diverges. Run the AI/runtime/security spike in the first weeks. Keep basic PostgreSQL jobs, defer sophisticated fairness, and report search degradation separately from core readiness.

**Things that are fine as-is**

- Rust modular monolith, separate worker and explicit SQL.
- Transaction-local tenant context, composite tenant FKs and non-owner runtime roles.
- Integer money, checked arithmetic and immutable financial snapshots.
- Constant gross prices across VAT changes, once tax liability is correctly determined.
- Better Auth for staff with Rust-owned authorization.
- Public storefront tokens as tenant identifiers—not secrets.
- Astro/Solid islands and Miniflare as choices, subject to the artifact spike.
- PostgreSQL outbox, disposable search projections and local provider mocks.