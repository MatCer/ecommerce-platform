//! Tax profile, pricing, promotions and inventory services against a real Postgres, as the
//! runtime role (RLS on).
#![allow(clippy::unwrap_used)]

use chrono::{DateTime, Duration, NaiveDate, Utc};
use commerce::inventory::{self, Adjustment, LevelSettings, MovementRef};
use commerce::money::Currency;
use commerce::pricing::cart::CouponDiscount;
use commerce::pricing::intervals::{self, Cause};
use commerce::pricing::{self, PriceChangeReason, PriceItem, PriceUpsert};
use commerce::promotions::coupons::{self, CouponInput};
use commerce::promotions::sales::{self, SaleDiscount, SaleInput, SaleTargets};
use commerce::tax::{self, DistanceSalesMode, TaxProfileInput, TaxRate};
use platform::db::{TenantTx, tenant_tx};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

async fn events(owner: &PgPool, tenant: Uuid, kind: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM queue.outbox WHERE tenant_id = $1 AND type = $2 ORDER BY id",
    )
    .bind(tenant)
    .bind(kind)
    .fetch_all(owner)
    .await
    .unwrap()
}

fn cz_profile(mode: DistanceSalesMode) -> TaxProfileInput {
    TaxProfileInput {
        establishment_country: "CZ".into(),
        vat_payer: true,
        vat_id: Some("CZ12345678".into()),
        sk_ic_dph: None,
        distance_sales_mode: mode,
        confirm_origin_threshold: mode == DistanceSalesMode::OriginThreshold,
        cash_rounding_in_vat_base: false,
    }
}

fn upsert(items: &[(Uuid, i64)]) -> PriceUpsert {
    PriceUpsert {
        reason: PriceChangeReason::Base,
        imported: false,
        items: items
            .iter()
            .map(|(v, a)| PriceItem {
                variant_id: *v,
                amount_minor: *a,
                compare_at_minor: None,
            })
            .collect(),
    }
}

fn sale(
    name: &str,
    bp: u32,
    starts: Option<DateTime<Utc>>,
    ends: Option<DateTime<Utc>>,
    targets: SaleTargets,
) -> SaleInput {
    SaleInput {
        name: name.into(),
        discount: SaleDiscount::Percent { basis_points: bp },
        starts_at: starts,
        ends_at: ends,
        targets,
    }
}

fn all() -> SaleTargets {
    SaleTargets {
        all: true,
        ..SaleTargets::default()
    }
}

async fn price_now(
    tx: &mut TenantTx,
    list: Uuid,
    variant: Uuid,
    at: DateTime<Utc>,
) -> Option<(i64, Cause)> {
    intervals::effective_prices(tx, list, &[variant], at)
        .await
        .unwrap()
        .first()
        .map(|p| (p.amount_minor, p.cause))
}

