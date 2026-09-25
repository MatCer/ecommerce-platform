//! Customer rows: upsert by email. Imported customers have no password and get no email; they
//! sign in with a magic link like anyone else (which verifies the address). Re-importing
//! updates name, phone and locale where the file has them and adds an address only once.

use std::collections::{BTreeMap, HashSet};

use platform::Error;
use platform::db::TenantTx;

use super::imports::{Check, Defaults, ImportReport};
use super::table::{self, Field, Row, field};

pub const FIELDS: &[Field] = &[
    field("email", true),
    field("name", false),
    field("phone", false),
    field("locale", false),
    field("company", false),
    field("street", false),
    field("city", false),
    field("postal_code", false),
    field("country", false),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub email: String,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub locale: Option<String>,
    pub address: Option<Address>,
}

impl Record {
    pub fn preview(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::from([("email".to_owned(), self.email.clone())]);
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                m.insert(k.to_owned(), v.clone());
            }
        };
        put("name", &self.name);
        put("phone", &self.phone);
        put("locale", &self.locale);
        if let Some(a) = &self.address {
            m.insert(
                "address".into(),
                format!("{}, {} {}, {}", a.street, a.postal_code, a.city, a.country),
            );
        }
        m
    }
}

pub fn validate(rows: &[Row], _d: &Defaults, report: &mut ImportReport) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let mut c = Check::new(row, report);
        let email = c.req("email", table::email);
        let name = c.opt("name", |v| table::text(v, 200));
        let phone = c.opt("phone", table::phone);
        let locale = c.opt("locale", table::locale);
        let company = c.opt("company", |v| table::text(v, 200));
        let parts = ["street", "city", "postal_code", "country"];
        let address = if parts.iter().any(|p| c.has(p)) || company.is_some() {
            let street = c.req("street", |v| table::text(v, 200));
            let city = c.req("city", |v| table::text(v, 100));
            let postal_code = c.req("postal_code", |v| table::text(v, 20));
            let country = c.req("country", table::country);
            if name.is_none() {
                c.fail(Some("name"), "missing", "an address needs the customer's name");
            }
            match (street, city, postal_code, country, &name) {
                (Some(street), Some(city), Some(postal_code), Some(country), Some(n)) => {
                    Some(Address {
                        name: n.clone(),
                        company,
                        street,
                        city,
                        postal_code,
                        country,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(e) = &email
            && !seen.insert(e.clone())
        {
            c.fail(Some("email"), "duplicate", format!("{e} appears on an earlier line"));
        }
        if !c.ok {
            report.invalid_rows += 1;
            continue;
        }
        if let Some(email) = email {
            out.push(Record {
                email,
                name,
                phone,
                locale,
                address,
            });
        }
    }
    out
}

/// Dry run: how many exist already.
pub async fn classify(
    tx: &mut TenantTx,
    records: &[Record],
    report: &mut ImportReport,
) -> Result<(), Error> {
    let emails: Vec<String> = records.iter().map(|r| r.email.clone()).collect();
    let existing = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM customers WHERE email = ANY($1)"#,
        &emails
    )
    .fetch_one(&mut **tx)
    .await?;
    let existing = u32::try_from(existing).unwrap_or(u32::MAX);
    report.existing = existing;
    report.new = report.records.saturating_sub(existing);
    report.count(
        "with_address",
        records.iter().filter(|r| r.address.is_some()).count(),
    );
    Ok(())
}

/// Upserts one customer; `true` when it was created.
pub async fn apply(tx: &mut TenantTx, r: &Record, d: &Defaults) -> Result<bool, Error> {
    let row = sqlx::query!(
        r#"INSERT INTO customers (tenant_id, email, name, phone, locale)
           VALUES ($1, $2, $3, $4, coalesce($5, $6))
           ON CONFLICT ON CONSTRAINT customers_email_unique DO UPDATE SET
               name = coalesce(EXCLUDED.name, customers.name),
               phone = coalesce(EXCLUDED.phone, customers.phone),
               locale = coalesce($5, customers.locale),
               updated_at = now()
           RETURNING id, (xmax = 0) AS "created!""#,
        tx.tenant_id(),
        r.email,
        r.name,
        r.phone,
        r.locale,
        d.locale
    )
    .fetch_one(&mut **tx)
    .await?;
    if let Some(a) = &r.address {
        sqlx::query!(
            "INSERT INTO customer_addresses (tenant_id, customer_id, name, company, street, city,
                                             postal_code, country, is_default)
             SELECT $1, $2, $3, $4, $5, $6, $7, $8,
                    NOT EXISTS (SELECT 1 FROM customer_addresses
                                WHERE customer_id = $2 AND is_default)
             WHERE NOT EXISTS (SELECT 1 FROM customer_addresses
                               WHERE customer_id = $2 AND street = $5 AND city = $6
                                 AND postal_code = $7 AND country = $8)",
            tx.tenant_id(),
            row.id,
            a.name,
            a.company,
            a.street,
            a.city,
            a.postal_code,
            a.country
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(row.created)
}
