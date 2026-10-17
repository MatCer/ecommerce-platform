use std::time::Duration;

use chrono::{DateTime, Utc};
use commerce::capability;
use commerce::cart::{self, CartCoupon, NewLine};
use commerce::checkout::{self, CheckoutView, ContactInput, Details, Settings, ShippingInput};
use commerce::checkout_access::{self, ShopOrigin};
use commerce::money::Currency;
use commerce::payments::Payments;
use commerce::pricing::cart::CouponDiscount;
use commerce::promotions::coupons::{self, CouponInput};
use commerce::shipping::{self, Carrier, ShippingMethodInput};
use commerce::storefront::{self, Context, PublicUrls};
use commerce::tenancy::CheckoutMode;
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use testkit::storefront::{Shop, shop};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Clone)]
struct At {
    tenant: Uuid,
    market: Uuid,
    origin: Option<ShopOrigin>,
}

fn at(shop: &Shop, market: Uuid, origin: &ShopOrigin) -> At {
    At {
        tenant: shop.tenant,
        market,
        origin: Some(origin.clone()),
    }
}

impl At {
    async fn tx(&self, runtime: &PgPool) -> (TenantTx, Context) {
        let mut tx = tenant_tx(runtime, self.tenant).await.unwrap();
        let mut ctx = storefront::context(
            &mut tx,
            &PublicUrls::default(),
            self.market,
            None,
            Utc::now(),
        )
        .await
        .unwrap();
        ctx.origin = self.origin.clone();
        (tx, ctx)
    }

    async fn run<T>(
        &self,
        runtime: &PgPool,
        f: impl AsyncFnOnce(&mut TenantTx, &Context) -> T,
    ) -> T {
        let (mut tx, ctx) = self.tx(runtime).await;
        let out = f(&mut tx, &ctx).await;
        tx.commit().await.unwrap();
        out
    }
}

fn settings() -> Settings {
    Settings {
        payments: Payments::default(),
        packeta: None,
    }
}

fn contact(email: &str) -> ContactInput {
    ContactInput {
        email: email.into(),
        phone: None,
    }
}

async fn shop_cart(runtime: &PgPool, at: &At, shop: &Shop) -> String {
    at.run(runtime, async |tx, ctx| {
        let (_, token) = cart::create(tx, ctx).await.unwrap();
        let c = cart::find(tx, ctx, &token).await.unwrap();
        let line = NewLine {
            variant_id: shop.variants[0],
            quantity: 1,
        };
        cart::add_line(tx, ctx, &c, &line).await.unwrap();
        token
    })
    .await
}

async fn start(runtime: &PgPool, at: &At, shop_token: &str) -> Result<String, Error> {
    at.run(runtime, async |tx, ctx| {
        checkout_access::start_handoff(tx, ctx, shop_token).await
    })
    .await
}

async fn same_origin_start(
    runtime: &PgPool,
    at: &At,
    shop_token: &str,
    previous: Option<&str>,
) -> Result<String, Error> {
    at.run(runtime, async |tx, ctx| {
        checkout_access::start(tx, ctx, shop_token, previous).await
    })
    .await
}

async fn redeem(runtime: &PgPool, at: &At, handoff: &str) -> Option<String> {
    at.run(runtime, async |tx, ctx| {
        checkout_access::redeem_handoff(tx, ctx, handoff)
            .await
            .unwrap()
    })
    .await
}

async fn open(runtime: &PgPool, at: &At, token: &str) -> Result<Uuid, Error> {
    at.run(runtime, async |tx, ctx| {
        checkout_access::open(tx, ctx, token).await.map(|c| c.id)
    })
    .await
}

async fn pending(runtime: &PgPool, at: &At, shop: &Shop) -> String {
    let shop_token = shop_cart(runtime, at, shop).await;
    start(runtime, at, &shop_token).await.unwrap()
}

async fn handed_off(runtime: &PgPool, at: &At, shop: &Shop) -> String {
    let handoff = pending(runtime, at, shop).await;
    redeem(runtime, at, &handoff).await.unwrap()
}

