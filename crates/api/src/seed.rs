//! Demo shop (`api admin seed-demo`, `make seed`): tenant `demo` with markets CZ
//! (`demo.localhost`, CZK, cs) and SK (`demo-sk.localhost`, EUR, sk), price lists, 60
//! products with variants, categories, parameters, stock, a running sale and a published
//! coupon. Everything goes through the commerce services (same validation, audit and events as
//! the Admin API); images go through the real media pipeline (upload -> complete -> process).
//!
//! Idempotent: every step checks whether its result already exists, so a rerun (or a rerun
//! after a failure) completes the shop without duplicating anything.

use std::collections::BTreeMap;
use std::io::Cursor;

use anyhow::{Context as _, anyhow};
use chrono::{Duration, Utc};
use commerce::catalog::I18n;
use commerce::catalog::categories::{self, CategoryTranslation, NewCategory};
use commerce::catalog::parameters::{self, ParameterInput, ParameterKind};
use commerce::catalog::products::{
    self, Gpsr, GpsrParty, OptionValue, ParameterValue, ProductInput, ProductMedia, ProductOption,
    ProductStatus, ProductTranslation, UnitMeasure, VariantInput,
};
use commerce::inventory::{self, Adjustment, LevelSettings};
use commerce::markets::{self, NewMarket, TaxMode};
use commerce::media::{self, NewUpload};
use commerce::money::Currency;
use commerce::payments::{MethodKind, PaymentMethodInput};
use commerce::pricing::{self, NewPriceList, PriceChangeReason, PriceItem, PriceUpsert};
use commerce::promotions::coupons::{self, CouponInput};
use commerce::promotions::sales::{self, SaleDiscount, SaleInput, SaleTargets};
use commerce::shipping::{self, Carrier, ShippingMethodInput, WeightTier};
use commerce::storefront::listing::fold;
use commerce::tax::{self, DistanceSalesMode, TaxProfileInput};
use commerce::{search, tenancy, themes};
use image::{DynamicImage, ImageFormat};
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::{TenantTx, tenant_tx};
use platform::storage::Storage;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub const TENANT: &str = "demo";
const ACTOR: &str = "seed";
const SK_HOST: &str = "demo-sk.localhost";

/// A color variant of a fixture photo: the image is the fixture with its hue rotated.
struct Color {
    code: &'static str,
    cs: &'static str,
    sk: &'static str,
    photo: &'static str,
    hue: i32,
}

const fn c(
    code: &'static str,
    cs: &'static str,
    sk: &'static str,
    photo: &'static str,
    hue: i32,
) -> Color {
    Color {
        code,
        cs,
        sk,
        photo,
        hue,
    }
}

const TEE: &[Color] = &[
    c("zelena", "Lesní zelená", "Lesná zelená", "tee-forest", 0),
    c("inkoustova", "Inkoustová", "Atramentová", "tee-ink", 0),
    c("piskova", "Písková", "Piesková", "tee-sand", 0),
    c("vinova", "Vínová", "Vínová", "tee-forest", 200),
    c(
        "petrolejova",
        "Petrolejová",
        "Petrolejová",
        "tee-forest",
        60,
    ),
];
const HOODIE: &[Color] = &[
    c("popelava", "Popelavá", "Popolavá", "hoodie-ash", 0),
    c("cihlova", "Cihlová", "Tehlová", "hoodie-clay", 0),
    c("tyrkysova", "Tyrkysová", "Tyrkysová", "hoodie-clay", 170),
];
const CAP: &[Color] = &[
    c("olivova", "Olivová", "Olivová", "cap-olive", 0),
    c("horcicova", "Hořčicová", "Horčicová", "cap-olive", -40),
    c("modra", "Modrá", "Modrá", "cap-olive", 150),
];
const BAG: &[Color] = &[
    c("prirodni", "Přírodní", "Prírodná", "bag-natural", 0),
    c("salvejova", "Šalvějová", "Šalviová", "bag-natural", 80),
    c("ruzova", "Pudrová", "Púdrová", "bag-natural", -60),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tee,
    Hoodie,
    Cap,
    Bag,
}

impl Kind {
    fn colors(self) -> &'static [Color] {
        match self {
            Self::Tee => TEE,
            Self::Hoodie => HOODIE,
            Self::Cap => CAP,
            Self::Bag => BAG,
        }
    }

    fn category(self) -> &'static str {
        match self {
            Self::Tee => "trika",
            Self::Hoodie => "mikiny",
            Self::Cap => "cepice",
            Self::Bag => "tasky",
        }
    }

    fn apparel(self) -> bool {
        matches!(self, Self::Tee | Self::Hoodie)
    }

    /// Base price in whole CZK and the step between products.
    fn price(self, i: usize) -> i64 {
        let (base, step, span) = match self {
            Self::Tee => (390, 100, 6),
            Self::Hoodie => (1190, 100, 8),
            Self::Cap => (290, 100, 5),
            Self::Bag => (490, 200, 8),
        };
        base + step * i64::try_from(i % span).unwrap_or(0)
    }
}