#[sqlx::test(migrations = "../../migrations")]
async fn tax_profile_and_liability(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, market) = testkit::tenant(&runtime, "shop").await;
    let mut input = testkit::catalog::product_input("TEA", 1);
    input.tax_categories = [
        ("CZ".to_owned(), "reduced".to_owned()),
        ("SK".to_owned(), "second_reduced".to_owned()),
    ]
    .into();
    let product = testkit::catalog::create(&runtime, tenant, &input).await;
    let day = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    assert!(tax::get(&mut tx).await.unwrap().is_none());
    assert_eq!(
        tax::resolve(&mut tx, product.id, "CZ", day)
            .await
            .unwrap_err()
            .code(),
        "tax_profile_missing"
    );
    let p = tax::upsert(
        &mut tx,
        "owner",
        &cz_profile(DistanceSalesMode::Destination),
    )
    .await
    .unwrap();
    assert!(p.origin_threshold_confirmed_at.is_none());
    // Domestic: CZ reduced 12 %; OSS to SK: SK's second reduced 5 %.
    let cz = tax::resolve(&mut tx, product.id, "CZ", day).await.unwrap();
    assert_eq!(
        (cz.country.as_deref(), cz.rate),
        (Some("CZ"), TaxRate(1200))
    );
    let sk = tax::resolve(&mut tx, product.id, "SK", day).await.unwrap();
    assert_eq!((sk.country.as_deref(), sk.rate), (Some("SK"), TaxRate(500)));
    // No mapping for PL: standard 23 %.
    let pl = tax::resolve(&mut tx, product.id, "PL", day).await.unwrap();
    assert_eq!(pl.rate, TaxRate(2300));

    // Threshold regime: the origin rate everywhere; confirmation time recorded and kept.
    let p = tax::upsert(
        &mut tx,
        "owner",
        &cz_profile(DistanceSalesMode::OriginThreshold),
    )
    .await
    .unwrap();
    let confirmed = p.origin_threshold_confirmed_at.unwrap();
    let sk = tax::resolve(&mut tx, product.id, "SK", day).await.unwrap();
    assert_eq!(
        (sk.country.as_deref(), sk.rate),
        (Some("CZ"), TaxRate(1200))
    );
    let again = TaxProfileInput {
        confirm_origin_threshold: false,
        ..cz_profile(DistanceSalesMode::OriginThreshold)
    };
    let p = tax::upsert(&mut tx, "owner", &again).await.unwrap();
    assert_eq!(p.origin_threshold_confirmed_at, Some(confirmed));

    // Non-payer: no VAT.
    let non_payer = TaxProfileInput {
        vat_payer: false,
        ..cz_profile(DistanceSalesMode::Destination)
    };
    tax::upsert(&mut tx, "owner", &non_payer).await.unwrap();
    let none = tax::resolve(&mut tx, product.id, "SK", day).await.unwrap();
    assert_eq!((none.country, none.rate), (None, TaxRate::ZERO));

    // The default market ships to CZ only.
    assert!(tax::validate_ship_to(&mut tx, market, "CZ").await.is_ok());
    assert_eq!(
        tax::validate_ship_to(&mut tx, market, "SK")
            .await
            .unwrap_err()
            .code(),
        "ship_to_not_allowed"
    );
    tx.commit().await.unwrap();
    assert_eq!(events(&db, tenant, "tax_profile.updated").await.len(), 4);
}