async fn written(runtime: &PgPool, at: &At, shop: &Shop, origin: &ShopOrigin) -> String {
    let shop_token = shop_cart(runtime, at, shop).await;
    let minted = capability::mint();
    at.run(runtime, async |tx, _| {
        sqlx::query(
            "UPDATE carts SET shop_token_hash = NULL, checkout_token_hash = $2,
                 checkout_origin = $3,
                 checkout_generation = coalesce(
                     (SELECT generation FROM platform.checkout_fences WHERE tenant_id = $4), 0)
             WHERE shop_token_hash = $1",
        )
        .bind(capability::hash(&shop_token))
        .bind(&minted.hash)
        .bind(origin.as_str())
        .bind(tx.tenant_id())
        .execute(&mut **tx)
        .await
        .unwrap();
    })
    .await;
    minted.token
}

async fn strip_binding_as_before_migration(runtime: &PgPool, tenant: Uuid, token: &str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "UPDATE carts SET checkout_origin = NULL, checkout_generation = NULL
         WHERE checkout_token_hash = $1",
    )
    .bind(capability::hash(token))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn alias(runtime: &PgPool, shop: &Shop, host: &str) -> ShopOrigin {
    sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, verified_at)
         VALUES ($1, $2, $3, now())",
    )
    .bind(host)
    .bind(shop.tenant)
    .bind(shop.cz)
    .execute(runtime)
    .await
    .unwrap();
    ShopOrigin::parse(&format!("https://{host}")).unwrap()
}

pub(crate) async fn attest(runtime: &PgPool, origin: &ShopOrigin) {
    sqlx::query(
        "UPDATE platform.checkout_origins SET history = 'merchant_attested_never_served',
             attested_by = 'user-owner', attested_at = now(), attestation_version = 1
         WHERE origin = $1",
    )
    .bind(origin.as_str())
    .execute(runtime)
    .await
    .unwrap();
}

pub(crate) async fn switch(runtime: &PgPool, shop: &Shop, origin: &ShopOrigin, mode: CheckoutMode) {
    let changed = checkout_access::switch_mode(runtime, shop.tenant, origin, mode)
        .await
        .unwrap();
    assert!(changed, "{} switched to {mode:?}", origin.as_str());
}

const EMAIL: &str = "SELECT email FROM carts WHERE checkout_token_hash = $1";

async fn read<T>(runtime: &PgPool, tenant: Uuid, sql: &'static str, token: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar(sql)
        .bind(capability::hash(token))
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn activity(runtime: &PgPool, tenant: Uuid, token: &str) -> DateTime<Utc> {
    let sql = "SELECT last_activity_at FROM carts WHERE checkout_token_hash = $1";
    read(runtime, tenant, sql, token).await
}

async fn set_activity(runtime: &PgPool, tenant: Uuid, token: &str, ago: &str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "UPDATE carts SET last_activity_at = now() - $2::interval WHERE checkout_token_hash = $1",
    )
    .bind(capability::hash(token))
    .bind(ago)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn handoff_unused(runtime: &PgPool, tenant: Uuid, handoff: &str) -> bool {
    let sql = "SELECT used_at IS NULL FROM checkout_handoffs WHERE token_hash = $1";
    read(runtime, tenant, sql, handoff).await
}

async fn live_without_lock(tx: &mut PgConnection, token: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM carts WHERE checkout_token_hash = $1)")
        .bind(capability::hash(token))
        .fetch_one(tx)
        .await
        .unwrap()
}

pub(crate) async fn pid(conn: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(conn)
        .await
        .unwrap()
}

pub(crate) async fn blocked_by(db: &PgPool, waiter: i32, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blockers: Vec<i32> = sqlx::query_scalar("SELECT pg_blocking_pids($1)")
                .bind(waiter)
                .fetch_one(db)
                .await
                .unwrap();
            if blockers.contains(&blocker) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("backend {waiter} never waited on {blocker}"));
}

fn not_found<T>(r: &Result<T, Error>) -> bool {
    matches!(r, Err(Error::NotFound))
}