/// (kind, Czech name, Slovak name, pieces in a multipack)
const PRODUCTS: &[(Kind, &str, &str, u8)] = &[
    (Kind::Tee, "Tričko Basic", "Tričko Basic", 1),
    (Kind::Tee, "Tričko Oversize", "Tričko Oversize", 1),
    (Kind::Tee, "Tričko s kapsičkou", "Tričko s vreckom", 1),
    (
        Kind::Tee,
        "Tričko s dlouhým rukávem",
        "Tričko s dlhým rukávom",
        1,
    ),
    (Kind::Tee, "Tričko Henley", "Tričko Henley", 1),
    (Kind::Tee, "Tričko Raglan", "Tričko Raglan", 1),
    (Kind::Tee, "Polo tričko Piqué", "Polo tričko Piqué", 1),
    (Kind::Tee, "Tílko Sport", "Tielko Šport", 1),
    (Kind::Tee, "Tričko Merino", "Tričko Merino", 1),
    (Kind::Tee, "Tričko Slub", "Tričko Slub", 1),
    (Kind::Tee, "Tričko Heavy", "Tričko Heavy", 1),
    (Kind::Tee, "Tričko do V", "Tričko do V", 1),
    (Kind::Tee, "Tričko Crop", "Tričko Crop", 1),
    (Kind::Tee, "Tričko Ringer", "Tričko Ringer", 1),
    (Kind::Tee, "Lněné tričko", "Ľanové tričko", 1),
    (Kind::Tee, "Dětské tričko", "Detské tričko", 1),
    (Kind::Tee, "Tričko Basic 3 ks", "Tričko Basic 3 ks", 3),
    (Kind::Tee, "Tričko Oversize 2 ks", "Tričko Oversize 2 ks", 2),
    (Kind::Tee, "Tričko Organic Logo", "Tričko Organic Logo", 1),
    (Kind::Tee, "Tričko Vafle", "Tričko Vafľa", 1),
    (
        Kind::Hoodie,
        "Mikina s kapucí Klasik",
        "Mikina s kapucňou Klasik",
        1,
    ),
    (Kind::Hoodie, "Mikina Crew", "Mikina Crew", 1),
    (Kind::Hoodie, "Mikina na zip", "Mikina na zips", 1),
    (Kind::Hoodie, "Mikina Oversize", "Mikina Oversize", 1),
    (Kind::Hoodie, "Mikina Fleece", "Mikina Fleece", 1),
    (
        Kind::Hoodie,
        "Mikina s kapucí Heavy",
        "Mikina s kapucňou Heavy",
        1,
    ),
    (Kind::Hoodie, "Mikina Polozip", "Mikina Polozips", 1),
    (Kind::Hoodie, "Mikina Merino", "Mikina Merino", 1),
    (Kind::Hoodie, "Mikina Vafle", "Mikina Vafľa", 1),
    (Kind::Hoodie, "Mikina Lehká", "Mikina Ľahká", 1),
    (Kind::Hoodie, "Dětská mikina", "Detská mikina", 1),
    (Kind::Hoodie, "Mikina Kardigan", "Mikina Kardigán", 1),
    (
        Kind::Hoodie,
        "Mikina s kapucí Zip Heavy",
        "Mikina s kapucňou Zips Heavy",
        1,
    ),
    (Kind::Hoodie, "Mikina College", "Mikina College", 1),
    (Kind::Hoodie, "Mikina Teplá", "Mikina Teplá", 1),
    (Kind::Hoodie, "Mikina Crop", "Mikina Crop", 1),
    (Kind::Cap, "Čepice Merino", "Čiapka Merino", 1),
    (Kind::Cap, "Kšiltovka Classic", "Šiltovka Classic", 1),
    (Kind::Cap, "Kulich Rib", "Čiapka Rib", 1),
    (Kind::Cap, "Čepice Fisherman", "Čiapka Fisherman", 1),
    (Kind::Cap, "Kšiltovka Trucker", "Šiltovka Trucker", 1),
    (Kind::Cap, "Klobouk Bucket", "Klobúk Bucket", 1),
    (Kind::Cap, "Čepice s bambulí", "Čiapka s brmbolcom", 1),
    (Kind::Cap, "Kšiltovka Manšestr", "Šiltovka Menčester", 1),
    (Kind::Cap, "Čelenka Merino", "Čelenka Merino", 1),
    (Kind::Cap, "Dětský kulich", "Detská čiapka", 1),
    (Kind::Cap, "Čepice Lehká", "Čiapka Ľahká", 1),
    (Kind::Cap, "Lněná kšiltovka", "Ľanová šiltovka", 1),
    (Kind::Bag, "Taška Shopper", "Taška Shopper", 1),
    (Kind::Bag, "Batoh Roll-top", "Ruksak Roll-top", 1),
    (Kind::Bag, "Plátěná taška", "Plátená taška", 1),
    (Kind::Bag, "Taška přes rameno", "Taška cez rameno", 1),
    (Kind::Bag, "Ledvinka", "Ľadvinka", 1),
    (Kind::Bag, "Sportovní taška", "Športová taška", 1),
    (Kind::Bag, "Taška na notebook", "Taška na notebook", 1),
    (Kind::Bag, "Batoh Mini", "Ruksak Mini", 1),
    (
        Kind::Bag,
        "Skládací nákupní taška",
        "Skladacia nákupná taška",
        1,
    ),
    (Kind::Bag, "Víkendová taška", "Víkendová taška", 1),
    (Kind::Bag, "Kosmetická taštička", "Kozmetická taštička", 1),
    (Kind::Bag, "Taška na jógu", "Taška na jogu", 1),
];

const SIZES: &[&str] = &["S", "M", "L", "XL"];

fn i18n(cs: &str, sk: &str) -> I18n {
    I18n::from([
        ("cs".to_owned(), cs.to_owned()),
        ("sk".to_owned(), sk.to_owned()),
    ])
}

/// `Tričko s dlouhým rukávem` -> `tricko-s-dlouhym-rukavem`.
pub fn slugify(name: &str) -> String {
    let folded = fold(name);
    let mut out = String::with_capacity(folded.len());
    for ch in folded.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Photo grain (xorshift noise, deterministic per image), so encoded variants weigh about as
/// much as real photos and the performance gate does not measure flat illustrations.
fn grain(img: &mut image::RgbImage, seed: u64) {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for p in img.pixels_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = (i16::from(u8::try_from(x >> 56).unwrap_or(128)) - 128) / 5;
        for c in &mut p.0 {
            *c = u8::try_from((i16::from(*c) + n).clamp(0, 255)).unwrap_or(*c);
        }
    }
}

/// Whole CZK -> minor units of EUR at ~25 CZK/EUR, ending in .90.
fn eur_minor(czk: i64) -> i64 {
    (czk / 25) * 100 + 90
}

/// What the seed printed at the end.
#[derive(Debug, Default)]
pub struct Summary {
    pub tenant_id: Uuid,
    pub products_created: usize,
    pub images_created: usize,
}

/// The fixture photos (`fixtures/images/demo`), compiled in so the distroless api image can
/// seed without a data directory.
const PHOTOS: &[(&str, &[u8])] = &[
    (
        "tee-forest",
        include_bytes!("../../../fixtures/images/demo/tee-forest.jpg"),
    ),
    (
        "tee-ink",
        include_bytes!("../../../fixtures/images/demo/tee-ink.jpg"),
    ),
    (
        "tee-sand",
        include_bytes!("../../../fixtures/images/demo/tee-sand.jpg"),
    ),
    (
        "hoodie-ash",
        include_bytes!("../../../fixtures/images/demo/hoodie-ash.jpg"),
    ),
    (
        "hoodie-clay",
        include_bytes!("../../../fixtures/images/demo/hoodie-clay.jpg"),
    ),
    (
        "cap-olive",
        include_bytes!("../../../fixtures/images/demo/cap-olive.jpg"),
    ),
    (
        "bag-natural",
        include_bytes!("../../../fixtures/images/demo/bag-natural.jpg"),
    ),
];

pub struct Seeder<'a> {
    pub db: &'a PgPool,
    pub storage: &'a Storage,
}

