//! Consent linking keeps decision chronology across browsers (A20).
#![allow(clippy::unwrap_used)]

use commerce::consent::{
    self, ConsentChoice, ConsentPurpose, Purposes, Source, Subject, new_anon_id,
};
use platform::db::tenant_tx;
use sqlx::PgPool;
use uuid::Uuid;

fn choice(analytics: bool) -> ConsentChoice {
    ConsentChoice {
        purposes: Purposes {
            analytics: Some(analytics),
            ..Purposes::default()
        },
        text_version: "v1".into(),
        source: Source::Banner,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_latest_decision_wins_however_sign_ins_interleave(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "consent").await;
    let customer = Uuid::now_v7();
    let (a, b) = (new_anon_id(), new_anon_id());
    let who = Subject::Customer(customer);
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    // Browser A grants, later browser B refuses.
    consent::record(&mut tx, &Subject::Anon(a.clone()), &choice(true), None)
        .await
        .unwrap();
    consent::record(&mut tx, &Subject::Anon(b.clone()), &choice(false), None)
        .await
        .unwrap();
    // A signs in first: its (older) grant is linked.
    consent::link_anonymous(&mut tx, &a, customer)
        .await
        .unwrap();
    assert!(
        consent::current(&mut tx, &who, ConsentPurpose::Analytics)
            .await
            .unwrap()
    );
    // B signs in: its refusal is newer than A's grant, whenever the links happen.
    consent::link_anonymous(&mut tx, &b, customer)
        .await
        .unwrap();
    assert!(
        !consent::current(&mut tx, &who, ConsentPurpose::Analytics)
            .await
            .unwrap()
    );
    // A signing in again changes nothing, and nothing is duplicated.
    consent::link_anonymous(&mut tx, &a, customer)
        .await
        .unwrap();
    assert!(
        !consent::current(&mut tx, &who, ConsentPurpose::Analytics)
            .await
            .unwrap()
    );
    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM consent_records WHERE subject_type = 'customer'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(linked, 2);
    tx.commit().await.unwrap();
}