#[sqlx::test(migrations = "../../migrations")]
async fn prices_sales_and_intervals(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, market) = testkit::tenant(&runtime, "shop").await;
    let shirts = testkit::catalog::category(&runtime, tenant, "shirts", None).await;
    let summer = testkit::catalog::category(&runtime, tenant, "summer", Some(shirts.id)).await;
    let mut input = testkit::catalog::product_input("TS", 2);
    input.category_ids = vec![summer.id];
    let shirt = testkit::catalog::create(&runtime, tenant, &input).await;
    let mug = testkit::catalog::product(&runtime, tenant, "MUG", 1).await;
    let (v1, v2, m) = (
        shirt.variants[0].id,
        shirt.variants[1].id,
        mug.variants[0].id,
    );

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let list = pricing::create_price_list(
        &mut tx,
        "u",
        &pricing::NewPriceList {
            code: "cz".into(),
            name: "CZ".into(),
            currency: Currency::Czk,
            market_ids: vec![market],
        },
    )
    .await
    .unwrap();
    assert_eq!(list.market_ids, vec![market]);
    // A market in another currency cannot use the list.
    let eur = pricing::create_price_list(
        &mut tx,
        "u",
        &pricing::NewPriceList {
            code: "eu".into(),
            name: "EU".into(),
            currency: Currency::Eur,
            market_ids: vec![],
        },
    )
    .await
    .unwrap();
    let err = pricing::update_price_list(
        &mut tx,
        "u",
        eur.id,
        &pricing::PriceListUpdate {
            name: "EU".into(),
            market_ids: vec![market],
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "currency_mismatch");

    pricing::upsert_prices(
        &mut tx,
        "u",
        list.id,
        &upsert(&[(v1, 100_000), (v2, 120_000), (m, 20_000)]),
    )
    .await
    .unwrap();
    let now = Utc::now();
    assert_eq!(
        price_now(&mut tx, list.id, v1, now).await,
        Some((100_000, Cause::Base))
    );

    // A 20 % sale on the parent category, starting in 2 days, ending in 5 days.
    let starts = now + Duration::days(2);
    let ends = now + Duration::days(5);
    let s = sales::create(
        &mut tx,
        "u",
        &sale(
            "Léto",
            2000,
            Some(starts),
            Some(ends),
            SaleTargets {
                category_ids: vec![shirts.id],
                ..SaleTargets::default()
            },
        ),
    )
    .await
    .unwrap();
    let at = |d: i64| now + Duration::days(d) + Duration::hours(1);
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(0)).await,
        Some((100_000, Cause::Base))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(3)).await,
        Some((80_000, Cause::Sale))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v2, at(3)).await,
        Some((96_000, Cause::Sale))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(6)).await,
        Some((100_000, Cause::Base))
    );
    // The mug is not in the category.
    assert_eq!(
        price_now(&mut tx, list.id, m, at(3)).await,
        Some((20_000, Cause::Base))
    );

    // Overlapping bigger sale on everything from day 4, open-ended: the lowest price wins.
    sales::create(
        &mut tx,
        "u",
        &sale("Vše", 3000, Some(now + Duration::days(4)), None, all()),
    )
    .await
    .unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(4)).await,
        Some((70_000, Cause::Sale))
    );
    assert_eq!(
        price_now(&mut tx, list.id, m, at(10)).await,
        Some((14_000, Cause::Sale))
    );

    // Base price change now: history closes, future re-derives from the new base.
    let changed_at = Utc::now();
    let mut repriced = upsert(&[(v1, 110_000)]);
    repriced.reason = PriceChangeReason::Tax;
    pricing::upsert_prices(&mut tx, "u", list.id, &repriced)
        .await
        .unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, v1, Utc::now()).await,
        Some((110_000, Cause::Tax))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(3)).await,
        Some((88_000, Cause::Sale))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v1, changed_at - Duration::milliseconds(1))
            .await
            .map(|p| p.0),
        Some(100_000)
    );

    // The first sale ends early (moved end), and is later deleted.
    let moved = SaleInput {
        ends_at: Some(now + Duration::days(3)),
        ..sale(
            "Léto",
            2000,
            Some(starts),
            None,
            SaleTargets {
                category_ids: vec![shirts.id],
                ..SaleTargets::default()
            },
        )
    };
    sales::update(&mut tx, "u", s.id, &moved).await.unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(3)).await,
        Some((110_000, Cause::Base))
    );
    sales::delete(&mut tx, "u", s.id).await.unwrap();
    // The interval opened by the tax-driven change simply continues.
    assert_eq!(
        price_now(&mut tx, list.id, v1, at(2)).await,
        Some((110_000, Cause::Tax))
    );

    // Removing a price ends the timeline now.
    pricing::delete_price(&mut tx, "u", list.id, m)
        .await
        .unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, m, Utc::now() + Duration::seconds(1)).await,
        None
    );

    // Timelines never overlap and future transitions are scheduled as jobs.
    let history = pricing::price_history(&mut tx, shirt.id, Some(list.id), Utc::now())
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    for h in &history {
        for w in h.intervals.windows(2) {
            assert!(w[0].valid_to.unwrap() <= w[1].valid_from);
        }
    }
    tx.commit().await.unwrap();

    let jobs: Vec<(DateTime<Utc>,)> = sqlx::query_as(
        "SELECT run_at FROM queue.jobs WHERE tenant_id = $1 AND kind = 'pricing.transition' ORDER BY run_at",
    )
    .bind(tenant)
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(
        jobs.iter()
            .any(|(t,)| (*t - (now + Duration::days(4))).num_milliseconds().abs() < 1)
    );

    let changes = events(&db, tenant, "price.changed").await;
    let v1_changes: Vec<&Value> = changes
        .iter()
        .filter(|e| e["variant_id"] == v1.to_string())
        .collect();
    assert_eq!(v1_changes[0]["before_minor"], Value::Null);
    assert_eq!(v1_changes[0]["after_minor"], 100_000);
    assert!(v1_changes.iter().any(|e| e["before_minor"] == 100_000
        && e["after_minor"] == 110_000
        && e["cause"] == "tax"));

    // The worker job at the "Vše" start publishes the scheduled change.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let published = intervals::publish_transitions(&mut tx, now + Duration::days(4))
        .await
        .unwrap();
    assert!(published >= 2);
    tx.commit().await.unwrap();
    let after = events(&db, tenant, "price.changed").await;
    let scheduled = after.last().unwrap();
    assert_eq!(scheduled["cause"], "sale");
    assert!(scheduled["before_minor"].is_i64());
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_category_change_reprices(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let cat = testkit::catalog::category(&runtime, tenant, "sale-cat", None).await;
    let product = testkit::catalog::product(&runtime, tenant, "P", 1).await;
    let list = testkit::pricing::price_list(&runtime, tenant, "cz", Currency::Czk).await;
    let v = product.variants[0].id;
    testkit::pricing::set_prices(&runtime, tenant, list.id, &[(v, 10_000)]).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sales::create(
        &mut tx,
        "u",
        &sale(
            "Cat",
            5000,
            None,
            None,
            SaleTargets {
                category_ids: vec![cat.id],
                ..SaleTargets::default()
            },
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, v, Utc::now()).await,
        Some((10_000, Cause::Base))
    );
    let mut input = testkit::catalog::product_input("P", 1);
    input.variants[0].id = Some(v);
    input.category_ids = vec![cat.id];
    commerce::catalog::products::replace(&mut tx, "u", product.id, &input)
        .await
        .unwrap();
    assert_eq!(
        price_now(&mut tx, list.id, v, Utc::now()).await,
        Some((5_000, Cause::Sale))
    );
    // Fixed sales only touch lists in their currency.
    let eur = pricing::create_price_list(
        &mut tx,
        "u",
        &pricing::NewPriceList {
            code: "eu".into(),
            name: "EU".into(),
            currency: Currency::Eur,
            market_ids: vec![],
        },
    )
    .await
    .unwrap();
    pricing::upsert_prices(&mut tx, "u", eur.id, &upsert(&[(v, 1000)]))
        .await
        .unwrap();
    sales::create(
        &mut tx,
        "u",
        &SaleInput {
            discount: SaleDiscount::Fixed {
                amount_minor: 9_000,
                currency: Currency::Czk,
            },
            ..sale("Fix", 1, None, None, all())
        },
    )
    .await
    .unwrap();
    // EUR: only the percent sale applies (50 %), the CZK amount does not.
    assert_eq!(
        price_now(&mut tx, eur.id, v, Utc::now()).await,
        Some((500, Cause::Sale))
    );
    assert_eq!(
        price_now(&mut tx, list.id, v, Utc::now()).await,
        Some((1_000, Cause::Sale))
    );
}