fn mode_changed<T>(r: &Result<T, Error>) -> bool {
    matches!(r, Err(Error::Conflict { code, .. }) if *code == "checkout_mode_changed")
}

#[sqlx::test(migrations = "../../migrations")]
async fn capability_wrong_shop_origin_generation_denied(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "deny").await;
    let other = testkit::storefront::shop(&runtime, "deny-other").await;
    let alias = alias(&runtime, &shop, "deny-alias.localhost").await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;
    let unbound = written(&runtime, &here, &shop, &shop.origin).await;
    strip_binding_as_before_migration(&runtime, shop.tenant, &unbound).await;
    set_activity(&runtime, shop.tenant, &k, "1 hour").await;
    set_activity(&runtime, shop.tenant, &unbound, "1 hour").await;
    let before = activity(&runtime, shop.tenant, &k).await;
    let unbound_before = activity(&runtime, shop.tenant, &unbound).await;

    for (what, place, token) in [
        ("another tenant", at(&other, other.cz, &other.origin), &k),
        (
            "an alias of the same market",
            at(&shop, shop.cz, &alias),
            &k,
        ),
        (
            "another market's origin",
            at(&shop, shop.sk, &shop.sk_origin),
            &k,
        ),
        ("an unbound pre-migration hash", here.clone(), &unbound),
    ] {
        let r = open(&runtime, &place, token).await;
        assert!(not_found(&r), "{what}: {r:?}");
    }
    for (what, place, code) in [
        (
            "no origin",
            At {
                origin: None,
                ..here.clone()
            },
            "shop_origin_required",
        ),
        (
            "another market's origin row",
            at(&shop, shop.cz, &shop.sk_origin),
            "shop_origin_mismatch",
        ),
        (
            "another tenant's origin row",
            at(&shop, shop.cz, &other.origin),
            "shop_origin_mismatch",
        ),
    ] {
        let r = open(&runtime, &place, &k).await;
        assert!(
            matches!(r, Err(Error::Forbidden { code: c }) if c == code),
            "{what}: {r:?}"
        );
    }
    assert_eq!(
        activity(&runtime, shop.tenant, &k).await,
        before,
        "no refusal refreshes the capability"
    );
    assert_eq!(
        activity(&runtime, shop.tenant, &unbound).await,
        unbound_before,
        "an unbound hash is not refreshed"
    );

    assert!(
        open(&runtime, &here, &k).await.is_ok(),
        "control: the right origin opens"
    );
    let after = activity(&runtime, shop.tenant, &k).await;
    assert!(after > before, "a valid open refreshes");

    attest(&runtime, &shop.sk_origin).await;
    switch(&runtime, &shop, &shop.sk_origin, CheckoutMode::SameOrigin).await;
    let r = open(&runtime, &here, &k).await;
    assert!(not_found(&r), "a stale shop generation: {r:?}");
    assert_eq!(activity(&runtime, shop.tenant, &k).await, after);
}

#[sqlx::test(migrations = "../../migrations")]
async fn idle_24h_reads_refresh_invalid_reads_do_not(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "idle").await;
    let alias = alias(&runtime, &shop, "idle-alias.localhost").await;
    let here = at(&shop, shop.cz, &shop.origin);
    let there = at(&shop, shop.cz, &alias);
    attest(&runtime, &shop.origin).await;
    switch(&runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    let s = written(&runtime, &here, &shop, &shop.origin).await;

    set_activity(&runtime, shop.tenant, &s, "23 hours").await;
    let at_23h = activity(&runtime, shop.tenant, &s).await;
    assert!(
        not_found(&open(&runtime, &there, &s).await),
        "an alias read at 23 h is denied"
    );
    assert_eq!(
        activity(&runtime, shop.tenant, &s).await,
        at_23h,
        "the alias read refreshed nothing"
    );
    assert!(
        open(&runtime, &here, &s).await.is_ok(),
        "a valid read at 23 h passes"
    );
    assert!(
        activity(&runtime, shop.tenant, &s).await > at_23h,
        "the valid read refreshed the idle clock"
    );

    set_activity(&runtime, shop.tenant, &s, "24 hours 1 minute").await;
    let expired = activity(&runtime, shop.tenant, &s).await;
    assert!(
        not_found(&open(&runtime, &here, &s).await),
        "24 h 1 min without use expires a same-origin capability"
    );
    assert_eq!(activity(&runtime, shop.tenant, &s).await, expired);

    let isolated = handed_off(&runtime, &there, &shop).await;
    set_activity(&runtime, shop.tenant, &isolated, "25 hours").await;
    assert!(
        open(&runtime, &there, &isolated).await.is_ok(),
        "an isolated capability keeps the 30-day idle limit"
    );
}

