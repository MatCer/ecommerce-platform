# Research report: EU e-commerce SaaS launching in Czechia and Slovakia

**Research cutoff: 24 September 2026**

**Recommendation:** prioritise reliable CZ/SK commerce operations, accessible storefronts and compliance controls. Build a clean catalog/cart/checkout interface for agents, then add protocol adapters as distribution partners become available. **“AI-native” and “agent-ready” are already incumbent features; neither alone is a durable differentiator.**

**Reading notes**

- **Must / Should / Later** are product recommendations, not statements that a technology is legally mandatory.
- **n.d.** means the source does not provide a verified publication date. These living pages were checked on **24 September 2026**.
- **Unverified** means this research did not establish the claim—not that it is necessarily false.
- Vendor adoption figures and product promises are identified as vendor claims. Strategic assessments are labelled **analysis**.
- Legal responsibility depends on the actual service: software supplier, hosting provider, merchant, marketplace operator, AI-system provider and payment intermediary are different roles.

## A. Agentic commerce standards: current state

### A1. Standards and implementation priorities

| Initiative | What it is; maturity and support | What the platform should implement | Priority |
|---|---|---|---|
| **OpenAI/Stripe Agentic Commerce Protocol — ACP** | Open protocol for agent-mediated commerce and delegated payment credentials. OpenAI and Stripe developed it; Stripe supplies compatible Shared Payment Tokens. The merchant remains merchant of record. Implementing ACP **does not automatically secure listing or admission to ChatGPT**. Current OpenAI onboarding starts with structured product feeds and approved-partner access. [ACP project](https://www.agenticcommerce.dev/) and [OpenAI onboarding](https://developers.openai.com/commerce/guides/get-started), n.d. | Accurate catalog feed; an adapter exposing checkout operations, authoritative totals, availability, fulfillment choices and order updates; compatible PSP token handling. Treat distribution approval separately from protocol implementation. | **Should:** catalog readiness and adapter design. **Later:** production delegated checkout until partner access and CZ/SK processing are confirmed. |
| **Google Agent Payments Protocol — AP2** | Payment authorization and evidence layer, rather than a complete storefront protocol. Google announced it with over 60 collaborating organisations, including Adyen, Mastercard, PayPal and Revolut. Google subsequently announced donation to **FIDO Alliance** and AP2 **v0.2**, including pre-authorized “human not present” payments. [Launch](https://cloud.google.com/blog/products/ai-machine-learning/announcing-agents-to-payments-ap2-protocol), **16 September 2025**; [FIDO/v0.2 announcement](https://blog.google/products-and-platforms/platforms/google-pay/agent-payments-protocol-fido-alliance/), **28 April 2026**. | Verify signed authorization evidence through the participating payment stack; bind purchase details, amount and permitted actions; retain evidence for disputes. Current documentation distinguishes checkout and payment mandates. Do not implement only the original launch-era terminology. [AP2 documentation](https://ap2-protocol.org/), n.d. | **Later**, normally through a PSP or agent-commerce partner. |
| **Universal Commerce Protocol — UCP** | Broader commerce interoperability: discovery, carts, checkout and subsequent commerce operations. Developed by Google with Shopify, Etsy, Wayfair, Target and Walmart; endorsed by payment and retail partners. Supports API/MCP integration and AP2 compatibility. Launched January 2026 and continues evolving; current specifications reference **2026-08-25**. [Google technical introduction](https://developers.googleblog.com/under-the-hood-universal-commerce-protocol-ucp/), **11 January 2026**; [specification](https://ucp.dev/specification/overview/), version **25 August 2026**. | Publish discovery/profile metadata and implemented capabilities; map catalog, cart, checkout and order operations; negotiate supported versions. Keep shipping, tax, discounts and inventory authoritative on the merchant backend. | **Should:** strongest candidate for the first broad commerce adapter. Google shopping-surface eligibility remains a separate issue. |
| **Visa Intelligent Commerce** | Visa’s commercial infrastructure for agent payments, tokenization, authentication and spending controls. **Intelligent Commerce Connect** adds a network/protocol-agnostic integration through Visa Acceptance Platform. Its April announcement described selected-partner pilots and support for ACP/UCP and other protocols—not universal merchant availability. [Visa announcement](https://investor.visa.com/news/news-details/2026/Visa-Opens-the-Door-to-AI-Driven-Shopping-for-Businesses-Worldwide/), **8 April 2026**. | Work through an acquirer/PSP supporting the program; preserve token, authorization and agent-transaction information. This is not a replacement for catalog or checkout APIs. | **Later.** CZ/SK self-service availability for a new small SaaS: **unverified**. |
| **Mastercard Agent Pay** | Mastercard’s agent-payment framework, including agentic tokens and trusted agent transactions. Announced in 2025; September 2026 brought **Agent Connect** and expanded merchant tooling, with named payment-provider collaborators. This demonstrates ecosystem investment, not automatic availability through every Mastercard acquirer. [Agent Pay launch](https://investor.mastercard.com/investor-news/investor-news-details/2025/Mastercard-Unveils-Agent-Pay-Pioneering-Agentic-Payments-Technology-to-Power-Commerce-in-the-Age-of-AI/default.aspx), **29 April 2025**; [merchant expansion](https://www.mastercard.com/us/en/news-and-trends/press/2026/september/mastercard-gives-merchants-a-simpler-way-to-build--connect-and-s.html), **9 September 2026**. | Integrate via a participating PSP/acquirer; support user authorization, tokenized payments and auditable transaction context. | **Later.** Specific CZ/SK onboarding and pricing: **unverified**. |
| **HTTP Message Signatures / Web Bot Auth** | **RFC 9421** is a published IETF standard for signing HTTP messages. Web Bot Auth applies cryptographic identity to automated web clients; an IETF working group and Cloudflare implementation exist. Agent identity is explicitly distinct from authenticating the human user. [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421), **February 2024**; [IETF charter](https://datatracker.ietf.org/wg/webbotauth/about/) and [Cloudflare implementation](https://developers.cloudflare.com/bots/reference/bot-verification/web-bot-auth/), n.d. | Verify signatures and trusted keys; enforce expiry/replay protection; apply access policy and rate limits. A valid bot signature must not, by itself, authorize account access or purchases. Cloudflare supports a specific subset/profile, so test interoperability. | **Should** when exposing agent traffic. Use infrastructure support where possible. |
| **Storefront/Catalog MCP** | MCP exposes tools and data to AI applications. Shopify now has per-store catalog/cart tools and **Global Catalog MCP** for cross-merchant discovery, using UCP shapes. Its catalog migration was announced April 2026; legacy cart tools were maintained only until **31 August 2026**. [Catalog changelog](https://shopify.dev/changelog/storefront-catalog-mcp-now-implements-ucp), **22 April 2026**; [cart changelog](https://shopify.dev/changelog/storefront-mcp-cart-tools-are-being-deprecated-in-favour-of-ucp-cart-mcp), **24 June 2026**; [Global Catalog](https://shopify.dev/docs/agents/catalog/global-catalog), n.d. | Offer catalog search, product details, policies, cart operations and a checkout handoff. Protect customer/order tools with delegated authorization. MCP supplies the tool interface; it does not independently solve payments, legal consent or discovery. | **Should** for an agent-ready positioning. Start with public catalog and cart-to-human-checkout flows. |
| **`llms.txt`** | A community proposal by Jeremy Howard for machine-friendly site guidance and Markdown links. The proposal’s **v2** was updated in August 2026. It remains a proposal, not a payment, authorization or guaranteed ranking standard. [Specification](https://llmstxt.org/), published **3 September 2024**, updated **10 August 2026**. | Generate a small, accurate guide to catalog, policies and agent endpoints. Avoid stale prices or private data. | **Should, low effort**; below structured data and feeds. Conversion/discovery uplift: **unverified**. |
| **schema.org `Product` / `Offer`** | Established structured product vocabulary, actively supported by Google for product search and merchant listings. It describes products and offers; it does not execute purchases. [Google Product documentation](https://developers.google.com/search/docs/appearance/structured-data/product) and [Merchant Center supported attributes](https://support.google.com/merchants/answer/6386198?hl=en), n.d. | Server-render accurate product/variant identifiers, prices, currency, availability, images and applicable shipping/return information. Keep markup, feeds and checkout consistent. | **Must** for commercial discoverability, although not a general legal requirement. |

**Availability caveat:** these sources establish real protocols and implementations. They do **not** establish that an arbitrary Czech or Slovak merchant can immediately activate every Google/OpenAI in-agent checkout experience.

### A2. Is `auth.md` real?

**Yes—as of the research date, it is a real WorkOS-authored open protocol.** Calling it fictional or merely confusing it with `AGENTS.md` would be outdated.

WorkOS introduced it on **21 May 2026**. It combines a Markdown discovery document with agent registration/authentication flows. Current documentation says structured OAuth Protected Resource Metadata remains the machine-readable source of truth. [WorkOS announcement](https://workos.com/blog/agent-registration-with-auth-md), **21 May 2026**; [current protocol](https://workos.com/auth-md), n.d.

Important distinctions:

- It is **WorkOS-led**, not an independently established IETF standard called “auth.md.”
- It builds on existing OAuth mechanisms. Current documentation describes `/agent/identity` and OAuth token/revocation endpoints; these differ from the launch article’s older endpoint examples.
- WorkOS describes early adoption; a complete independently verified adopter list and broad commerce adoption were **not established**.
- Registration and delegated identity do not themselves grant permission to spend money. [Current application guide](https://workos.com/auth-md/docs/apps), n.d.

**Priority: Later**, unless agent-created customer accounts become a validated requirement. For v1, established OAuth-based delegated access plus explicit checkout authorization is sufficient. Public product browsing should not require account creation.

### A3. Recommended agent-ready architecture — analysis

Use one authoritative commerce backend, with ACP/UCP/MCP adapters around it:

1. Public, current catalog and policies.
2. Deterministic cart pricing, inventory checks and shipping quotes.
3. Explicit purchase authorization and PSP-controlled payment credentials.
4. Idempotent order creation and auditable changes.
5. Human handoff when authentication, payment challenge or ambiguous intent requires it.

The useful promise is **“agents can reliably discover products and prepare valid purchases.”** Avoid promising universal autonomous purchasing before partner access, payment authorization and dispute handling are proven.

## B. EU/CZ/SK legal requirements and responsibility

### B1. Platform versus merchant

| Role | Typical responsibility |
|---|---|
| **Merchant** | Seller identity, truthful product information, lawful pricing/promotions, fulfillment, returns, tax treatment and lawful marketing. |
| **SaaS platform** | Its own contracts/security/privacy obligations; generally processor duties for merchant customer data; controller duties for its own purposes; potentially hosting-provider duties. |
| **Both** | Platform supplies correct controls; merchant configures and uses them lawfully. Contract wording cannot override the actual GDPR role. |
| **Marketplace operator** | Additional rules may arise when the platform facilitates contracts between consumers and independent sellers. |
| **AI provider/deployer** | Classification depends on who develops, markets and uses the AI system, not merely who calls the underlying model API. |

The controller/processor distinction and processor contracts follow GDPR Articles 4, 24 and 28. **A SaaS is not automatically “only a processor”** if it independently reuses customer data for advertising or model training. [GDPR](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32016R0679), published **4 May 2016**.

### B2. Omnibus: discounts, reviews and personalized prices

| Requirement | Practical platform requirement | Responsible party |
|---|---|---|
| **30-day prior price** | For covered announcements of goods-price reductions, calculate/display the lowest price applied during the preceding 30 days. Preserve price history by merchant, item/variant and relevant sales context. | Merchant legally; platform supplies correct calculations and records. |
| **Discount percentage** | Calculate the advertised reduction against the legally relevant prior price, not an inflated “regular price.” | Merchant and promotion engine. |
| **Review authenticity disclosure** | Explain **whether and how** reviews are checked. Do not claim verified purchasers without reasonable verification. Preserve provenance and disclose moderation practices. | Merchant; platform if it operates the review service. |
| **Personalized pricing disclosure** | Tell the consumer before purchase when price is personalized using automated decision-making. This differs from ordinary demand-based dynamic pricing. | Merchant; platform must expose the disclosure trigger. |

Sources: [Omnibus Directive 2019/2161](https://eur-lex.europa.eu/legal-content/en/ALL/?uri=CELEX:32019L2161), published **18 December 2019**; [CJEU, Aldi Süd, C‑330/23](https://curia.europa.eu/jcms/upload/docs/application/pdf/2024-09/cp240152en.pdf), **26 September 2024**; [Commission pricing guidance](https://europa.eu/youreurope/citizens/consumers/unfair-treatment/unfair-pricing/index_en.htm), n.d.

**Local implementation:** Czech consumer-law changes took effect in January 2023; Slovakia’s new Consumer Protection Act 108/2024 took effect **1 July 2024**. Implement national exceptions for new goods, progressive reductions and perishables rather than one unconditional rule. [ČOI explanation](https://coi.gov.cz/novela-zakona-o-ochrane-spotrebitele/), **2023**; [Slovak Act 108/2024](https://static.slov-lex.sk/pdf/SK/ZZ/2024/108/ZZ_2024_108_20240701.pdf), **2024**, effective **1 July 2024**.

**Recommendation:** price-history tracking is v1 infrastructure. For imported shops without trustworthy historical prices, block unsupported discount claims rather than inventing a prior price.

### B3. European Accessibility Act — EAA

E-commerce services are covered from **28 June 2025**. The service-provider microenterprise exemption generally concerns businesses with **fewer than 10 people and turnover or balance-sheet total not exceeding €2 million**. It is not an exemption for every SME, and product obligations are distinct. [Directive 2019/882](https://eur-lex.europa.eu/legal-content/NL/ALL/?uri=CELEX:32019L0882), published **7 June 2019**; [EU business guidance](https://europa.eu/youreurope/business/selling-in-eu/selling-goods-services/accessibility/index_en.htm), n.d.

- **Merchant:** determines whether its consumer-facing service is covered, supplies accessibility information and maintains compliance.
- **Platform:** should provide accessible templates, checkout, consent dialogs, search, authentication, payment and pickup-point selection. Being a solo developer does not exempt a larger merchant using the platform.
- **Implementation target—analysis:** WCAG 2.2 AA is a sensible engineering baseline, but do not advertise it as the complete statutory test or assume an accessibility overlay establishes compliance.

Czech implementation is **Act 424/2023**; MPO published specific e-commerce guidance in 2026. Slovakia implements through **Act 351/2022**. [MPO guidance](https://mpo.gov.cz/cz/podnikani/pristupnost-vyrobku-a-sluzeb/navodne-dokumenty-k-zajisteni-pozadavku-na-pristupnost--292236/), **March 2026**; [Slovak legislation](https://www.slov-lex.sk/ezbierky/pravne-predpisy/SK/ZZ/2022/351/), **2022**.

**Priority: Must** for the platform’s default design. AI customization must preserve accessible components and interactions.

### B4. GDPR, ePrivacy and server-side tracking

**Moving tracking to a server does not remove consent or GDPR obligations.** ePrivacy concerns access to or storage of information on a user’s device; GDPR separately governs personal-data processing. Pixels, identifiers and tracking links can fall within the rules even without conventional cookies. [EDPB Guidelines 2/2023, final version](https://www.edpb.europa.eu/system/files/2024-10/edpb_guidelines_202302_technical_scope_art_53_eprivacydirective_v2_en_0.pdf), **7 October 2024**.

| Processing | Treatment |
|---|---|
| Cart, checkout, security and necessary transaction processing | Use the relevant necessity/legal basis; do not ask for blanket consent to perform the purchase contract. |
| Advertising pixels, retargeting and nonessential device tracking | Obtain valid prior consent where required; propagate withdrawal to server-side destinations. |
| Product recommendations | Contextual recommendations can avoid personal profiling. Behavioral personalization requires its own lawful-basis and tracking assessment. |
| Profiling with legal or similarly significant effects | Assess GDPR Article 22; ordinary product recommendations are not automatically such decisions. |
| Hashed email sent to an advertising platform | Hashing does not automatically make identifiable data anonymous. |

The platform needs processor terms, subprocessor disclosure, appropriate transfer arrangements, retention controls, tenant isolation, data-rights support and breach procedures. Merchants need transparent notices and documented purposes/lawful bases. [GDPR, Articles 5–6, 12–22, 28, 32–35 and Chapter V](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32016R0679), **4 May 2016**.

**Recommendation:** maintain a purpose-specific consent ledger and enforce it at the event dispatcher, not only in browser tags. Do not market server-side tracking as a consent bypass.

### B5. Newsletters, soft opt-in and abandoned carts

**Baseline:** electronic direct marketing generally requires prior consent. The existing-customer exception—“soft opt-in”—permits marketing of the seller’s **own similar products/services** where contact details were obtained in a sale context and the recipient can refuse at collection and in every message. [ePrivacy Directive, Article 13](https://eur-lex.europa.eu/eli/dir/2002/58/oj), published **31 July 2002**, subsequently amended.

- **Czechia:** Section 7 of Act 480/2004 and ÚOOÚ guidance distinguish customers from prospects. Marketing must identify the sender and offer effective refusal. [ÚOOÚ commercial communications FAQ](https://uoou.gov.cz/index.php/profesional/qa-otazky-a-odpovedi/obchodni-sdeleni), n.d.
- **Slovakia:** Section 116 of Act 452/2021 provides consent rules and specific exceptions. Do not assume a general GDPR legitimate interest authorizes unsolicited email. [Slovak regulator guidance](https://www.teleoff.gov.sk/urad/odbory-oddelenia/odbor-statneho-dohladu-elektronickych-komunikacii/nevyziadana-komunikacia/informacie-verejnost-ohladom-nevyziadanej-komunikacie/), n.d.
- **Abandoned cart—conservative implementation recommendation:** an entered email or unfinished checkout is insufficient evidence of an existing-customer exception. Require consent or a separately established qualifying customer relationship before sending promotional recovery messages.
- **B2B:** do not assume all business addresses are exempt. National exceptions differ.

**Platform:** record the basis, source, wording and timestamp; maintain suppression lists; separate transactional and promotional messages. **Merchant:** chooses lawful audiences/content. Double opt-in is useful evidence, but should not be described as universally mandatory.

### B6. Withdrawal, checkout information and consumer remedies

For ordinary distance B2C goods sales, consumers generally have **14 days from receipt** to withdraw. Refund rules cover standard outbound delivery; withholding may be permitted until goods or return evidence arrive. Exceptions include genuinely personalized goods and certain sealed/digital products, subject to conditions. Failure to give withdrawal information can extend the period. B2B buyers do not receive this consumer right merely because they order online. [EU returns guidance](https://europa.eu/youreurope/citizens/consumers/shopping/returns/indexamp_en.htm), n.d.; [Consumer Rights Directive materials](https://commission.europa.eu/law/law-topic/consumer-protection-law/consumer-contract-law/consumer-rights-directive_en), n.d., underlying directive **2011**.

**A critical 2026/2027 distinction:**

| Jurisdiction | Online withdrawal-function position |
|---|---|
| **EU directive** | Directive 2023/2673 requires implementation of an online withdrawal function, with application from **19 June 2026**. It applies beyond financial services through amendments to consumer-rights rules. |
| **Slovakia** | Act **311/2025** implements changes effective **19 June 2026**. |
| **Czechia** | ČOI’s **14 September 2026** notice says Act **159/2026**, published **2 September 2026**, takes effect **1 January 2027**. Do not confuse the EU deadline with the Czech implementing law’s actual effective date. |

Sources: [EU Directive 2023/2673](https://eur-lex.europa.eu/eli/dir/2023/2673/oj), published **28 November 2023**; [Slovak implementing text](https://eur-lex.europa.eu/legal-content/SK/TXT/PDF/?uri=NIM:202506954), **2025**; [ČOI notice](https://coi.gov.cz/tlacitko-usnadni-odstoupeni-od-smlouvy/), **14 September 2026**.

**V1:** supply a visible withdrawal workflow, order identification, confirmation step and durable acknowledgment. Also support seller/contact information, full payable totals, clear payment-obligation wording, terms, complaints and applicable legal-guarantee information.

**Outdated-template trap:** the EU ODR platform closed **20 July 2025**. Do not insert the old mandatory ODR-platform link into new templates; applicable national ADR information still needs assessment. [Commission closure notice](https://consumer-redress.ec.europa.eu/site-relocation_cs?event=main.privacyForTrader.show), n.d.

### B7. VAT, OSS/IOSS and B2B validation

| Topic | Requirement and platform consequence |
|---|---|
| **Domestic VAT** | Support VAT-registered and nonregistered merchants. Current headline rates: CZ **21% / 12%**; SK **23% / 19% / 5%**. Product categories and exemptions matter; rates cannot be hard-coded as one national percentage. |
| **Union OSS** | Eligible intra-EU B2C distance sales can be reported through one member state. The **€10,000** combined threshold has conditions; it is not a general exemption for all cross-border sellers. Destination VAT generally applies once relevant conditions are met. |
| **IOSS** | For qualifying imported consignments with intrinsic value **not exceeding €150**, excluding excise goods. It is a VAT mechanism, not a blanket promise of duty-free imports. |
| **B2B / VIES** | Validate VAT IDs and retain evidence. A valid VAT ID alone does not establish eligibility for zero-rated intra-Community goods supply: movement of goods and other conditions still matter. |
| **Responsibility** | Merchant/accountant owns registration, tax classification and filing. Platform supplies correct calculations, evidence and exports; marketplace deemed-supplier rules require separate analysis. |

Sources: [CZ tax administration](https://financnisprava.gov.cz/cs/dane/danovy-system-cr/popis-systemu), n.d.; [SK VAT rates](https://www.financnasprava.sk/sk/podnikatelia/dane/dan-z-pridanej-hodnoty/sadzby-dane), updated materials including **2026**; [Commission OSS guidance](https://vat-one-stop-shop.ec.europa.eu/one-stop-shop_en), n.d.; [IOSS/import guidance](https://taxation-customs.ec.europa.eu/customs/customs-procedures-import-and-export/customs-operations/customs-formalities-low-value-consignments_en), n.d.; [VIES guidance](https://europa.eu/youreurope/business/finance-and-tax/vat/check-vat-number-vies/index_en.htm), n.d.

**Current import issue:** Slovakia’s customs guidance reports the new €3 customs-duty treatment for covered low-value imports from **1 July 2026**. A dropshipping feature must distinguish VAT, customs duty and carrier charges. [Slovak e-commerce customs guidance](https://www.financnasprava.sk/sk/podnikatelia/clo-obchodny-tovar/ecommerce), n.d., **2026 rules**.

### B8. Invoices and ViDA

**V1 invoice data model—recommendation:** seller/buyer identity; separate company, tax and VAT identifiers; unique document numbering; issue and supply dates; line descriptions; net/tax/gross amounts; currencies; appropriate exemption/reverse-charge wording; credit notes and links to originals. Do not treat Slovak **DIČ** and **IČ DPH** as interchangeable.

Czech VAT-document requirements are principally in Act 235/2004, notably §29; Slovak invoice requirements in Act 222/2004, notably §§72–76. Export to accounting software and preserve issued-document history. The exact document/issuance requirement varies by transaction. [CZ tax-document guidance](https://financnisprava.gov.cz/cs/dane/dane/dan-z-pridane-hodnoty/informace-stanoviska-a-sdeleni/danove-doklady), n.d.; [SK invoice requirements](https://podpora.financnasprava.sk/227408-N%C3%A1le%C5%BEitosti-fakt%C3%BAry), n.d.

| Date | Confirmed development | Product implication |
|---|---|---|
| **14 April 2025** | ViDA entered into force; member states gained more scope for domestic mandatory e-invoicing. | National mandates can precede 2030. |
| **1 January 2027 — Slovakia** | Mandatory electronic invoicing for covered domestic B2B supplies by VAT payers; recipients need receiving capability. B2C treatment differs. | Plan an accounting/provider integration now. A PDF alone is not a structured e-invoice implementation. |
| **1 July 2028** | Further ViDA single-VAT-registration/OSS measures. | Keep tax/export architecture adaptable. |
| **1 July 2030** | EU digital reporting for covered cross-border B2B transactions. | Structured e-invoicing/reporting becomes a wider requirement. |
| **1 January 2035** | Deadline for alignment of existing domestic real-time reporting systems with the EU model. | **Not** a universal instruction to postpone domestic e-invoicing until 2035. |

Sources: [Commission ViDA timeline](https://taxation-customs.ec.europa.eu/taxation/vat/vat-digital-age-vida_en), package adopted **11 March 2025**, implementation work programme **13 May 2026**; [Slovak implementation announcement](https://www.financnasprava.sk/_img/pfsedit/Dokumenty_PFS/Pre_media/Tlacove_spravy/Rok_2025/2025.12.11_TS_e-Faktura.pdf), **11 December 2025**; [Slovak provider/onboarding announcement](https://www.financnasprava.sk/sk/pre-media/novinky/archiv-noviniek/detail-novinky/_efak-vyber-digi-post-verej-ts), **3 June 2026**.

**Unverified:** a generally applicable Czech domestic B2B e-invoicing mandate before the EU cross-border milestone was not established in this research. Do not substitute EET for e-invoicing: they address different records.

### B9. EU AI Act

**The timeline changed during 2026.** The Commission confirms the AI Omnibus entered into force **27 July 2026**, moving relevant Annex III high-risk rules to **2 December 2027**, and Annex I product-related high-risk rules to **2 August 2028**. Article 50 transparency rules generally applied from **2 August 2026**. [Commission update](https://digital-strategy.ec.europa.eu/en/news/ai-omnibus-enters-force), **27 July 2026**; [transparency guidance](https://digital-strategy.ec.europa.eu/en/library/guidelines-transparency-obligations-providers-and-deployers-ai-systems), **20 July 2026**.

| Feature | Likely treatment and action |
|---|---|
| **Recommendations, AI storefront design, ordinary product-copy assistance** | Not automatically high-risk simply because they use AI. Classification follows intended use; consumer/GDPR rules still apply. |
| **Customer chatbot / conversational shopping agent** | Make interaction with AI apparent unless already obvious in context. Provide disclosure by the first interaction. |
| **AI-generated text/images** | Providers of relevant generation systems face machine-readable marking/detectability duties. User-facing disclosure duties differ for deepfakes and public-interest text; there is no blanket rule requiring a visible “AI-written” badge on every product description. |
| **AI credit scoring or BNPL eligibility** | May enter high-risk territory; leave underwriting to the regulated provider rather than adding it casually. |
| **Purchasing agents** | “Agent” is not a standalone high-risk class. Assess actual purpose, permissions and effects. Prevent unauthorized orders and misleading representations. |

Sources: [AI Act overview](https://digital-strategy.ec.europa.eu/en/policies/regulatory-framework-ai), n.d.; [Article 50](https://ai-act-service-desk.ec.europa.eu/en/ai-act/article-50), underlying regulation published **12 July 2024**.

The amended rules retain measures to **support staff AI literacy**, without requiring a guaranteed individual literacy level. Providers of qualifying synthetic-content systems placed on the market before **2 August 2026** have a specific Article 50(2) transition to **2 December 2026**; a new launch should not assume it qualifies. [Amending Regulation 2026/1744](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=OJ:L_202601744), **July 2026**.

**Role split:** the SaaS can be an AI-system provider when it markets its own integrated system; the merchant may be a deployer. Using an external model API does not automatically transfer every obligation to the model vendor.

### B10. GPSR, geo-blocking and DSA

| Area | Requirements | Allocation |
|---|---|---|
| **GPSR product safety** | Applicable since **13 December 2024**. Covered online offers must clearly show manufacturer contact information; EU responsible-person information where required; product identification/image; relevant warnings and safety information in appropriate languages. | Merchant/economic operator supplies correct data. Platform needs structured fields and visible presentation. Marketplace duties are additional. [GPSR, Article 19](https://eur-lex.europa.eu/eli/reg/2023/988), published **23 May 2023**, consolidated version **29 May 2026**. |
| **Geo-blocking** | Avoid unjustified nationality/residence discrimination, blocked access and forced redirects. The regulation does **not** generally oblige a merchant to deliver everywhere in the EU or accept every payment method. | Merchant sets lawful territories/payment rules; platform must support access without forced country redirects. [Commission guidance](https://digital-strategy.ec.europa.eu/en/policies/geoblocking), n.d.; Regulation 2018/302, **2018**. |
| **DSA before a marketplace** | A hosted SaaS may already qualify as a hosting intermediary. Relevant duties can include contact points, appropriate terms, notice-and-action procedures and explanations of restrictions. | Platform. Do not defer all DSA assessment until marketplace launch. |
| **DSA after adding a marketplace** | Additional online-platform/marketplace provisions address trader traceability, compliance-oriented interface design and consumer information. Micro/small-enterprise exemptions cover specified duties, not the whole regulation. | Marketplace operator; sellers retain their underlying duties. [DSA, Articles 11–18, 19 and 29–32](https://eur-lex.europa.eu/eli/reg/2022/2065), published **27 October 2022**, generally applicable **17 February 2024**. |

**Scope boundary:** regulated goods, packaging/EPR, WEEE, food, cosmetics and product-specific labeling need separate category/country checks. The report’s general commerce checklist is not an exhaustive approval for every assortment.

## C. CZ/SK/CEE integration checklist

**Priorities below are analysis.** They reflect launch practicality and documented capabilities; they are not a claim that every merchant needs every provider.

### C1. Payments

| Integration | Market relevance and implementation | Priority |
|---|---|---|
| **GoPay** | Local cards, wallets and bank-payment options. Implement merchant onboarding, payment status verification, refunds and reconciliation. Official materials now include **Payments API v4**; older V3 documentation remains discoverable. [Payment methods](https://www.gopay.com/cs/platebni-metody/) and [current API](https://api-docs.gopay.com/), n.d. | **Must have one local PSP:** GoPay is a candidate. |
| **Comgate** | CZ/SK-oriented gateway with local banking methods and expansion support including BLIK. Comgate currently claims **23,000 e-shops**; this is a vendor figure, not independently audited market share. [Comgate](https://www.comgate.eu/cs/platebni-brana), n.d. | **Must-have candidate**, alternative to GoPay. |
| **ThePay** | Another Czech gateway; official REST client and test environment exist. Detailed current market share was **not verified**. [Official client/documentation](https://github.com/ThePay/api-client), n.d. | **Should** when merchants request it. |
| **Stripe** | Available to businesses in CZ/SK; useful for international payments and the SaaS’s own subscriptions. Validate merchant-country, customer-country, currency and method eligibility separately. [Global availability](https://stripe.com/global), n.d. | **Should**, potentially the initial PSP if target merchants accept its local coverage/economics. |
| **Adyen** | Broad international payment coverage and unified-commerce capabilities. Commercial fit/onboarding must be assessed against merchant scale. [Pricing and supported methods](https://www.adyen.com/pricing), n.d. | **Later**, for larger merchants. |
| **BLIK / Przelewy24 — Poland** | BLIK reported **2.9 billion transactions in 2025**, with e-commerce its largest channel. P24 supports BLIK and other Polish methods. Integration may come through a PSP rather than a direct connection. [BLIK results](https://www.blik.com/en/nearly-3-bn-blik-transactions-in-2025-and-over-2-m-new-users), **17 February 2026**; [P24 methods](https://www.przelewy24.pl/metody-platnosci), n.d. | **Must for a serious PL launch**, not CZ/SK v1. |
| **Skip Pay / Twisto** | Local deferred-payment/installment options. Skip Pay has merchant integration and individually agreed pricing; Twisto publishes current Merchant/PSP APIs and marks older interfaces deprecated. [Skip Pay](https://skippay.cz/pro-eshop), [Twisto docs](https://docs.twisto.cz/cs), n.d. | **Should**, category-dependent. |
| **Klarna** | Current documentation includes CZ/CZK and SK/EUR. Country agreements, product availability and merchant approval still matter. [Klarna country requirements](https://docs.klarna.com/acquirer/klarna/get-started/data-requirements/puchase-countries-currencies-locales/), n.d. | **Should/Later**, depending on international demand. |

**V1 payment workflow—recommendation:** cards/wallets through one supported PSP, bank transfer, optional cash on delivery, partial refunds and reconciliation. Preserve asynchronous payment states. Do not mark an order paid solely from the browser’s return URL.

Allow merchant-owned PSP accounts. Becoming the party that receives and redistributes merchant funds introduces a materially different payments-business scope.

### C2. Delivery and pickup points

Packeta reports **184 million parcels delivered in 2025** and continued pickup-box expansion in CZ/SK/Hungary—strong practical evidence for prioritizing its integration. [Packeta results](https://blog.packeta.com/in-2025-packeta-group-delivered-184-million-parcels-their-total-weight-was-equivalent-to-about-500-million-average-book/), **February 2026**.

| Carrier | Required integration scope | Priority |
|---|---|---|
| **Packeta / Zásilkovna** | Pickup/box selector, current location IDs, carrier/service restrictions, labels, tracking and COD. Use current widget/feed documentation. [Packeta docs](https://docs.packeta.com/how-to-update-packeta-widget), n.d. | **Must** for the proposed mainstream CZ/SK offering. |
| **PPL** | Czech home delivery and pickup network; integrate current API/widget and labels. [PPL integration portal](https://developer.ppl.cz/web/guest/jak-zacit), n.d. | **Should**; can be the first CZ home-delivery option. |
| **DPD** | Home delivery and Pickup; API credentials and country/service-specific mappings. [DPD GeoAPI](https://geoapi.dpd.cz/public-docs/), n.d. | **Should**; useful across launch markets. |
| **GLS** | Home delivery/pickup alternatives; official API integration available. [GLS integration information](https://gls-group.com/CZ/cs/prepravni-reseni/uzitecne-informace-pro-odesilatele/prepravni-software/), n.d. | **Should**. |
| **Balíkovna** | Czech pickup/address delivery; checkout selector, labels and submission data. [Official implementation materials](https://www.balikovna.cz/cs/ke-stazeni), documents effective **1 January–1 July 2026**. | **Should**, merchant-demand driven. |
| **Slovenská pošta** | Postal delivery/pickup, widget, labels and ePodací hárok. REST and SOAP interfaces are documented. [Official integration guide](https://www.posta.sk/epodaci-harok/pomoc), n.d. | **Should**, particularly for SK merchants already using postal services. |

**Minimum practical launch:** Packeta plus one reliable home-delivery path per country. An aggregator can cover the remaining carriers initially, provided service selection, label generation, COD and tracking actually work.

### C3. Feeds, comparison sites and reviews

| Channel | Platform work | Priority |
|---|---|---|
| **Heureka CZ/SK** | Country-appropriate XML, stable item IDs, identifiers, VAT-inclusive prices, delivery/availability and variant handling. [Official feed specification](https://sluzby.heureka.cz/napoveda/xml-feed/), n.d. | **Must-have capability** for broad local retail positioning. |
| **Zboží.cz** | Separate compliant feed and diagnostics; do not assume Heureka XML is interchangeable. [Specification](https://napoveda.zbozi.cz/xml-feed/), n.d. | **Must for CZ-focused acquisition support**. |
| **Google Merchant Center** | Product feed or Merchant API, shipping/returns setup and synchronized landing-page data. [Product specification](https://support.google.com/merchants/answer/7052112), [Merchant API](https://developers.google.com/merchant/api), n.d. | **Must**. |
| **Glami** | Fashion-specific taxonomy, sizes, materials, variant grouping and images. [Feed specification](https://help.glami.info/xml-feed), n.d. | **Must for fashion; Later otherwise**. |
| **Heureka Ověřeno zákazníky / Overené zákazníkmi** | Submit qualifying orders for satisfaction surveys, respect refusal/suppression and display earned ratings accurately. The program is distinct from merely having a product feed. [Survey workflow](https://sluzby.heureka.cz/napoveda/jak-funguje-odesila-dotazniku-spokojenosti/), n.d. | **Should**, early. |

Review-survey integration is **not** permission for unrelated newsletters. Feed publication and conversion tracking also have different privacy implications; gate tracking separately.

### C4. Accounting and ERP

| Product | Integration route and relevance | Priority |
|---|---|---|
| **Pohoda** | XML import/export; mServer supports automated communication. Useful for accountants and merchants whose orders/inventory already live there. [Official XML API](https://www.stormware.cz/pohoda/xml/), n.d. | **Should**, often the first accounting export. |
| **Money S3** | Eshop-konektor/XML and a newer S3Api module; confirm edition/module licensing. [Official connector](https://money.cz/vlastnosti/e-shop-konektor-s3/), n.d. | **Should** for relevant merchants. |
| **Fakturoid** | Cloud invoicing via API v3; plan/request limits apply. [API v3](https://www.fakturoid.cz/api/v3), n.d. | **Should**, good small-CZ-business integration. |
| **iDoklad** | Build against **API v3**. API v2 is scheduled to stop **5 October 2026**, shortly after this report. [Official deprecation notice](https://www.idoklad.cz/blog/blizi-se-ukonceni-idoklad-api-v2-zkontrolujte-sve-integrace), **3 July 2026**. | **Should**. |
| **SuperFaktúra** | API integration for invoicing, especially relevant to SK onboarding. Confirm plan limits and its supported 2027 e-invoicing pathway. [Official integration guidance](https://pomoc.superfaktura.sk/ktore-eshopy-je-mozne-prepojit-so-superfakturou/), originally **2014**, living page. | **Should**, early for SK. |

**Recommendation:** ship reliable document export plus one cloud invoicing connector first. Add bidirectional ERP stock/order synchronization only with a clear system of record and reconciliation workflow.

**Unverified:** comparable current market-share figures for these accounting products were not established; the ordering above is not a market-share ranking.

### C5. Fiscal reporting: CZ EET and SK eKasa

**Czechia**

- Original EET was abolished from **1 January 2023**. [Tax administration notice](https://financnisprava.gov.cz/cs/financni-sprava/media-a-verejnost/tiskove-zpravy-gfr/tiskove-zpravy-2022/zruseni-elektronicke-evidence-trzeb-od), **21 December 2022**.
- **Current development:** the president signed EET 2.0 on **17 September 2026**. The official announcement dated **22 September** states a **1 January 2027** start and says promulgation will follow. Parliament’s record confirms signature. [Official announcement](https://eet.gov.cz/cs/pro-media/tiskove-zpravy/2026/prezident-podepsal-zakon-o-eet-2-0-evidence-trzeb-1315), **22 September 2026**; [legislative record](https://www.psp.cz/sqw/historie.sqw?o=10&t=189), status **24 September 2026**.
- The announced scope centres on contact payments, including cash. **Promulgation number and final transaction-by-transaction scope were not verified here.**

**Platform consequence:** no original-EET integration for today’s launch; schedule EET 2.0 assessment now for merchants taking pickup/POS/cash payments. Do not declare “Czech fiscal reporting is permanently irrelevant.”

**Slovakia**

Act **384/2025** changed revenue-recording rules from **1 January 2026**, broadening coverage. Applicability depends on how revenue is received; the official guide includes an exception for goods sold on COD. Treat ordinary account transfers, gateway settlements, merchant-collected cash and courier COD as separate payment flows. [Official statutory guide](https://www.financnasprava.sk/_img/pfsedit/Dokumenty_PFS/Zverejnovanie_dok/Sprievodca/Sprievodca_danami/2026/2026.01.13_Sprievodca_ev_trzieb.pdf), **13 January 2026**.

Current tax-authority guidance gives **1 May 2026** for the covered obligation to offer cashless payment for revenue exceeding €1, subject to exceptions—older announcements said March. [Current eKasa guidance](https://www.financnasprava.sk/sk/podnikatelia/dane/ekasa/erp), n.d.

**Platform consequence:** provide an eKasa/POS integration path where the merchant’s collection method requires it. A normal SaaS invoice is not an eKasa receipt. Gateway-specific classification should be confirmed before promising an exemption.

## D. Competitive landscape

### D1. Current pricing and competitive position

Prices are **base platform prices**, not total operating cost. Payment fees, applications, AI usage, implementation and tax may be additional. Living pricing pages are **n.d., checked 24 September 2026**.

| Platform | Verified pricing | Strengths | Constraints / competitive opening — analysis |
|---|---|---|---|
| **Shoptet** | Monthly CZK, excluding VAT: **Free 0; Basic 440; Business 1,490; Profi 2,490; Enterprise 4,690**. Product caps: **10 / 100 / 1,000 / 5,000 / 50,000**. Premium from **12,000 CZK/month**. [Pricing](https://www.shoptet.cz/cenik/). | Strong local packaged offering, support and extensions. Already offers AI administration features and MCP connectivity. [AI offering](https://www.shoptet.cz/umela-inteligence/), n.d. | Add-ons affect total cost; standard SaaS customization and private-API access differ from Premium. Compete on complete workflows and safe customization, not “we have AI.” |
| **Upgates** | Monthly CZK excluding VAT: **Bronze 450; Silver 1,150; Gold 1,750; Platinum 3,250**. Annual payment discounts shown at 10%. Product limits **100 / 1,000 / 5,000 / 50,000**; languages **1 / 2 / 5 / unlimited**. [Pricing](https://www.upgates.cz/cenik). | Broad included feature set; multilingual tiers make it a credible CZ/SK expansion competitor. | Product/language tier limits remain. Its included-feature proposition already challenges an “all essentials included” pitch. |
| **Shopify** | CZ monthly billing: **Basic 670; Grow 1,830; Advanced 9,510 CZK/month**. Annual equivalents **499 / 1,370 / 7,130 CZK/month**. Plus advertised from **€2,100/month**. [CZ pricing](https://www.shopify.com/cz/pricing). | Large ecosystem, international operations, hosted checkout, AI assistant and agent-commerce tooling. | Compare complete local integration/app costs and payment terms. CZ/SK accounting and delivery workflows are a plausible area for a simpler packaged alternative. |
| **WooCommerce** | Core software **free/open source**. Hosting, maintenance, extensions and implementation are separate. [Official pricing](https://woocommerce.com/pricing/). | Ownership, WordPress integration, extensibility and flexibility. | Merchant/operator carries more responsibility for integration compatibility, maintenance and performance. A managed service can win on predictable operations. |
| **Shopware** | Community Edition **free**; Rise from **€600/month**; Evolve from **€2,400/month**; Beyond custom. Published prices exclude VAT. [Pricing](https://www.shopware.com/en/pricing/). | Flexible commerce platform with commercial B2B/advanced capabilities and AI tooling. | Paid entry and implementation effort make it a different proposition from a low-cost small-shop SaaS. More relevant as merchants grow. |
| **BigCommerce** | From **1 June 2026**: **Core $39 / Growth $105 / Scale $399 monthly**; annual equivalents **$29 / $79 / $299**. Performance custom. [2026 pricing update](https://www.bigcommerce.com/dm/plan-pricing-updates-2026/). | Hosted commerce, APIs and growth-oriented features. | New **Open Payment Provider fees**: **2% / 1% / 0.6%** on eligible order GMV. Core/Growth thresholds are **$30k/$100k**; Scale has its own monthly overage structure. “BigCommerce has no transaction fees” is now an unsafe blanket claim. |
| **Genstore — AI-native newcomer** | Current public page: **Free; Lite $25; Growth $75; Scale $199/month**. Credit allowances apply; paid tiers show third-party payment fees **2% / 1% / 0.6%**. [Pricing](https://www.genstore.ai/pricing). | Markets integrated AI store creation and operational agents; first-party documentation and product updates exist. [Product update](https://www.genstore.ai/blog/product-updates-march-2026), **12 March 2026**. | CZ/SK merchant onboarding, local logistics/accounting, compliance coverage and production scale are **unverified**. Treat as evidence that AI-native positioning is competitive, not as a proven local substitute. |

### D2. Merchant complaints: evidence quality

Public complaints are useful interview hypotheses, not representative prevalence statistics.

| Platform | Evidence-supported theme | Qualification |
|---|---|---|
| **Shopify** | Subscription plus app costs, and mixed support experiences. | Reported in review aggregation and independent testing. [Capterra](https://www.capterra.com/p/83891/Shopify/), n.d.; [TechRadar review](https://www.techradar.com/reviews/shopify), **2026**. Frequency among CZ/SK merchants specifically: **unverified**. |
| **BigCommerce** | Opposition to 2026 payment-provider fees and pricing changes. | Actual fee changes are confirmed first-party; merchant dissatisfaction is anecdotal. [Merchant discussion](https://www.reddit.com/r/bigcommerce/comments/1sej5r4/2_penalty_fee_for_not_using_bcs_approved/), **April 2026**. |
| **Shoptet** | Add-on expense and limits on customization. | Pricing/Premium distinctions support the underlying constraints. Review sites also discuss add-on expense, but a representative merchant sample was **not verified**. [Review](https://www.webhostingcentrum.cz/shoptet-recenze/), **2026 edition; exact revision date unverified**. |
| **WooCommerce** | Plugin complexity, maintenance and performance concerns. | Review themes and architectural tradeoffs, not evidence that every store suffers them. [Capterra reviews](https://www.capterra.com/p/225601/WooCommerce/reviews/), n.d. |
| **Upgates / Shopware** | Potential friction from learning curve, customization effort or implementation cost. | **Analysis, not a verified ranking of common complaints.** A sufficiently strong current local complaint sample was not established. |

### D3. Where a new platform could credibly win — analysis

1. **Local operational completeness.** A merchant can take payment, select a pickup point, print a label, issue/export the right document and process a return without assembling several unrelated integrations.

2. **AI that completes reviewable work.** Import a supplier catalog, normalize variants, propose translations, create a promotion with a valid prior-price basis, and preview storefront changes. Measure time saved and corrections required.

3. **Safe storefront customization.** Preserve accessibility, checkout reliability, product data and performance when AI changes appearance. Provide preview, version history and rollback.

4. **Transparent total cost.** Publish limits for AI, email, products, integrations and overages. Compare like-for-like merchant stacks; Upgates already competes on included features.

5. **Accessible B2B for ordinary businesses.** Company accounts, approved price lists, quantity breaks, VAT validation, purchase-order references, invoice terms and accounting exports can be more useful than enterprise-scale procurement features.

6. **Migration quality.** Reliable imports, identifiers, redirects, catalog validation and accounting reconciliation reduce switching risk.

7. **Practical agent readiness.** Publish reliable data and controlled commerce tools. Sell proven functionality; do not claim guaranteed placement in AI shopping results.

**Weak positioning:** generic AI copywriting, an `llms.txt` file, or MCP alone. Shopify already has substantial UCP/MCP infrastructure, and Shoptet exposes AI/MCP features. [Shopify Global Catalog](https://shopify.dev/docs/agents/catalog/global-catalog), [Shoptet MCP](https://podpora.shoptet.cz/shoptet-mcp/), n.d.

**Commercial hypothesis requiring validation:** a focused segment of CZ/SK merchants will pay for fewer operational problems and faster safe changes. This research does not establish willingness to switch or a conversion advantage; validate those through paid pilots.

## One-page launch summary

The following priorities synthesize the evidence above.

### Must-have for v1

| Area | Minimum launch requirement |
|---|---|
| **Selling fundamentals** | CZ/SK language, CZK/EUR, products/variants, stock, full totals, orders, refunds and dependable transactional email. |
| **Payments** | One suitable PSP with verified payment status/refunds; bank transfer; configurable COD where supported. |
| **Delivery** | Packeta/pickup selection and a working home-delivery route; labels and tracking. |
| **Consumer compliance** | Merchant identity/contact information, terms, total prices, payment-obligation wording, withdrawal/return/complaint flows. Include online withdrawal functionality for the two-country product. |
| **Promotions and reviews** | Price-history storage and correct discount calculations; verification disclosure if reviews are enabled. |
| **Product safety** | GPSR manufacturer/responsible-person/product-warning fields and visible presentation where applicable. |
| **Accessibility** | Accessible default storefront and checkout; AI edits must preserve it. Merchant exemptions do not justify an inaccessible platform default. |
| **Privacy and marketing** | Processor agreement, subprocessor/transfer information, retention and rights support; consent enforcement across browser and server; lawful email audiences and suppression. |
| **Tax and accounting** | VAT/non-VAT merchants, relevant tax rates, company/VAT identifiers, proper document records and useful accounting export. Destination VAT/OSS support when applicable. |
| **Discovery** | Accurate Product/Offer markup; Google Merchant Center and Heureka feeds; Zboží.cz for CZ merchants. |
| **AI transparency** | Identify customer-facing AI; assess provider/deployer and content-marking duties before launch. |
| **Platform operations** | Security, backups, tenant isolation, auditable financial changes and applicable DSA hosting procedures. |

### Should-have immediately after—or conditionally before—launch

- One cloud invoicing connector per target segment; Pohoda/Money export.
- Heureka verified reviews; additional carriers and PSPs.
- B2B accounts, price lists, VAT validation and invoice terms.
- Consent-aware recommendations and abandoned-cart flows.
- Public catalog/cart MCP or UCP adapter, with human checkout handoff.
- `llms.txt` and infrastructure-backed signed-agent verification.
- **SK B2B e-invoicing readiness for 1 January 2027.**
- **CZ EET 2.0 readiness for affected payment flows**, following promulgation and final scope verification.
- If these obligations apply when a merchant goes live, they become **must-have for that merchant**, not optional backlog.

### Later

- Fully delegated ACP/AP2 payments and direct Visa/Mastercard agent-program integrations.
- `auth.md` registration where customer demand justifies it.
- Poland: BLIK/P24, local logistics and Polish compliance.
- Advanced ERP synchronization, enterprise B2B and Adyen.
- Marketplace functionality, after DSA/GPSR/VAT-role assessment.
- Broad EU country coverage with maintained national rules.

**Recommended launch proposition:** a dependable CZ/SK commerce platform with excellent local operations, safe AI-assisted customization and a usable path for agents to discover products and prepare purchases.