/// Inserts a price history directly (the services only ever write from "now").
async fn backdate(
    tx: &mut TenantTx,
    list: Uuid,
    variant: Uuid,
    rows: &[(i64, Option<i64>, i64, &str, bool)],
) {
    let now = Utc::now();
    sqlx::query("DELETE FROM price_intervals WHERE variant_id = $1")
        .bind(variant)
        .execute(&mut **tx)
        .await
        .unwrap();
    for (from, to, amount, cause, imported) in rows {
        sqlx::query(
            "INSERT INTO price_intervals (tenant_id, price_list_id, variant_id, amount_minor, valid_from, valid_to, cause, imported)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(tx.tenant_id())
        .bind(list)
        .bind(variant)
        .bind(amount)
        .bind(now + Duration::days(*from))
        .bind(to.map(|t| now + Duration::days(t)))
        .bind(cause)
        .bind(imported)
        .execute(&mut **tx)
        .await
        .unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn omnibus_reference_from_history(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let product = testkit::catalog::product(&runtime, tenant, "O", 2).await;
    let (v, w) = (product.variants[0].id, product.variants[1].id);
    let list = testkit::pricing::price_list(&runtime, tenant, "cz", Currency::Czk).await;
    testkit::pricing::set_prices(&runtime, tenant, list.id, &[(v, 1000), (w, 1000)]).await;

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    // v: 1000 for 60 days, 800 between -20 and -10 days, 1000 since.
    backdate(
        &mut tx,
        list.id,
        v,
        &[
            (-60, Some(-20), 1000, "base", false),
            (-20, Some(-10), 800, "base", false),
            (-10, None, 1000, "base", false),
        ],
    )
    .await;
    // w: imported 10 days ago at 1000.
    backdate(&mut tx, list.id, w, &[(-10, None, 1000, "base", true)]).await;
    // A 10 % sale now and a deeper one in 40 days.
    sales::create(
        &mut tx,
        "u",
        &sale(
            "Now",
            1000,
            None,
            Some(Utc::now() + Duration::days(20)),
            all(),
        ),
    )
    .await
    .unwrap();
    sales::create(
        &mut tx,
        "u",
        &sale(
            "Later",
            3000,
            Some(Utc::now() + Duration::days(40)),
            None,
            all(),
        ),
    )
    .await
    .unwrap();

    let history =
        pricing::price_history(&mut tx, product.id, None, Utc::now() + Duration::minutes(1))
            .await
            .unwrap();
    let hv = history.iter().find(|h| h.variant_id == v).unwrap();
    // The 800 lies within the 30 days before the reduction: no claim for 900.
    assert!(hv.omnibus.on_sale);
    assert_eq!(hv.omnibus.current_minor, Some(900));
    assert_eq!(hv.omnibus.reference_minor, Some(800));
    assert!(!hv.omnibus.claim);
    let hw = history.iter().find(|h| h.variant_id == w).unwrap();
    assert!(hw.omnibus.on_sale && !hw.omnibus.claim && hw.omnibus.reference_minor.is_none());

    // At the future sale (day 40) the window is days 10-40: the first sale (900 until day 20)
    // is in it, but the base price in between breaks the chain.
    let future = pricing::price_history(&mut tx, product.id, None, Utc::now() + Duration::days(41))
        .await
        .unwrap();
    let fv = future.iter().find(|h| h.variant_id == v).unwrap();
    assert_eq!(fv.omnibus.current_minor, Some(700));
    assert_eq!(
        fv.omnibus
            .reduction_started_at
            .map(|t| t > Utc::now() + Duration::days(39)),
        Some(true)
    );
    assert_eq!(fv.omnibus.reference_minor, Some(900));
    assert!(fv.omnibus.claim);
    assert_eq!(fv.omnibus.discount_percent, Some(22));

    // A published 25 % coupon active now lowers the future reference to 750.
    coupons::create(
        &mut tx,
        "u",
        &CouponInput {
            code: "VSICHNI25".into(),
            discount: CouponDiscount::Percent { basis_points: 2500 },
            currency: None,
            min_subtotal_minor: None,
            starts_at: Some(Utc::now() + Duration::days(25)),
            ends_at: Some(Utc::now() + Duration::days(30)),
            usage_limit: None,
            per_customer_limit: None,
            published: true,
        },
    )
    .await
    .unwrap();
    let future = pricing::price_history(
        &mut tx,
        product.id,
        Some(list.id),
        Utc::now() + Duration::days(41),
    )
    .await
    .unwrap();
    let fv = future.iter().find(|h| h.variant_id == v).unwrap();
    assert_eq!(fv.omnibus.reference_minor, Some(750));
    assert!(fv.omnibus.claim);
}

#[sqlx::test(migrations = "../../migrations")]
async fn coupon_limits_hold_under_concurrency(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let coupon = coupons::create(
        &mut tx,
        "u",
        &CouponInput {
            code: "last1".into(),
            discount: CouponDiscount::Fixed {
                amount_minor: 10_000,
            },
            currency: Some(Currency::Czk),
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: Some(1),
            per_customer_limit: None,
            published: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(coupon.code, "LAST1");
    let found = coupons::find_by_code(&mut tx, "last1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, coupon.id);
    tx.commit().await.unwrap();

    let redeem = |order: &'static str| {
        let runtime = runtime.clone();
        async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let r = coupons::redeem(&mut tx, coupon.id, "a@example.test", order, Utc::now()).await;
            if r.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                tx.commit().await.unwrap();
            }
            r.map(|r| r.order_ref).map_err(|e| e.code())
        }
    };
    let (a, b) = tokio::join!(redeem("order-a"), redeem("order-b"));
    let mut outcomes = [a.clone(), b.clone()];
    outcomes.sort();
    assert!(
        outcomes.iter().filter(|o| o.is_ok()).count() == 1,
        "{a:?} {b:?}"
    );
    assert!(outcomes.contains(&Err("coupon_exhausted")));

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let winner = a.or(b).unwrap();
    // Replaying the winning order is idempotent.
    let again = coupons::redeem(&mut tx, coupon.id, "a@example.test", &winner, Utc::now())
        .await
        .unwrap();
    assert_eq!(again.order_ref, winner);
    assert_eq!(
        coupons::get(&mut tx, coupon.id).await.unwrap().used_count,
        1
    );
    // Released on cancel, it can be used again; deleting a used coupon is refused.
    assert!(coupons::release(&mut tx, coupon.id, &winner).await.unwrap());
    assert_eq!(
        coupons::get(&mut tx, coupon.id).await.unwrap().used_count,
        0
    );
    coupons::redeem(&mut tx, coupon.id, "b@example.test", "order-c", Utc::now())
        .await
        .unwrap();
    assert_eq!(
        coupons::delete(&mut tx, "u", coupon.id)
            .await
            .unwrap_err()
            .code(),
        "coupon_in_use"
    );
    // Per-customer limit.
    let per = coupons::create(
        &mut tx,
        "u",
        &CouponInput {
            code: "ONCE".into(),
            discount: CouponDiscount::FreeShipping,
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: None,
            per_customer_limit: Some(1),
            published: false,
        },
    )
    .await
    .unwrap();
    coupons::redeem(&mut tx, per.id, "c@example.test", "o1", Utc::now())
        .await
        .unwrap();
    assert_eq!(
        coupons::redeem(&mut tx, per.id, "c@example.test", "o2", Utc::now())
            .await
            .unwrap_err()
            .code(),
        "coupon_customer_limit"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn stock_movements_and_the_last_unit_race(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let product = testkit::catalog::product(&runtime, tenant, "S", 1).await;
    let v = product.variants[0].id;

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let lvl = inventory::get(&mut tx, v).await.unwrap();
    assert_eq!((lvl.on_hand, lvl.track), (0, true));
    // Nothing on hand: a reservation fails closed.
    let r = MovementRef {
        ref_type: "order",
        ref_id: "o0",
    };
    assert_eq!(
        inventory::reserve(&mut tx, &r, v, 1)
            .await
            .unwrap_err()
            .code(),
        "insufficient_stock"
    );
    tx.rollback().await.unwrap();

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let adj = Adjustment {
        delta: None,
        on_hand: Some(1),
        note: Some("count".into()),
    };
    let moved = inventory::adjust(&mut tx, "clerk", v, "key-1", &adj)
        .await
        .unwrap();
    assert!(moved.applied);
    assert_eq!(moved.level.on_hand, 1);
    // Replayed adjustment (same key) is a no-op.
    let replay = inventory::adjust(
        &mut tx,
        "clerk",
        v,
        "key-1",
        &Adjustment {
            delta: Some(1),
            on_hand: None,
            note: None,
        },
    )
    .await
    .unwrap();
    assert!(!replay.applied);
    assert_eq!(replay.level.on_hand, 1);
    tx.commit().await.unwrap();

    // Two orders race for the last unit: exactly one wins.
    let reserve = |order: &'static str| {
        let runtime = runtime.clone();
        async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let r = inventory::reserve(
                &mut tx,
                &MovementRef {
                    ref_type: "order",
                    ref_id: order,
                },
                v,
                1,
            )
            .await;
            if r.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                tx.commit().await.unwrap();
            }
            r.map(|m| m.level.available).map_err(|e| e.code())
        }
    };
    let (a, b) = tokio::join!(reserve("o1"), reserve("o2"));
    assert_eq!(
        [&a, &b].iter().filter(|r| r.is_ok()).count(),
        1,
        "{a:?} {b:?}"
    );
    assert!(a == Err("insufficient_stock") || b == Err("insufficient_stock"));
    let winner = if a.is_ok() { "o1" } else { "o2" };

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let r = MovementRef {
        ref_type: "order",
        ref_id: winner,
    };
    // Replaying the reservation does not double-reserve.
    assert!(!inventory::reserve(&mut tx, &r, v, 1).await.unwrap().applied);
    // Cannot count stock below the reserved unit.
    assert_eq!(
        inventory::adjust(
            &mut tx,
            "clerk",
            v,
            "key-2",
            &Adjustment {
                delta: Some(-1),
                on_hand: None,
                note: None
            }
        )
        .await
        .unwrap_err()
        .code(),
        "below_reserved"
    );
    tx.rollback().await.unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let shipped = inventory::commit(&mut tx, &r, v, 1).await.unwrap();
    assert_eq!((shipped.level.on_hand, shipped.level.reserved), (0, 0));
    let back = inventory::restock(
        &mut tx,
        "clerk",
        &MovementRef {
            ref_type: "return",
            ref_id: "r1",
        },
        v,
        1,
    )
    .await
    .unwrap();
    assert_eq!(back.level.available, 1);
    assert_eq!(
        inventory::release(
            &mut tx,
            &MovementRef {
                ref_type: "order",
                ref_id: "x"
            },
            v,
            1
        )
        .await
        .unwrap_err()
        .code(),
        "insufficient_reserved"
    );
    tx.rollback().await.unwrap();

    // Backorders allow selling below zero; untracking stops the checks.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    inventory::update_settings(
        &mut tx,
        "clerk",
        v,
        &LevelSettings {
            track: true,
            allow_backorder: true,
        },
    )
    .await
    .unwrap();
    let r = inventory::reserve(
        &mut tx,
        &MovementRef {
            ref_type: "order",
            ref_id: "bo",
        },
        v,
        5,
    )
    .await
    .unwrap();
    assert_eq!(r.level.available, -5);
    assert_eq!(
        inventory::update_settings(
            &mut tx,
            "clerk",
            v,
            &LevelSettings {
                track: true,
                allow_backorder: false
            }
        )
        .await
        .unwrap_err()
        .code(),
        "below_reserved"
    );
    tx.rollback().await.unwrap();

    let changes = events(&db, tenant, "inventory.changed").await;
    let first = changes.iter().find(|e| e["kind"] == "adjust").unwrap();
    assert_eq!(first["before"]["on_hand"], 0);
    assert_eq!(first["after"]["on_hand"], 1);
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let page = inventory::movements(&mut tx, v, None, 10).await.unwrap();
    assert_eq!(page.items.len(), 2);
    let listed = inventory::list(&mut tx, Some(product.id), None, 10)
        .await
        .unwrap();
    assert_eq!(listed.items[0].level.reserved, 1);
}

const PRICING_TABLES: [&str; 9] = [
    "tax_profiles",
    "price_lists",
    "variant_prices",
    "price_intervals",
    "sales",
    "coupons",
    "coupon_redemptions",
    "inventory_levels",
    "stock_movements",
];

async fn fill(runtime: &PgPool, tenant: Uuid, prefix: &str) -> (Uuid, Uuid) {
    let product = testkit::catalog::product(runtime, tenant, prefix, 1).await;
    let v = product.variants[0].id;
    let list = testkit::pricing::price_list(runtime, tenant, "cz", Currency::Czk).await;
    testkit::pricing::set_prices(runtime, tenant, list.id, &[(v, 1000)]).await;
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    tax::upsert(&mut tx, "u", &cz_profile(DistanceSalesMode::Destination))
        .await
        .unwrap();
    sales::create(&mut tx, "u", &sale("s", 100, None, None, all()))
        .await
        .unwrap();
    let c = coupons::create(
        &mut tx,
        "u",
        &CouponInput {
            code: format!("{prefix}CODE"),
            discount: CouponDiscount::FreeShipping,
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: None,
            per_customer_limit: None,
            published: false,
        },
    )
    .await
    .unwrap();
    coupons::redeem(&mut tx, c.id, "k", "o", Utc::now())
        .await
        .unwrap();
    inventory::adjust(
        &mut tx,
        "u",
        v,
        "k",
        &Adjustment {
            delta: Some(3),
            on_hand: None,
            note: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (list.id, v)
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenants_cannot_reach_each_others_pricing(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let (b, _) = testkit::tenant(&runtime, "beta").await;
    let (list_a, v_a) = fill(&runtime, a, "A").await;
    fill(&runtime, b, "B").await;

    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    assert_eq!(
        pricing::get_price_list(&mut tx, list_a)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        inventory::get(&mut tx, v_a).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        pricing::upsert_prices(&mut tx, "u", list_a, &upsert(&[(v_a, 1)]))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    tx.rollback().await.unwrap();

    for table in PRICING_TABLES {
        let mut tx = tenant_tx(&runtime, b).await.unwrap();
        let (own, foreign): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FILTER (WHERE tenant_id = $1), count(*) FILTER (WHERE tenant_id <> $1) FROM {table}"
        )))
        .bind(b)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(own > 0, "{table}: fixture row missing");
        assert_eq!(foreign, 0, "{table}: A's rows visible to B");
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = $1"
        )))
        .bind(a)
        .execute(&mut *tx)
        .await
        .unwrap_err();
        // RLS WITH CHECK (or, for the append-only ledger, the missing UPDATE grant).
        assert_eq!(
            err.as_database_error().unwrap().code().as_deref(),
            Some("42501"),
            "{table}"
        );
        tx.rollback().await.unwrap();
    }

    // B cannot price A's variant in its own list: composite foreign keys.
    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    let list_b: Uuid = sqlx::query_scalar("SELECT id FROM price_lists")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let err = sqlx::query("INSERT INTO variant_prices (tenant_id, price_list_id, variant_id, amount_minor) VALUES ($1, $2, $3, 1)")
        .bind(b)
        .bind(list_b)
        .bind(v_a)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
}