async fn queued_behind_rotation<V>(
    db: &PgPool,
    runtime: &PgPool,
    here: &At,
    k: &str,
    attack: Option<Uuid>,
    victim_writes: impl AsyncFnOnce(&mut TenantTx, &Context, &cart::CartRef) -> V,
) -> (Result<CheckoutView, Error>, String, V) {
    let (mut victim, vctx) = here.tx(runtime).await;
    let c = checkout_access::open(&mut victim, &vctx, k).await.unwrap();
    let victim_pid = pid(&mut victim).await;
    let (pid_tx, pid_rx) = oneshot::channel();
    let attacker = tokio::spawn({
        let (runtime, here, k) = (runtime.clone(), here.clone(), k.to_owned());
        async move {
            let (mut tx, ctx) = here.tx(&runtime).await;
            assert!(
                live_without_lock(&mut tx, &k).await,
                "preliminary authorization: K is live"
            );
            pid_tx.send(pid(&mut tx).await).unwrap();
            let c = checkout_access::open(&mut tx, &ctx, &k).await?;
            if let Some(method_id) = attack {
                let pick = ShippingInput {
                    method_id,
                    pickup_point: None,
                };
                checkout::set_shipping(&mut tx, &ctx, &c, &pick).await?;
            }
            let view = checkout::view(&mut tx, &ctx, &settings(), &c).await?;
            tx.commit().await?;
            Ok(view)
        }
    });
    blocked_by(db, pid_rx.await.unwrap(), victim_pid).await;
    let written = victim_writes(&mut victim, &vctx, &c).await;
    let rotated = checkout_access::rotate(&mut victim, &vctx, k)
        .await
        .unwrap();
    victim.commit().await.unwrap();
    (attacker.await.unwrap(), rotated, written)
}

