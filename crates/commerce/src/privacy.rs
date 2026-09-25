//! Personal-data helpers (spec §14). WP13 adds customer data export and erasure here.

use std::net::IpAddr;

use sha2::{Digest, Sha256};
use sqlx::PgConnection;

/// SHA-256 of the address under today's salt (UTC day). Salts are random, per day and deleted
/// after two days (`platform.purge_customer_auth`), so a stored hash can be compared with
/// others from the same day (rate limits) but never turned back into an address.
pub async fn ip_hash(conn: &mut PgConnection, ip: IpAddr) -> Result<Vec<u8>, sqlx::Error> {
    let fresh: [u8; 32] = rand::random();
    // The CTE's SELECT does not see the row its INSERT adds: exactly one branch yields a salt.
    let salt = sqlx::query_scalar!(
        r#"WITH new AS (
               INSERT INTO platform.ip_salts (day, salt)
               VALUES ((now() AT TIME ZONE 'utc')::date, $1)
               ON CONFLICT (day) DO NOTHING
               RETURNING salt
           )
           SELECT salt AS "salt!" FROM new
           UNION ALL
           SELECT salt FROM platform.ip_salts WHERE day = (now() AT TIME ZONE 'utc')::date
           LIMIT 1"#,
        &fresh[..]
    )
    .fetch_one(conn)
    .await?;
    let mut h = Sha256::new();
    h.update(&salt);
    h.update(ip.to_string().as_bytes());
    Ok(h.finalize().to_vec())
}