impl Seeder<'_> {
    async fn tx(&self, tenant_id: Uuid) -> anyhow::Result<TenantTx> {
        Ok(tenant_tx(self.db, tenant_id).await?)
    }

    /// Runs every step. `tenant_id` must exist (created by the CLI with its owner).
    pub async fn run(&self, tenant_id: Uuid) -> anyhow::Result<Summary> {
        let mut summary = Summary {
            tenant_id,
            ..Summary::default()
        };
        let (cz, sk) = self.markets(tenant_id).await?;
        self.tax_profile(tenant_id).await?;
        let (czk_list, eur_list) = self.price_lists(tenant_id, cz, sk).await?;
        let cats = self.categories(tenant_id).await?;
        let params = self.parameters(tenant_id).await?;
        let (images, created) = self.images(tenant_id).await?;
        summary.images_created = created;
        for (i, spec) in PRODUCTS.iter().enumerate() {
            if self
                .product(
                    tenant_id, i, spec, &cats, &params, &images, czk_list, eur_list,
                )
                .await?
            {
                summary.products_created += 1;
            }
        }
        self.promotions(tenant_id, &cats).await?;
        self.checkout_methods(tenant_id, cz, sk).await?;
        self.content(tenant_id).await?;
        self.history(tenant_id, cz, sk).await?;
        let mut tx = self.tx(tenant_id).await?;
        themes::assign_default(&mut tx, ACTOR).await?;
        // A full search index build (WP7) for the demo catalog, run by the worker. Product
        // events index incrementally too; the rebuild makes a rerun converge as well.
        let version = search::next_version(&mut *tx).await?;
        platform::queue::enqueue(&mut *tx, &search::manual_rebuild_job(tenant_id, version)).await?;
        // Export feeds right away instead of at the end of the debounce window (WP13a).
        platform::queue::enqueue(
            &mut *tx,
            &commerce::feeds::export::job(tenant_id, Utc::now(), false),
        )
        .await?;
        tx.commit().await?;
        Ok(summary)
    }

    async fn markets(&self, tenant_id: Uuid) -> anyhow::Result<(Uuid, Uuid)> {
        let mut tx = self.tx(tenant_id).await?;
        let existing = markets::list(&mut tx).await?;
        let cz = existing
            .iter()
            .find(|m| m.code == "cz")
            .map(|m| m.id)
            .ok_or_else(|| anyhow!("the demo tenant has no cz market"))?;
        let sk = match existing.iter().find(|m| m.code == "sk") {
            Some(m) => m.id,
            None => {
                markets::create(
                    &mut tx,
                    ACTOR,
                    &NewMarket {
                        code: "sk".into(),
                        name: "Slovensko".into(),
                        country_codes: vec!["SK".into()],
                        currency: "EUR".into(),
                        default_locale: "sk".into(),
                        // A second locale exercises locale prefixes (`demo-sk.localhost/cs/...`).
                        locales: vec!["sk".into(), "cs".into()],
                        tax_mode: TaxMode::Gross,
                        is_default: false,
                    },
                )
                .await?
                .id
            }
        };
        tx.commit().await?;
        if tenancy::domain(self.db, SK_HOST).await.is_err() {
            tenancy::add_domain(self.db, TENANT, SK_HOST, Some("sk"), true).await?;
        }
        Ok((cz, sk))
    }

    async fn tax_profile(&self, tenant_id: Uuid) -> anyhow::Result<()> {
        let mut tx = self.tx(tenant_id).await?;
        if tax::get(&mut tx).await?.is_none() {
            // An OSS-registered Czech VAT payer: Slovak customers pay Slovak VAT (A3).
            tax::upsert(
                &mut tx,
                ACTOR,
                &TaxProfileInput {
                    establishment_country: "CZ".into(),
                    vat_payer: true,
                    vat_id: Some("CZ12345678".into()),
                    sk_ic_dph: None,
                    distance_sales_mode: DistanceSalesMode::Destination,
                    confirm_origin_threshold: false,
                    cash_rounding_in_vat_base: false,
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn price_lists(
        &self,
        tenant_id: Uuid,
        cz: Uuid,
        sk: Uuid,
    ) -> anyhow::Result<(Uuid, Uuid)> {
        let mut tx = self.tx(tenant_id).await?;
        let lists = pricing::list_price_lists(&mut tx).await?;
        let mut ensure = async |code: &str,
                                name: &str,
                                currency: Currency,
                                market: Uuid|
               -> anyhow::Result<Uuid> {
            if let Some(l) = lists.iter().find(|l| l.code == code) {
                return Ok(l.id);
            }
            Ok(pricing::create_price_list(
                &mut tx,
                ACTOR,
                &NewPriceList {
                    code: code.into(),
                    name: name.into(),
                    currency,
                    market_ids: vec![market],
                },
            )
            .await?
            .id)
        };
        let czk = ensure("czk", "Ceník CZK", Currency::Czk, cz).await?;
        let eur = ensure("eur", "Cenník EUR", Currency::Eur, sk).await?;
        tx.commit().await?;
        Ok((czk, eur))
    }

    /// Category slug (cs) -> id.
    async fn categories(&self, tenant_id: Uuid) -> anyhow::Result<BTreeMap<String, Uuid>> {
        // (cs slug, parent cs slug, cs name, sk name, sk slug)
        let tree: &[(&str, Option<&str>, &str, &str, &str)] = &[
            ("obleceni", None, "Oblečení", "Oblečenie", "oblecenie"),
            ("doplnky", None, "Doplňky", "Doplnky", "doplnky"),
            ("trika", Some("obleceni"), "Trička", "Tričká", "tricka"),
            ("mikiny", Some("obleceni"), "Mikiny", "Mikiny", "mikiny"),
            (
                "cepice",
                Some("doplnky"),
                "Čepice a kšiltovky",
                "Čiapky a šiltovky",
                "ciapky",
            ),
            (
                "tasky",
                Some("doplnky"),
                "Tašky a batohy",
                "Tašky a ruksaky",
                "tasky",
            ),
        ];
        let mut tx = self.tx(tenant_id).await?;
        let mut ids = BTreeMap::new();
        for (slug, parent, cs, sk, sk_slug) in tree {
            let existing = sqlx::query_scalar!(
                "SELECT category_id FROM category_translations WHERE locale = 'cs' AND slug = $1",
                slug
            )
            .fetch_optional(&mut *tx)
            .await?;
            let id = match existing {
                Some(id) => id,
                None => {
                    let translation =
                        |locale: &str, name: &str, slug: &str, text: &str| CategoryTranslation {
                            locale: locale.into(),
                            name: name.into(),
                            slug: slug.into(),
                            description_html: format!("<p>{text}</p>"),
                            seo_title: None,
                            seo_description: None,
                        };
                    categories::create(
                        &mut tx,
                        ACTOR,
                        &NewCategory {
                            parent_id: parent.and_then(|p| ids.get(p).copied()),
                            image_asset_id: None,
                            translations: vec![
                                translation(
                                    "cs",
                                    cs,
                                    slug,
                                    &format!("{cs} z přírodních materiálů, šité v Evropě."),
                                ),
                                translation(
                                    "sk",
                                    sk,
                                    sk_slug,
                                    &format!("{sk} z prírodných materiálov, šité v Európe."),
                                ),
                            ],
                        },
                    )
                    .await?
                    .id
                }
            };
            ids.insert((*slug).to_owned(), id);
        }
        tx.commit().await?;
        Ok(ids)
    }

    /// Parameter key -> id.
    async fn parameters(&self, tenant_id: Uuid) -> anyhow::Result<BTreeMap<String, Uuid>> {
        let defs = [
            (
                "material",
                "Materiál",
                "Materiál",
                ParameterKind::Text,
                None,
                true,
            ),
            ("strih", "Střih", "Strih", ParameterKind::Text, None, true),
            (
                "gramaz",
                "Gramáž",
                "Gramáž",
                ParameterKind::Number,
                Some("g/m²"),
                false,
            ),
            (
                "zeme-vyroby",
                "Země výroby",
                "Krajina výroby",
                ParameterKind::Text,
                None,
                false,
            ),
        ];
        let mut tx = self.tx(tenant_id).await?;
        let mut ids = BTreeMap::new();
        for (key, cs, sk, kind, unit, filterable) in defs {
            let existing = sqlx::query_scalar!("SELECT id FROM parameters WHERE key = $1", key)
                .fetch_optional(&mut *tx)
                .await?;
            let id = match existing {
                Some(id) => id,
                None => {
                    parameters::create(
                        &mut tx,
                        ACTOR,
                        &ParameterInput {
                            key: key.into(),
                            name_i18n: i18n(cs, sk),
                            kind,
                            unit: unit.map(str::to_owned),
                            filterable,
                        },
                    )
                    .await?
                    .id
                }
            };
            ids.insert(key.to_owned(), id);
        }
        tx.commit().await?;
        Ok(ids)
    }

    /// One front and one back (mirrored) photo per color: `"<photo>:<hue>"` -> [front, back].
    async fn images(
        &self,
        tenant_id: Uuid,
    ) -> anyhow::Result<(BTreeMap<String, [Uuid; 2]>, usize)> {
        let mut out = BTreeMap::new();
        let mut created = 0;
        for color in [TEE, HOODIE, CAP, BAG].iter().flat_map(|p| p.iter()) {
            let key = format!("{}:{}", color.photo, color.hue);
            if out.contains_key(&key) {
                continue;
            }
            let mut pair = [Uuid::nil(); 2];
            for (side, slot) in pair.iter_mut().enumerate() {
                let filename = format!("demo-{}-{}-{side}.jpg", color.photo, color.hue);
                let mut tx = self.tx(tenant_id).await?;
                let existing = sqlx::query_scalar!(
                    "SELECT id FROM assets WHERE filename = $1 AND status = 'ready'",
                    filename
                )
                .fetch_optional(&mut *tx)
                .await?;
                tx.commit().await?;
                *slot = match existing {
                    Some(id) => id,
                    None => {
                        created += 1;
                        self.upload(tenant_id, &filename, &Self::render(color, side == 1)?)
                            .await?
                    }
                };
            }
            out.insert(key, pair);
        }
        Ok((out, created))
    }

    fn render(color: &Color, mirrored: bool) -> anyhow::Result<Vec<u8>> {
        let bytes = PHOTOS
            .iter()
            .find(|(name, _)| *name == color.photo)
            .map(|(_, b)| *b)
            .ok_or_else(|| anyhow!("no fixture photo {}", color.photo))?;
        let mut img = image::load_from_memory(bytes).context("decode fixture photo")?;
        if color.hue != 0 {
            img = DynamicImage::ImageRgba8(image::imageops::huerotate(&img, color.hue));
        }
        if mirrored {
            img = img.fliph();
        }
        let mut rgb = img.to_rgb8();
        grain(
            &mut rgb,
            u64::from(color.hue.unsigned_abs()) + u64::from(mirrored),
        );
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(rgb).write_to(&mut out, ImageFormat::Jpeg)?;
        Ok(out.into_inner())
    }

    /// The real pipeline: pending asset + upload -> verify -> re-encode variants.
    async fn upload(&self, tenant_id: Uuid, filename: &str, bytes: &[u8]) -> anyhow::Result<Uuid> {
        let mut tx = self.tx(tenant_id).await?;
        let upload = media::create_upload(
            &mut tx,
            self.storage,
            ACTOR,
            &NewUpload {
                filename: Some(filename.into()),
                content_type: "image/jpeg".into(),
                size: u64::try_from(bytes.len())?,
            },
        )
        .await?;
        tx.commit().await?;
        let id = upload.asset.id;
        // What the browser's presigned PUT does.
        self.storage
            .private
            .put(
                &media::upload_key(tenant_id, id),
                PutPayload::from(bytes.to_vec()),
            )
            .await?;
        let mut tx = self.tx(tenant_id).await?;
        media::complete(&mut tx, self.storage, ACTOR, id).await?;
        tx.commit().await?;
        // The worker would pick the job up; running it here makes the seed deterministic
        // (the queued job then finds the asset ready and skips).
        media::process(self.db, self.storage, tenant_id, id).await?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn product(
        &self,
        tenant_id: Uuid,
        i: usize,
        (kind, cs, sk, pack): &(Kind, &str, &str, u8),
        cats: &BTreeMap<String, Uuid>,
        params: &BTreeMap<String, Uuid>,
        images: &BTreeMap<String, [Uuid; 2]>,
        czk_list: Uuid,
        eur_list: Uuid,
    ) -> anyhow::Result<bool> {
        let sku_prefix = format!("LN-{:03}", i + 1);
        let mut tx = self.tx(tenant_id).await?;
        let exists = sqlx::query_scalar!(
            "SELECT count(*) AS \"n!\" FROM variants WHERE sku LIKE $1",
            format!("{sku_prefix}-%")
        )
        .fetch_one(&mut *tx)
        .await?;
        if exists > 0 {
            tx.commit().await?;
            return Ok(false);
        }
        let palette = kind.colors();
        // 2-3 colors per product, rotating through the palette.
        let n_colors = 2 + i % 2;
        let colors: Vec<&Color> = (0..n_colors.min(palette.len()))
            .map(|k| &palette[(i + k) % palette.len()])
            .collect();
        let sizes: &[&str] = if kind.apparel() { SIZES } else { &[] };

        let mut options = vec![ProductOption {
            code: "barva".into(),
            name_i18n: i18n("Barva", "Farba"),
            values: colors
                .iter()
                .map(|c| OptionValue {
                    code: c.code.into(),
                    name_i18n: i18n(c.cs, c.sk),
                })
                .collect(),
        }];
        if !sizes.is_empty() {
            options.push(ProductOption {
                code: "velikost".into(),
                name_i18n: i18n("Velikost", "Veľkosť"),
                values: sizes
                    .iter()
                    .map(|s| OptionValue {
                        code: s.to_ascii_lowercase(),
                        name_i18n: i18n(s, s),
                    })
                    .collect(),
            });
        }
        let mut variants = Vec::new();
        for (ci, color) in colors.iter().enumerate() {
            let size_codes: Vec<Option<&str>> = if sizes.is_empty() {
                vec![None]
            } else {
                sizes.iter().map(|s| Some(*s)).collect()
            };
            for size in size_codes {
                let mut ov = BTreeMap::from([("barva".to_owned(), color.code.to_owned())]);
                let mut sku = format!("{sku_prefix}-{}", color.code.to_ascii_uppercase());
                if let Some(s) = size {
                    ov.insert("velikost".into(), s.to_ascii_lowercase());
                    sku.push('-');
                    sku.push_str(s);
                }
                variants.push(VariantInput {
                    id: None,
                    sku: sku.chars().take(64).collect(),
                    ean: None,
                    option_values: ov,
                    weight_g: Some(match kind {
                        Kind::Tee => 180,
                        Kind::Hoodie => 520,
                        Kind::Cap => 90,
                        Kind::Bag => 400,
                    }),
                    is_default: ci == 0 && (size.is_none() || size == Some("M")),
                });
            }
        }
        let mut media_items = Vec::new();
        for (ci, color) in colors.iter().enumerate() {
            let pair = images
                .get(&format!("{}:{}", color.photo, color.hue))
                .ok_or_else(|| anyhow!("image for {} missing", color.code))?;
            let first_sku = variants
                .iter()
                .find(|v| v.option_values.get("barva").map(String::as_str) == Some(color.code))
                .map(|v| v.sku.clone());
            // The first color's front and back lead the gallery (card image + hover image).
            let sides: &[usize] = if ci == 0 { &[0, 1] } else { &[0] };
            for side in sides {
                media_items.push(ProductMedia {
                    asset_id: pair[*side],
                    variant_sku: if *side == 0 { first_sku.clone() } else { None },
                    alt_i18n: i18n(
                        &format!("{cs} – {}", color.cs.to_lowercase()),
                        &format!("{sk} – {}", color.sk.to_lowercase()),
                    ),
                });
            }
        }
        let (material_cs, material_sk) = match (kind, i % 3) {
            (Kind::Tee | Kind::Hoodie, 0) => ("Organická bavlna", "Organická bavlna"),
            (Kind::Tee | Kind::Hoodie, 1) => ("Merino vlna", "Merino vlna"),
            (Kind::Tee | Kind::Hoodie, _) => ("Len", "Ľan"),
            (Kind::Cap, 0) => ("Merino vlna", "Merino vlna"),
            (Kind::Cap, _) => ("Organická bavlna", "Organická bavlna"),
            (Kind::Bag, 0) => ("Recyklovaný polyester", "Recyklovaný polyester"),
            (Kind::Bag, _) => ("Bavlněné plátno", "Bavlnené plátno"),
        };
        let mut parameters = vec![
            ParameterValue {
                parameter_id: params["material"],
                variant_sku: None,
                value: json!({ "cs": material_cs, "sk": material_sk }),
            },
            ParameterValue {
                parameter_id: params["zeme-vyroby"],
                variant_sku: None,
                value: if i % 4 == 3 {
                    json!({ "cs": "Portugalsko", "sk": "Portugalsko" })
                } else {
                    json!({ "cs": "Česko", "sk": "Česko" })
                },
            },
        ];
        if kind.apparel() {
            let (fit_cs, fit_sk) = [
                ("Regular", "Regular"),
                ("Oversize", "Oversize"),
                ("Slim", "Slim"),
            ][i % 3];
            parameters.push(ParameterValue {
                parameter_id: params["strih"],
                variant_sku: None,
                value: json!({ "cs": fit_cs, "sk": fit_sk }),
            });
            parameters.push(ParameterValue {
                parameter_id: params["gramaz"],
                variant_sku: None,
                value: json!(if *kind == Kind::Tee {
                    160 + 20 * (i % 4)
                } else {
                    280 + 40 * (i % 3)
                }),
            });
        }
        let description = |name: &str, material: &str, cs_lang: bool| {
            if cs_lang {
                format!(
                    "<p>{name} z materiálu {m}. Pohodlný střih, pečlivě ušité švy a barvy bez azobarviv.</p>\
                     <ul><li>Materiál: {m}</li><li>Předsrážená látka, po vyprání nesrazí</li><li>Praní na 30 °C naruby</li></ul>",
                    m = material.to_lowercase()
                )
            } else {
                format!(
                    "<p>{name} z materiálu {m}. Pohodlný strih, starostlivo ušité švy a farby bez azofarbív.</p>\
                     <ul><li>Materiál: {m}</li><li>Predzrazená látka, po praní sa nezrazí</li><li>Pranie na 30 °C naruby</li></ul>",
                    m = material.to_lowercase()
                )
            }
        };
        let input = ProductInput {
            status: ProductStatus::Active,
            brand: Some("Lnen & Co.".into()),
            gpsr: Gpsr {
                manufacturer: Some(GpsrParty {
                    name: "Lnen & Co. s.r.o.".into(),
                    address: "Údolní 12, 602 00 Brno, Česko".into(),
                    email: Some("bezpecnost@lnen.example".into()),
                    url: None,
                    phone: None,
                }),
                eu_responsible_person: None,
                safety_info: i18n(
                    "Nevhodné pro děti do 3 let kvůli malým částem (knoflíky, zipy).",
                    "Nevhodné pre deti do 3 rokov pre malé časti (gombíky, zipsy).",
                ),
                warnings: I18n::new(),
            },
            unit_measure: (*pack > 1).then_some(UnitMeasure::Pcs),
            unit_quantity: (*pack > 1).then_some(f64::from(*pack)),
            heureka_category: None,
            google_category: None,
            translations: vec![
                ProductTranslation {
                    locale: "cs".into(),
                    name: (*cs).into(),
                    slug: slugify(cs),
                    description_html: description(cs, material_cs, true),
                    short_description: format!("{cs} – {}.", material_cs.to_lowercase()),
                    seo_title: None,
                    seo_description: None,
                },
                ProductTranslation {
                    locale: "sk".into(),
                    name: (*sk).into(),
                    slug: slugify(sk),
                    description_html: description(sk, material_sk, false),
                    short_description: format!("{sk} – {}.", material_sk.to_lowercase()),
                    seo_title: None,
                    seo_description: None,
                },
            ],
            options,
            variants,
            category_ids: vec![cats[kind.category()]],
            media: media_items,
            parameters,
            tax_categories: BTreeMap::new(),
        };
        let product = products::create(&mut tx, ACTOR, &input).await?;
        // Most of the catalog is older than a month; the last products of each kind are new.
        let age_days = if i % 5 == 4 {
            3
        } else {
            40 + i64::try_from(i).unwrap_or(0)
        };
        sqlx::query!(
            "UPDATE products SET created_at = now() - make_interval(days => $2::int) WHERE id = $1",
            product.id,
            i32::try_from(age_days).unwrap_or(40)
        )
        .execute(&mut *tx)
        .await?;

        // Whole CZK; a multipack is about 10 % cheaper than its pieces (still ending in 90).
        let pack = i64::from(*pack).max(1);
        let czk = if pack > 1 {
            kind.price(i) * pack * 9 / 10 / 100 * 100 + 90
        } else {
            kind.price(i)
        };
        for (list, amount_minor) in [(czk_list, czk * 100), (eur_list, eur_minor(czk))] {
            let items = product
                .variants
                .iter()
                .map(|v| PriceItem {
                    variant_id: v.id,
                    amount_minor,
                    compare_at_minor: None,
                })
                .collect();
            pricing::upsert_prices(
                &mut tx,
                ACTOR,
                list,
                &PriceUpsert {
                    reason: PriceChangeReason::Base,
                    imported: false,
                    items,
                },
            )
            .await?;
        }

        // Stock: mostly plenty; some low, some sold out, one product on backorder.
        for (k, v) in product.variants.iter().enumerate() {
            let on_hand = match (i % 13, i % 7, k % 5) {
                (5, _, _) => 0,
                (_, 3, _) => 2,
                (_, _, 4) => 0,
                _ => i32::try_from(8 + (i * 7 + k * 3) % 33).unwrap_or(10),
            };
            if i % 11 == 9 {
                inventory::update_settings(
                    &mut tx,
                    ACTOR,
                    v.id,
                    &LevelSettings {
                        track: true,
                        allow_backorder: true,
                    },
                )
                .await?;
            }
            if on_hand > 0 {
                inventory::adjust(
                    &mut tx,
                    ACTOR,
                    v.id,
                    &format!("seed-{}", v.id),
                    &Adjustment {
                        delta: None,
                        on_hand: Some(on_hand),
                        note: Some("Počáteční stav".into()),
                    },
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn promotions(
        &self,
        tenant_id: Uuid,
        cats: &BTreeMap<String, Uuid>,
    ) -> anyhow::Result<()> {
        let mut tx = self.tx(tenant_id).await?;
        let sale_exists = sqlx::query_scalar!(
            "SELECT count(*) AS \"n!\" FROM sales WHERE name = 'Podzimní výprodej mikin'"
        )
        .fetch_one(&mut *tx)
        .await?;
        if sale_exists == 0 {
            // Starts a moment after the prices exist, so the Omnibus reference (the lowest
            // price before the reduction) is known and the discount may be shown (A18).
            sales::create(
                &mut tx,
                ACTOR,
                &SaleInput {
                    name: "Podzimní výprodej mikin".into(),
                    discount: SaleDiscount::Percent { basis_points: 2000 },
                    starts_at: Some(Utc::now() + Duration::seconds(2)),
                    ends_at: Some(Utc::now() + Duration::days(30)),
                    targets: SaleTargets {
                        all: false,
                        product_ids: Vec::new(),
                        category_ids: vec![cats["mikiny"]],
                    },
                },
            )
            .await?;
        }
        if coupons::find_by_code(&mut tx, "VITEJTE10").await?.is_none() {
            coupons::create(
                &mut tx,
                ACTOR,
                &CouponInput {
                    code: "VITEJTE10".into(),
                    discount: commerce::pricing::cart::CouponDiscount::Percent {
                        basis_points: 1000,
                    },
                    currency: None,
                    min_subtotal_minor: None,
                    starts_at: None,
                    ends_at: None,
                    usage_limit: None,
                    per_customer_limit: None,
                    published: true,
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Demo order history for the recommendations (WP17): about two orders a day over the last
    /// 90 days with recurring baskets (so "bought together" has pairs above the support
    /// threshold) and a month of orders a year ago (seasonal bestsellers). Inserted as placed,
    /// paid, cash-on-delivery orders without stock movements: it is history, not live demand.
    /// Deterministic; skipped when the history exists. Ends with a backfill rollup job.
    async fn history(&self, tenant_id: Uuid, cz: Uuid, sk: Uuid) -> anyhow::Result<()> {
        let mut tx = self.tx(tenant_id).await?;
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM orders WHERE email LIKE '%@history.example.com') AS "e!""#
        )
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            return Ok(());
        }
        // (product index in PRODUCTS, default variant, product id, sku, cs name)
        let mut catalog = Vec::new();
        for (i, (_, cs, _, _)) in PRODUCTS.iter().enumerate() {
            let row = sqlx::query!(
                "SELECT v.id, v.product_id, v.sku FROM variants v
                 JOIN product_translations t ON t.product_id = v.product_id
                 WHERE t.locale = 'cs' AND t.slug = $1
                 ORDER BY v.is_default DESC, v.sku LIMIT 1",
                slugify(cs)
            )
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(r) = row {
                catalog.push((i, r.id, r.product_id, r.sku, (*cs).to_owned()));
            }
        }
        if catalog.len() < PRODUCTS.len() {
            return Err(anyhow!("history needs the whole demo catalog"));
        }
        // Baskets shoppers keep buying together (indices into PRODUCTS).
        const BASKETS: &[&[usize]] = &[
            &[0, 37],     // Tričko Basic + Kšiltovka Classic
            &[1, 48],     // Tričko Oversize + Taška Shopper
            &[20, 36],    // Mikina s kapucí Klasik + Čepice Merino
            &[22, 49],    // Mikina na zip + Batoh Roll-top
            &[8, 27, 44], // Merino: tričko, mikina, čelenka
            &[2, 50],     // Tričko s kapsičkou + Plátěná taška
        ];
        // A year ago: hoodies and warm caps.
        const LAST_YEAR: &[usize] = &[20, 21, 24, 25, 36, 38, 42];
        let mut state: u64 = 0x5eed_1717;
        let mut next = move |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % n
        };
        let now = Utc::now();
        let mut orders: Vec<(chrono::DateTime<Utc>, Vec<usize>)> = Vec::new();
        for day in 1..=90_i64 {
            for _ in 0..=next(2) {
                let at = now - Duration::days(day)
                    + Duration::seconds(i64::try_from(next(80_000)).unwrap_or(0));
                let lines: Vec<usize> = if next(10) < 6 {
                    BASKETS[usize::try_from(next(BASKETS.len() as u64)).unwrap_or(0)].to_vec()
                } else {
                    let a = usize::try_from(next(PRODUCTS.len() as u64)).unwrap_or(0);
                    let b = usize::try_from(next(PRODUCTS.len() as u64)).unwrap_or(0);
                    if a == b || next(2) == 0 {
                        vec![a]
                    } else {
                        vec![a, b]
                    }
                };
                orders.push((at, lines));
            }
        }
        for day in 350..=380_i64 {
            let at = now - Duration::days(day)
                + Duration::seconds(i64::try_from(next(80_000)).unwrap_or(0));
            let p = LAST_YEAR[usize::try_from(next(LAST_YEAR.len() as u64)).unwrap_or(0)];
            orders.push((at, vec![p]));
        }
        orders.sort_by_key(|(at, _)| *at);

        for (n, (at, lines)) in orders.iter().enumerate() {
            let slovak = next(4) == 0;
            let (market, currency, locale, country, rate) = if slovak {
                (sk, "EUR", "sk", "SK", 23_i64)
            } else {
                (cz, "CZK", "cs", "CZ", 21_i64)
            };
            let priced: Vec<(Uuid, Uuid, String, String, i64, i32)> = lines
                .iter()
                .map(|&i| {
                    let (_, variant, product, sku, name) = &catalog[i];
                    let (kind, ..) = PRODUCTS[i];
                    let czk = kind.price(i);
                    let unit = if slovak { eur_minor(czk) } else { czk * 100 };
                    let qty = if next(6) == 0 { 2 } else { 1 };
                    (*variant, *product, sku.clone(), name.clone(), unit, qty)
                })
                .collect();
            let total: i64 = priced.iter().map(|l| l.4 * i64::from(l.5)).sum();
            let tax = total * rate / (100 + rate);
            let delivered = *at < now - Duration::days(5);
            let cart = sqlx::query_scalar!(
                "INSERT INTO carts (tenant_id, market_id, locale, currency, status, created_at,
                                    updated_at, last_activity_at)
                 VALUES ($1, $2, $3, $4, 'converted', $5, $5, $5) RETURNING id",
                tenant_id,
                market,
                locale,
                currency,
                at
            )
            .fetch_one(&mut *tx)
            .await?;
            let number = sqlx::query_scalar!(
                "INSERT INTO order_numbers (tenant_id, last) VALUES ($1, $2)
                 ON CONFLICT (tenant_id) DO UPDATE SET last = order_numbers.last + 1
                 RETURNING last",
                tenant_id,
                commerce::checkout::FIRST_NUMBER
            )
            .fetch_one(&mut *tx)
            .await?;
            let order = sqlx::query_scalar!(
                r#"INSERT INTO orders (tenant_id, number, market_id, cart_id, email, locale, currency,
                                       status, payment_status, fulfillment_status, ship_to_country,
                                       vat_payer, subtotal_minor, discount_minor, shipping_minor,
                                       payment_fee_minor, tax_minor, rounding_minor, total_minor,
                                       vat_recap, shipping_method_snapshot, payment_method, notes,
                                       placed_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'paid', $9, $10, true, $11, 0, 0, 0,
                           $12, 0, $11, '[]', '{}', 'cod', 'Demo history (seed)', $13, $13)
                   RETURNING id"#,
                tenant_id,
                number,
                market,
                cart,
                format!("zakaznik{n}@history.example.com"),
                locale,
                currency,
                if delivered { "delivered" } else { "confirmed" },
                if delivered { "delivered" } else { "unfulfilled" },
                country,
                total,
                tax,
                at
            )
            .fetch_one(&mut *tx)
            .await?;
            for (pos, (variant, product, sku, name, unit, qty)) in priced.iter().enumerate() {
                let line = unit * i64::from(*qty);
                let line_tax = line * rate / (100 + rate);
                sqlx::query!(
                    "INSERT INTO order_lines (tenant_id, order_id, position, variant_id, product_id,
                                              sku, name, quantity, unit_gross_minor, base_minor,
                                              discount_minor, total_minor, tax_rate, tax_minor,
                                              net_minor)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, $10, $11, $12, $13)",
                    tenant_id,
                    order,
                    i32::try_from(pos + 1).unwrap_or(1),
                    variant,
                    product,
                    sku,
                    name,
                    qty,
                    unit,
                    line,
                    rate.to_string(),
                    line_tax,
                    line - line_tax
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        let mut job = platform::queue::NewJob::new(
            commerce::recommendations::ROLLUP_JOB,
            json!({ "backfill": true }),
        );
        job.tenant_id = Some(tenant_id);
        platform::queue::enqueue(&mut *tx, &job).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Shipping methods (Packeta pickup + home, PPL with weight tiers, personal pickup) and
    /// payment methods (fake gateway where `PAYMENTS_FAKE=1`, cash on delivery) for CZ and SK.
    async fn checkout_methods(&self, tenant_id: Uuid, cz: Uuid, sk: Uuid) -> anyhow::Result<()> {
        let mut tx = self.tx(tenant_id).await?;
        let names = |cs: &str, sk: &str, en: &str| {
            I18n::from([
                ("cs".to_owned(), cs.to_owned()),
                ("sk".to_owned(), sk.to_owned()),
                ("en".to_owned(), en.to_owned()),
            ])
        };
        let tier = |up_to_g, price_minor| WeightTier {
            up_to_g,
            price_minor,
        };
        #[rustfmt::skip]
        let methods = [
            (cz, Carrier::PacketaPickup, names("Zásilkovna – výdejní místo", "Packeta – výdajné miesto", "Packeta pickup point"), 7900, Some(150_000), vec![], 2900, 0),
            (cz, Carrier::PacketaHome, names("Zásilkovna – na adresu", "Packeta – na adresu", "Packeta home delivery"), 11_900, Some(250_000), vec![], 2900, 1),
            (cz, Carrier::Ppl, names("PPL – kurýr", "PPL – kuriér", "PPL courier"), 12_900, None, vec![tier(5_000, 12_900), tier(30_000, 18_900)], 3900, 2),
            (cz, Carrier::PersonalPickup, names("Osobní odběr – Praha", "Osobný odber – Praha", "Personal pickup – Prague"), 0, None, vec![], 0, 3),
            (sk, Carrier::PacketaPickup, names("Zásilkovna – výdejní místo", "Packeta – výdajné miesto", "Packeta pickup point"), 290, Some(6_000), vec![], 150, 0),
            (sk, Carrier::PacketaHome, names("Zásilkovna – na adresu", "Packeta – na adresu", "Packeta home delivery"), 490, Some(9_000), vec![], 150, 1),
        ];
        for (market, carrier, name_i18n, price, free, tiers, cod_fee, position) in methods {
            if shipping::list(&mut tx, Some(market))
                .await?
                .iter()
                .any(|m| m.carrier == carrier)
            {
                continue;
            }
            shipping::create(
                &mut tx,
                ACTOR,
                &ShippingMethodInput {
                    market_id: market,
                    carrier,
                    name_i18n,
                    description_i18n: I18n::new(),
                    price_minor: price,
                    free_over_minor: free,
                    weight_tiers: tiers,
                    cod_allowed: carrier != Carrier::PersonalPickup,
                    cod_fee_minor: cod_fee,
                    active: true,
                    position,
                },
            )
            .await?;
        }
        // WP11: a receiving account per market (well-formed demo IBANs, not real accounts).
        for (market, iban, bic) in [
            (cz, "CZ6508000000192000145399", "GIBACZPX"),
            (sk, "SK9611000000002918599669", "TATRSKBX"),
        ] {
            if commerce::payments::bank::account(&mut tx, market)
                .await?
                .is_none()
            {
                commerce::payments::bank::configure_account(
                    &mut tx,
                    ACTOR,
                    None,
                    market,
                    &commerce::payments::bank::BankAccountInput {
                        iban: iban.into(),
                        bic: Some(bic.into()),
                        account_name: "Demo obchod s.r.o.".into(),
                        fio_token: None,
                        clear_fio_token: false,
                    },
                )
                .await?;
            }
        }
        let payments = commerce::payments::Payments::default();
        for market in [cz, sk] {
            for (kind, position) in [
                (MethodKind::Fake, 0),
                (MethodKind::Stripe, 1),
                (MethodKind::BankTransfer, 2),
                (MethodKind::Cod, 3),
            ] {
                let configured = commerce::payments::methods(&mut tx, &payments, market)
                    .await?
                    .iter()
                    .any(|m| m.kind == kind && m.enabled);
                if !configured {
                    commerce::payments::configure(
                        &mut tx,
                        ACTOR,
                        &payments,
                        market,
                        kind,
                        &PaymentMethodInput {
                            enabled: true,
                            name_i18n: I18n::new(),
                            timeout_minutes: None,
                            position,
                        },
                    )
                    .await?;
                }
            }
        }
        tx.commit().await?;
        self.stripe_simulator(tenant_id).await
    }

    /// Local demo: with the Stripe simulator (no real key), the shop is onboarded right away,
    /// through the same path as the admin button (a signed `account.updated`, processed by
    /// the worker). With a real key onboarding stays a person's job in the admin.
    async fn stripe_simulator(&self, tenant_id: Uuid) -> anyhow::Result<()> {
        let config = platform::config::PaymentsConfig::from_env(platform::config::AppEnv::Dev)?;
        let Some(cfg) = config
            .stripe
            .filter(|c| c.mode == platform::config::StripeMode::Simulator)
        else {
            return Ok(());
        };
        let stripe = commerce::payments::stripe::Stripe::new(&cfg, reqwest::Client::new());
        let mut tx = self.tx(tenant_id).await?;
        let ready = commerce::payments::stripe::account(&mut tx)
            .await?
            .is_some_and(|a| a.ready);
        tx.commit().await?;
        if !ready {
            commerce::payments::stripe::start_onboarding(
                self.db, &stripe, tenant_id, ACTOR, "", "",
            )
            .await?;
        }
        Ok(())
    }

    /// WP13a: the seller's legal entity, the legal templates (published for the demo), shipping
    /// and contact pages, a blog post and the footer menu.
    async fn content(&self, tenant_id: Uuid) -> anyhow::Result<()> {
        use commerce::content::blocks::FaqItem;
        use commerce::content::legal::{self, InstallInput, LegalEntity};
        use commerce::content::menus::{self, MenuEntry, MenuInput, MenuLink};
        use commerce::content::{self, Block, PageInput, PageKind, PageStatus, PageTranslation};

        let mut tx = self.tx(tenant_id).await?;
        if legal::entity(&mut tx).await?.updated_at.is_none() {
            legal::put_entity(
                &mut tx,
                ACTOR,
                &LegalEntity {
                    company_name: "Demo Shop s.r.o.".into(),
                    company_id: "12345678".into(),
                    street: "Dlouhá 1".into(),
                    city: "Praha 1".into(),
                    postal_code: "110 00".into(),
                    country: "CZ".into(),
                    email: "info@demo.localhost".into(),
                    phone: "+420 800 123 456".into(),
                    registry: "zapsaná v obchodním rejstříku vedeném Městským soudem v Praze, \
                               oddíl C, vložka 000000 (demo)"
                        .into(),
                    returns_address: String::new(),
                },
            )
            .await?;
        }
        // Demo only: the templates go live unreviewed. Real shops review them with a lawyer.
        let installed = legal::install(&mut tx, ACTOR, &InstallInput::default()).await?;
        for id in installed.created {
            let p = content::get(&mut tx, id).await?;
            let input = PageInput {
                kind: p.kind,
                legal_type: p.legal_type,
                status: PageStatus::Published,
                published_at: None,
                image_asset_id: p.image_asset_id,
                translations: p.translations,
            };
            content::update(&mut tx, ACTOR, id, &input).await?;
        }

        let text = |html: &str| Block::RichText { html: html.into() };
        let tr = |locale: &str, title: &str, slug: &str, blocks: Vec<Block>| PageTranslation {
            locale: locale.into(),
            title: title.into(),
            slug: slug.into(),
            excerpt: String::new(),
            blocks,
            seo_title: None,
            seo_description: None,
        };
        let pages = [
            (
                PageKind::Page,
                vec![
                    tr("cs", "Doprava a platba", "doprava-a-platba", vec![
                        text("<p>Objednávky odesíláme do 24 hodin v pracovní dny. Nad 1 500 Kč je doprava zdarma.</p>"),
                        Block::Heading { text: "Způsoby dopravy".into(), level: 2 },
                        text("<ul><li>Zásilkovna – výdejní místa a boxy</li><li>PPL – doručení na adresu</li></ul>"),
                        Block::Heading { text: "Platba".into(), level: 2 },
                        text("<p>Kartou online, převodem nebo dobírkou.</p>"),
                    ]),
                    tr("sk", "Doprava a platba", "doprava-a-platba", vec![
                        text("<p>Objednávky odosielame do 24 hodín v pracovné dni.</p>"),
                        Block::Heading { text: "Spôsoby dopravy".into(), level: 2 },
                        text("<ul><li>Packeta – výdajné miesta a boxy</li><li>PPL – doručenie na adresu</li></ul>"),
                    ]),
                ],
            ),
            (
                PageKind::Page,
                vec![
                    tr("cs", "Kontakt", "kontakt", vec![
                        text("<p>Demo Shop s.r.o., Dlouhá 1, 110 00 Praha 1</p><p>E-mail: <a href=\"mailto:info@demo.localhost\">info@demo.localhost</a>, telefon +420 800 123 456 (Po–Pá 9–17).</p>"),
                        Block::Faq { items: vec![
                            FaqItem { question: "Kdy mi přijde objednávka?".into(), answer_html: "<p>Obvykle do dvou pracovních dnů.</p>".into() },
                            FaqItem { question: "Jak vrátit zboží?".into(), answer_html: "<p>Do 14 dnů bez udání důvodu, viz Odstoupení od smlouvy.</p>".into() },
                        ]},
                    ]),
                    tr("sk", "Kontakt", "kontakt", vec![
                        text("<p>Demo Shop s.r.o., Dlouhá 1, 110 00 Praha 1</p><p>E-mail: <a href=\"mailto:info@demo.localhost\">info@demo.localhost</a></p>"),
                    ]),
                ],
            ),
            (
                PageKind::BlogPost,
                vec![tr("cs", "Jak vybrat správnou velikost trička", "jak-vybrat-velikost-tricka", vec![
                    text("<p>Změřte si obvod hrudníku a porovnejte ho s tabulkou velikostí u produktu. Když váháte mezi dvěma velikostmi, sáhněte po větší.</p>"),
                    Block::Heading { text: "Naše oblíbená trička".into(), level: 2 },
                    Block::Button { label: "Všechna trička".into(), href: "/c/tricka".into() },
                ])],
            ),
        ];
        let mut ids = Vec::new();
        for (kind, translations) in pages {
            let slug = translations[0].slug.clone();
            let existing = sqlx::query_scalar!(
                "SELECT page_id FROM page_translations WHERE locale = 'cs' AND slug = $1",
                slug
            )
            .fetch_optional(&mut *tx)
            .await?;
            let id = match existing {
                Some(id) => id,
                None => {
                    content::create(
                        &mut tx,
                        ACTOR,
                        &PageInput {
                            kind,
                            legal_type: None,
                            status: PageStatus::Published,
                            published_at: None,
                            image_asset_id: None,
                            translations,
                        },
                    )
                    .await?
                    .id
                }
            };
            ids.push(id);
        }
        if menus::entries(&mut tx, "footer").await?.is_none() {
            let page = |id: Uuid| MenuEntry {
                label_i18n: I18n::new(),
                link: MenuLink::Page { id },
                children: Vec::new(),
            };
            menus::put(
                &mut tx,
                ACTOR,
                "footer",
                &MenuInput {
                    items: vec![
                        page(ids[0]),
                        page(ids[1]),
                        MenuEntry {
                            label_i18n: i18n("Blog", "Blog"),
                            link: MenuLink::Url {
                                url: "/blog".into(),
                            },
                            children: Vec::new(),
                        },
                    ],
                },
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_prices() {
        assert_eq!(
            slugify("Tričko s dlouhým rukávem"),
            "tricko-s-dlouhym-rukavem"
        );
        assert_eq!(slugify("Tričko Basic 3 ks"), "tricko-basic-3-ks");
        assert_eq!(slugify("Ľanová šiltovka"), "lanova-siltovka");
        assert_eq!(eur_minor(590), 2390);
        assert_eq!(PRODUCTS.len(), 60);
        // Slugs are unique per locale.
        let mut cs: Vec<String> = PRODUCTS.iter().map(|p| slugify(p.1)).collect();
        let mut sk: Vec<String> = PRODUCTS.iter().map(|p| slugify(p.2)).collect();
        cs.sort();
        cs.dedup();
        sk.sort();
        sk.dedup();
        assert_eq!((cs.len(), sk.len()), (60, 60));
        assert_eq!(slugify(PRODUCTS[0].1), "tricko-basic");
    }
}