#[sqlx::test(migrations = "../../migrations")]
async fn protected_read_queued_before_rotation_fails(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = shop(&runtime, "rotread").await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;

    let (attack, rotated, ()) =
        queued_behind_rotation(&db, &runtime, &here, &k, None, async |tx, ctx, _| {
            let victim = Details::Contact(contact("victim@example.cz"));
            checkout::set_isolated_details(tx, ctx, &k, &victim)
                .await
                .unwrap();
        })
        .await;
    assert!(not_found(&attack), "no victim bytes: {attack:?}");
    assert!(not_found(&open(&runtime, &here, &k).await), "K is dead");
    assert!(open(&runtime, &here, &rotated).await.is_ok(), "K' opens");
    assert_eq!(
        read::<String>(&runtime, shop.tenant, EMAIL, &rotated).await,
        "victim@example.cz"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn protected_write_queued_before_rotation_fails(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = shop(&runtime, "rotwrite").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let home = ShippingMethodInput {
        market_id: shop.cz,
        carrier: Carrier::Ppl,
        name_i18n: [("cs".to_owned(), "PPL".to_owned())].into(),
        description_i18n: Default::default(),
        price_minor: 9900,
        free_over_minor: None,
        weight_tiers: vec![],
        cod_allowed: false,
        cod_fee_minor: 0,
        active: true,
        position: 0,
    };
    let method = shipping::create(&mut tx, "test", &home).await.unwrap().id;
    tx.commit().await.unwrap();
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;

    let (write, rotated, version) = queued_behind_rotation(
        &db,
        &runtime,
        &here,
        &k,
        Some(method),
        async |tx, ctx, c| {
            let victim = Details::Contact(contact("victim@example.cz"));
            checkout::set_isolated_details(tx, ctx, &k, &victim)
                .await
                .unwrap();
            sqlx::query_scalar::<_, i32>("SELECT version FROM carts WHERE id = $1")
                .bind(c.id)
                .fetch_one(&mut **tx)
                .await
                .unwrap()
        },
    )
    .await;
    assert!(not_found(&write), "the queued write is refused: {write:?}");
    let sql = "SELECT shipping_method_id FROM carts WHERE checkout_token_hash = $1";
    let selected: Option<Uuid> = read(&runtime, shop.tenant, sql, &rotated).await;
    assert_eq!(selected, None, "the queued selection never landed");
    let sql = "SELECT version FROM carts WHERE checkout_token_hash = $1";
    let committed: i32 = read(&runtime, shop.tenant, sql, &rotated).await;
    assert_eq!(
        committed, version,
        "the version is what the victim committed"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn read_holding_lock_cannot_observe_later_write(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = shop(&runtime, "holdread").await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;

    let (mut reader, rctx) = here.tx(&runtime).await;
    let c = checkout_access::open(&mut reader, &rctx, &k).await.unwrap();
    let first = checkout::view(&mut reader, &rctx, &settings(), &c)
        .await
        .unwrap();
    let reader_pid = pid(&mut reader).await;
    let (pid_tx, pid_rx) = oneshot::channel();
    let writer = tokio::spawn({
        let (runtime, here, k) = (runtime.clone(), here.clone(), k.clone());
        async move {
            let (mut tx, ctx) = here.tx(&runtime).await;
            pid_tx.send(pid(&mut tx).await).unwrap();
            let later = Details::Contact(contact("later@example.cz"));
            checkout::set_isolated_details(&mut tx, &ctx, &k, &later).await?;
            let rotated = checkout_access::rotate(&mut tx, &ctx, &k).await?;
            tx.commit().await?;
            Ok::<_, Error>(rotated)
        }
    });
    blocked_by(&db, pid_rx.await.unwrap(), reader_pid).await;
    let second = checkout::view(&mut reader, &rctx, &settings(), &c)
        .await
        .unwrap();
    assert!(
        live_without_lock(&mut reader, &k).await,
        "the reader sees no rotation"
    );
    reader.commit().await.unwrap();
    let rotated = writer.await.unwrap().unwrap();

    assert_eq!(first, second, "the reader saw no later write");
    assert_eq!(first.email, None);
    assert!(not_found(&open(&runtime, &here, &k).await), "K is dead");
    assert_eq!(
        read::<String>(&runtime, shop.tenant, EMAIL, &rotated).await,
        "later@example.cz",
        "K' sees the write"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn switch_orders_against_inflight_requests(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = shop(&runtime, "order").await;
    attest(&runtime, &shop.origin).await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;

    let mut switching = runtime.begin().await.unwrap();
    let switch_pid = pid(&mut switching).await;
    sqlx::query("UPDATE platform.checkout_origins SET mode = 'same_origin' WHERE origin = $1")
        .bind(shop.origin.as_str())
        .execute(&mut *switching)
        .await
        .unwrap();
    let (pid_tx, pid_rx) = oneshot::channel();
    let request = tokio::spawn({
        let (runtime, here, k) = (runtime.clone(), here.clone(), k.clone());
        async move {
            let (mut tx, ctx) = here.tx(&runtime).await;
            assert!(
                live_without_lock(&mut tx, &k).await,
                "preliminary authorization: K is live"
            );
            pid_tx.send(pid(&mut tx).await).unwrap();
            let r = checkout_access::open(&mut tx, &ctx, &k).await.map(|c| c.id);
            tx.commit().await.unwrap();
            r
        }
    });
    blocked_by(&db, pid_rx.await.unwrap(), switch_pid).await;
    switching.commit().await.unwrap();
    let r = request.await.unwrap();
    assert!(
        not_found(&r),
        "a request queued behind a switch fails: {r:?}"
    );

    let k2 = written(&runtime, &here, &shop, &shop.origin).await;
    let (mut request, rctx) = here.tx(&runtime).await;
    checkout_access::open(&mut request, &rctx, &k2)
        .await
        .unwrap();
    let request_pid = pid(&mut request).await;
    let switch_pool = testkit::runtime_pool(&db, 1).await;
    let switch_pid = pid(&mut switch_pool.acquire().await.unwrap()).await;
    let switch = tokio::spawn({
        let (tenant, origin) = (shop.tenant, shop.origin.clone());
        async move {
            checkout_access::switch_mode(&switch_pool, tenant, &origin, CheckoutMode::Isolated)
                .await
        }
    });
    blocked_by(&db, switch_pid, request_pid).await;
    let first = Details::Contact(contact("first@example.cz"));
    let k2 = checkout::set_same_origin_details(&mut request, &rctx, &k2, &first)
        .await
        .unwrap();
    request.commit().await.unwrap();
    assert!(
        switch.await.unwrap().unwrap(),
        "the switch commits after the request"
    );
    assert_eq!(
        read::<String>(&runtime, shop.tenant, EMAIL, &k2).await,
        "first@example.cz",
        "the request's write committed"
    );
    assert!(
        not_found(&open(&runtime, &here, &k2).await),
        "the request's capability is dead after the switch"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn switch_revokes_every_capability_of_the_shop(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "revoke").await;
    let other = testkit::storefront::shop(&runtime, "revoke-other").await;
    let alias = alias(&runtime, &shop, "revoke-alias.localhost").await;
    attest(&runtime, &shop.origin).await;
    let places = [
        at(&shop, shop.cz, &shop.origin),
        at(&shop, shop.cz, &alias),
        at(&shop, shop.sk, &shop.sk_origin),
    ];
    let mut caps = Vec::new();
    for place in &places {
        caps.push(handed_off(&runtime, place, &shop).await);
    }
    let elsewhere = at(&other, other.cz, &other.origin);
    let survivor = handed_off(&runtime, &elsewhere, &other).await;
    let pending = pending(&runtime, &places[1], &shop).await;

    switch(&runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    for (place, k) in places.iter().zip(&caps) {
        let r = open(&runtime, place, k).await;
        assert!(
            not_found(&r),
            "{:?} survived the switch: {r:?}",
            place.origin
        );
    }
    assert!(
        open(&runtime, &elsewhere, &survivor).await.is_ok(),
        "another tenant's capability survives"
    );
    assert_eq!(
        redeem(&runtime, &places[1], &pending).await,
        None,
        "a pending alias handoff is revoked"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_generation_cannot_mint_or_consume(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "stale").await;
    attest(&runtime, &shop.origin).await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;
    let handoff = pending(&runtime, &here, &shop).await;
    let waiting = shop_cart(&runtime, &here, &shop).await;
    let restore = capability::mint();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO flow_restore_tokens (tenant_id, token_hash, cart_id, expires_at)
         SELECT tenant_id, $2, id, now() + interval '1 day' FROM carts WHERE shop_token_hash = $1",
    )
    .bind(capability::hash(&waiting))
    .bind(&restore.hash)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    switch(&runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    let r = start(&runtime, &here, &waiting).await;
    assert!(mode_changed(&r), "start: {r:?}");
    let sql = "SELECT count(*) FROM carts WHERE shop_token_hash = $1";
    let intact: i64 = read(&runtime, shop.tenant, sql, &waiting).await;
    assert_eq!(intact, 1, "the shop cart is intact");
    assert_eq!(redeem(&runtime, &here, &handoff).await, None);
    assert!(handoff_unused(&runtime, shop.tenant, &handoff).await);
    let r = here
        .run(&runtime, async |tx, ctx| {
            checkout_access::restore_cart(tx, ctx, &restore.token).await
        })
        .await;
    assert!(mode_changed(&r), "restore: {r:?}");
    let sql = "SELECT used_at IS NULL FROM flow_restore_tokens WHERE token_hash = $1";
    let unused: bool = read(&runtime, shop.tenant, sql, &restore.token).await;
    assert!(unused, "the restore link is not burned");

    switch(&runtime, &shop, &shop.origin, CheckoutMode::Isolated).await;
    assert_eq!(
        redeem(&runtime, &here, &handoff).await,
        None,
        "no ABA: the old handoff stays dead"
    );
    assert!(
        not_found(&open(&runtime, &here, &k).await),
        "no ABA: the old capability stays dead"
    );
    let fresh = start(&runtime, &here, &waiting).await.unwrap();
    let k2 = redeem(&runtime, &here, &fresh).await.unwrap();
    assert!(
        open(&runtime, &here, &k2).await.is_ok(),
        "a fresh start works"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn handoff_is_bound_to_its_origin(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "bound").await;
    let alias = alias(&runtime, &shop, "bound-alias.localhost").await;
    let here = at(&shop, shop.cz, &shop.origin);
    let there = at(&shop, shop.cz, &alias);
    let handoff = pending(&runtime, &here, &shop).await;

    assert_eq!(
        redeem(&runtime, &there, &handoff).await,
        None,
        "an alias cannot redeem it"
    );
    assert!(
        handoff_unused(&runtime, shop.tenant, &handoff).await,
        "nor burn it"
    );
    let k = redeem(&runtime, &here, &handoff).await.unwrap();
    assert_eq!(redeem(&runtime, &here, &handoff).await, None, "single use");
    assert!(open(&runtime, &here, &k).await.is_ok());
    assert!(
        not_found(&open(&runtime, &there, &k).await),
        "the capability opens only on its origin"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn switch_does_not_wait_for_fenceless_cart_writers(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "fenceless").await;
    attest(&runtime, &shop.origin).await;
    let here = at(&shop, shop.cz, &shop.origin);
    let k = handed_off(&runtime, &here, &shop).await;

    let mut writer = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("SELECT id FROM carts WHERE checkout_token_hash = $1 FOR UPDATE")
        .bind(capability::hash(&k))
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let switched = tokio::time::timeout(
        Duration::from_secs(10),
        checkout_access::switch_mode(
            &runtime,
            shop.tenant,
            &shop.origin,
            CheckoutMode::SameOrigin,
        ),
    )
    .await
    .expect("the switch waited for a cart row lock")
    .unwrap();
    assert!(switched);
    writer.commit().await.unwrap();
    assert!(
        not_found(&open(&runtime, &here, &k).await),
        "the capability on the locked cart is dead"
    );
}

async fn same_origin_shop(runtime: &PgPool, slug: &str) -> (Shop, At) {
    let shop = shop(runtime, slug).await;
    attest(runtime, &shop.origin).await;
    switch(runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    let here = at(&shop, shop.cz, &shop.origin);
    (shop, here)
}

async fn checkout_view(runtime: &PgPool, at: &At, token: &str) -> CheckoutView {
    at.run(runtime, async |tx, ctx| {
        let c = checkout_access::open(tx, ctx, token).await.unwrap();
        checkout::view(tx, ctx, &settings(), &c).await.unwrap()
    })
    .await
}

async fn shop_view(runtime: &PgPool, at: &At, shop_token: &str) -> Result<cart::CartView, Error> {
    at.run(runtime, async |tx, ctx| {
        let c = cart::find(tx, ctx, shop_token).await?;
        cart::view(tx, ctx, &c).await
    })
    .await
}

fn lines(view: &cart::CartView) -> Vec<(Uuid, u32)> {
    view.lines
        .iter()
        .map(|l| (l.variant_id, l.quantity))
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn start_links_the_snapshot_to_its_source(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, here) = same_origin_shop(&runtime, "snap").await;
    let shop_token = shop_cart(&runtime, &here, &shop).await;
    here.run(&runtime, async |tx, ctx| {
        coupons::create(
            tx,
            "test",
            &CouponInput {
                code: "SNAP10".into(),
                discount: CouponDiscount::Percent { basis_points: 1000 },
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
        let c = cart::find(tx, ctx, &shop_token).await.unwrap();
        let line = NewLine {
            variant_id: shop.variants[1],
            quantity: 2,
        };
        cart::add_line(tx, ctx, &c, &line).await.unwrap();
        cart::apply_coupon(tx, ctx, &c, "SNAP10").await.unwrap();
    })
    .await;
    here.run(&runtime, async |tx, _| {
        sqlx::query(
            "UPDATE carts SET email = 'planted@example.cz', phone = '+420 600 000 000',
                 billing_address = $2, shipping_address = $2, ship_to_country = 'SK'
             WHERE shop_token_hash = $1",
        )
        .bind(capability::hash(&shop_token))
        .bind(
            json!({"name": "Planted", "street": "Tajná 1", "city": "Brno",
                     "postal_code": "602 00", "country": "SK"}),
        )
        .execute(&mut **tx)
        .await
        .unwrap();
    })
    .await;
    let source = shop_view(&runtime, &here, &shop_token).await.unwrap();
    assert_eq!(
        lines(&source),
        [(shop.variants[0], 1), (shop.variants[1], 2)]
    );

    let k = same_origin_start(&runtime, &here, &shop_token, None)
        .await
        .unwrap();
    let v = checkout_view(&runtime, &here, &k).await;
    assert_ne!(v.cart.id, source.id, "the checkout is its own row");
    assert_eq!(lines(&v.cart), lines(&source), "the lines, in order");
    assert_eq!(
        v.cart.coupon,
        Some(CartCoupon {
            code: "SNAP10".into(),
            applied: true,
            reason: None,
        })
    );
    assert_eq!(v.cart.currency, Currency::Czk);
    let sql = "SELECT locale || ' ' || currency FROM carts WHERE checkout_token_hash = $1";
    assert_eq!(
        read::<String>(&runtime, shop.tenant, sql, &k).await,
        "cs CZK"
    );
    assert_eq!(
        (&v.email, &v.phone, &v.billing_address, &v.shipping_address),
        (&None, &None, &None, &None),
        "no contact or address is copied"
    );
    assert_eq!(v.cart.ship_to_country, "CZ", "nor the ship-to country");

    here.run(&runtime, async |tx, _| {
        sqlx::query(
            "UPDATE carts SET last_activity_at = now() - interval '31 days'
             WHERE shop_token_hash = $1",
        )
        .bind(capability::hash(&shop_token))
        .execute(&mut **tx)
        .await
        .unwrap();
    })
    .await;
    let (deleted, _) = commerce::ops::expired_carts(&runtime, shop.tenant)
        .await
        .unwrap();
    assert_eq!(deleted, 1, "the expiry job deletes the idle source");
    assert!(not_found(&shop_view(&runtime, &here, &shop_token).await));
    let kept = checkout_view(&runtime, &here, &k).await;
    assert_eq!(
        lines(&kept.cart),
        lines(&source),
        "the snapshot outlives its source"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn start_capability_is_bound_to_its_origin_and_idles_out(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, here) = same_origin_shop(&runtime, "snapidle").await;
    let alias = alias(&runtime, &shop, "snapidle-alias.localhost").await;
    let there = at(&shop, shop.cz, &alias);
    let shop_token = shop_cart(&runtime, &here, &shop).await;
    let k = same_origin_start(&runtime, &here, &shop_token, None)
        .await
        .unwrap();

    assert!(
        not_found(&open(&runtime, &there, &k).await),
        "an alias of the same market cannot open it"
    );
    set_activity(&runtime, shop.tenant, &k, "23 hours").await;
    assert!(
        open(&runtime, &here, &k).await.is_ok(),
        "passes at 23 h idle"
    );
    set_activity(&runtime, shop.tenant, &k, "24 hours 1 minute").await;
    assert!(
        not_found(&open(&runtime, &here, &k).await),
        "refused at 24 h 1 min idle"
    );
}
