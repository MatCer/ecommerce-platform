//! Personal-data helpers (spec §14). WP13 adds customer data export and erasure here.

use std::net::IpAddr;

use sha2::{Digest, Sha256};
use sqlx::PgConnection;

/// SHA-256 of the address under today's salt (UTC day). Salts are random, per day and deleted
/// after two days (`platform.purge_customer_auth`), so a stored hash can be compared with
/// others from the same day (rate limits) but never turned back into an address.
pub async fn ip_hash(conn: &mut PgConnection, ip: IpAddr) -> Result<Vec<u8>, sqlx::Error> {
    let fresh: [u8; 32] = rand::random();
    // Two statements: when two requests create the day's salt at once, the loser's INSERT
    // waits for the winner and does nothing; the SELECT then runs with a fresh snapshot
    // (read committed) and sees the winner's row.
    sqlx::query!(
        "INSERT INTO platform.ip_salts (day, salt) VALUES ((now() AT TIME ZONE 'utc')::date, $1)
         ON CONFLICT (day) DO NOTHING",
        &fresh[..]
    )
    .execute(&mut *conn)
    .await?;
    let salt = sqlx::query_scalar!(
        "SELECT salt FROM platform.ip_salts WHERE day = (now() AT TIME ZONE 'utc')::date"
    )
    .fetch_one(&mut *conn)
    .await?;
    let mut h = Sha256::new();
    h.update(&salt);
    h.update(ip.to_string().as_bytes());
    Ok(h.finalize().to_vec())
}
