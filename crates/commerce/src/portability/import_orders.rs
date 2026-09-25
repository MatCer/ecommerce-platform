//! Historical order rows (A28): one row per order line, rows of one `order_number` form the
//! order (order columns may repeat on every row but must not disagree). Orders land in
//! `archived_orders` only: no payment, stock movement, invoice, email, webhook or analytics
//! event, ever. Re-importing a number replaces the archived record; the order is linked to the
//! customer account with the same email when there is one.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::imports::{Check, DataImportReport, Defaults};
use super::table::{self, Field, Row, field};

pub const FIELDS: &[Field] = &[
    field("order_number", true),
    field("placed_at", true),
    field("email", true),
    field("currency", true),
    field("total", true),
    field("status", false),
    field("name", false),
    field("phone", false),
    field("company", false),
    field("street", false),
    field("city", false),
    field("postal_code", false),
    field("country", false),
    field("sku", false),
    field("item_name", false),
    field("quantity", false),
    field("unit_price", false),
];

pub const MAX_LINES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub sku: Option<String>,
    pub name: String,
    pub quantity: i32,
    pub unit_price_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub number: String,
    pub placed_at: DateTime<Utc>,
    pub email: String,
    pub currency: String,
    pub total_minor: i64,
    pub status: Option<String>,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub address: Option<serde_json::Value>,
    pub lines: Vec<Line>,
}

impl Record {
    pub fn preview(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("order_number".to_owned(), self.number.clone()),
            ("placed_at".to_owned(), self.placed_at.to_rfc3339()),
            ("email".to_owned(), self.email.clone()),
            (
                "total".to_owned(),
                format!(
                    "{}.{:02} {}",
                    self.total_minor / 100,
                    self.total_minor % 100,
                    self.currency
                ),
            ),
            ("lines".to_owned(), self.lines.len().to_string()),
        ])
    }
}

/// The order-level cells of a row, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    placed_at: DateTime<Utc>,
    email: String,
    currency: String,
    total_minor: i64,
    status: Option<String>,
    name: Option<String>,
    phone: Option<String>,
    address: Option<serde_json::Value>,
}

fn head(c: &mut Check<'_>, now: DateTime<Utc>) -> Option<Head> {
    let placed_at = c.req("placed_at", |v| table::past_time(v, now));
    let email = c.req("email", table::email);
    let currency = c.req("currency", table::currency);
    let total_minor = c.req("total", table::amount);
    let status = c.opt("status", |v| table::text(v, 64));
    let name = c.opt("name", |v| table::text(v, 200));
    let phone = c.opt("phone", table::phone);
    let company = c.opt("company", |v| table::text(v, 200));
    let address = if ["street", "city", "postal_code", "country"]
        .iter()
        .any(|p| c.has(p))
    {
        let street = c.req("street", |v| table::text(v, 200));
        let city = c.req("city", |v| table::text(v, 100));
        let postal_code = c.req("postal_code", |v| table::text(v, 20));
        let country = c.req("country", table::country);
        match (street, city, postal_code, country) {
            (Some(street), Some(city), Some(postal_code), Some(country)) => Some(json!({
                "name": name, "company": company, "street": street, "city": city,
                "postal_code": postal_code, "country": country,
            })),
            _ => None,
        }
    } else {
        None
    };
    Some(Head {
        placed_at: placed_at?,
        email: email?,
        currency: currency?,
        total_minor: total_minor?,
        status,
        name,
        phone,
        address,
    })
}

/// The line on a row, if it has one.
fn line(c: &mut Check<'_>) -> Option<Option<Line>> {
    if !["sku", "item_name", "quantity", "unit_price"]
        .iter()
        .any(|p| c.has(p))
    {
        return Some(None);
    }
    let sku = c.opt("sku", |v| table::text(v, 100));
    let name = c.req("item_name", |v| table::text(v, 300));
    let quantity = c.req("quantity", table::quantity);
    let unit_price_minor = c.req("unit_price", table::amount);
    Some(Some(Line {
        sku,
        name: name?,
        quantity: quantity?,
        unit_price_minor: unit_price_minor?,
    }))
}

pub fn validate(rows: &[Row], d: &Defaults, report: &mut DataImportReport) -> Vec<Record> {
    // Orders in first-appearance order; an order with any bad row is left out entirely.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (Option<Record>, bool)> = HashMap::new();
    for row in rows {
        let mut c = Check::new(row, report);
        let Some(number) = c.req("order_number", |v| table::text(v, 64)) else {
            report.invalid_rows += 1;
            continue;
        };
        let head = head(&mut c, d.now);
        let line = line(&mut c);
        let entry = groups.entry(number.clone()).or_insert_with(|| {
            order.push(number.clone());
            (None, true)
        });
        match (head, line, c.ok) {
            (Some(h), Some(l), true) => match &mut entry.0 {
                None => {
                    entry.0 = Some(Record {
                        number,
                        placed_at: h.placed_at,
                        email: h.email,
                        currency: h.currency,
                        total_minor: h.total_minor,
                        status: h.status,
                        name: h.name,
                        phone: h.phone,
                        address: h.address,
                        lines: l.into_iter().collect(),
                    });
                }
                Some(r) => {
                    let same = r.placed_at == h.placed_at
                        && r.email == h.email
                        && r.currency == h.currency
                        && r.total_minor == h.total_minor;
                    if !same {
                        c.fail(
                            None,
                            "conflicting_order",
                            format!(
                                "order {number} has different order columns on an earlier line"
                            ),
                        );
                        entry.1 = false;
                    } else if r.lines.len() == MAX_LINES {
                        c.fail(
                            None,
                            "too_many_lines",
                            format!("more than {MAX_LINES} lines"),
                        );
                        entry.1 = false;
                    } else {
                        r.lines.extend(l);
                    }
                }
            },
            _ => entry.1 = false,
        }
        if !c.ok {
            report.invalid_rows += 1;
        }
    }
    order
        .into_iter()
        .filter_map(|n| match groups.remove(&n) {
            Some((Some(r), true)) => Some(r),
            _ => None,
        })
        .collect()
}

pub async fn classify(
    tx: &mut TenantTx,
    records: &[Record],
    report: &mut DataImportReport,
) -> Result<(), Error> {
    let numbers: Vec<String> = records.iter().map(|r| r.number.clone()).collect();
    let emails: Vec<String> = records.iter().map(|r| r.email.clone()).collect();
    let existing = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM archived_orders WHERE number = ANY($1)"#,
        &numbers
    )
    .fetch_one(&mut **tx)
    .await?;
    let linked = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM unnest($1::text[]) AS e(email)
           WHERE EXISTS (SELECT 1 FROM customers c WHERE c.email = e.email)"#,
        &emails
    )
    .fetch_one(&mut **tx)
    .await?;
    report.existing = u32::try_from(existing).unwrap_or(u32::MAX);
    report.new = report.records.saturating_sub(report.existing);
    report.count("linked_to_customer", usize::try_from(linked).unwrap_or(0));
    report.count("lines", records.iter().map(|r| r.lines.len()).sum());
    Ok(())
}

pub async fn apply(tx: &mut TenantTx, r: &Record, import_id: Uuid) -> Result<bool, Error> {
    let lines = serde_json::to_value(&r.lines).map_err(|e| Error::Internal(e.to_string()))?;
    let created = sqlx::query_scalar!(
        r#"INSERT INTO archived_orders (tenant_id, number, placed_at, email, customer_id, name,
                                        phone, currency, total_minor, status_label, address,
                                        lines, import_id)
           VALUES ($1, $2, $3, $4, (SELECT id FROM customers WHERE email = $4), $5, $6, $7, $8,
                   $9, $10, $11, $12)
           ON CONFLICT ON CONSTRAINT archived_orders_number_unique DO UPDATE SET
               placed_at = EXCLUDED.placed_at, email = EXCLUDED.email,
               customer_id = EXCLUDED.customer_id, name = EXCLUDED.name, phone = EXCLUDED.phone,
               currency = EXCLUDED.currency, total_minor = EXCLUDED.total_minor,
               status_label = EXCLUDED.status_label, address = EXCLUDED.address,
               lines = EXCLUDED.lines, import_id = EXCLUDED.import_id, updated_at = now()
           RETURNING (xmax = 0) AS "created!""#,
        tx.tenant_id(),
        r.number,
        r.placed_at,
        r.email,
        r.name,
        r.phone,
        r.currency,
        r.total_minor,
        r.status,
        r.address,
        lines,
        import_id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn d() -> Defaults {
        Defaults::test()
    }

    fn row(line: u64, cells: &[(&'static str, &str)]) -> Row {
        Row::of(line, cells)
    }

    const HEAD: [(&str, &str); 5] = [
        ("order_number", "1001"),
        ("placed_at", "2024-03-01 10:00"),
        ("email", "anna@example.com"),
        ("currency", "CZK"),
        ("total", "300,00"),
    ];

    fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        HEAD.iter().copied().chain(extra.iter().copied()).collect()
    }

    #[test]
    fn groups_lines_by_order_number() {
        let rows = [
            row(
                2,
                &with(&[
                    ("item_name", "Tričko"),
                    ("quantity", "2"),
                    ("unit_price", "100"),
                ]),
            ),
            row(
                3,
                &with(&[
                    ("item_name", "Hrnek"),
                    ("quantity", "1"),
                    ("unit_price", "100"),
                    ("sku", "MUG"),
                ]),
            ),
            row(
                4,
                &[
                    ("order_number", "1002"),
                    ("placed_at", "2024-03-02"),
                    ("email", "b@example.com"),
                    ("currency", "eur"),
                    ("total", "5"),
                ],
            ),
        ];
        let mut report = DataImportReport::default();
        let out = validate(&rows, &d(), &mut report);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].lines.len(), 2);
        assert_eq!(out[0].lines[1].sku.as_deref(), Some("MUG"));
        assert_eq!(out[0].total_minor, 30_000);
        assert_eq!(out[1].currency, "EUR");
        assert!(out[1].lines.is_empty());
    }

    #[test]
    fn a_bad_row_drops_its_whole_order() {
        let rows = [
            row(
                2,
                &with(&[
                    ("item_name", "Tričko"),
                    ("quantity", "2"),
                    ("unit_price", "100"),
                ]),
            ),
            row(
                3,
                &with(&[
                    ("item_name", "Hrnek"),
                    ("quantity", "zero"),
                    ("unit_price", "100"),
                ]),
            ),
            row(
                4,
                &[
                    ("order_number", "1001"),
                    ("placed_at", "2024-03-01 10:00"),
                    ("email", "other@example.com"),
                    ("currency", "CZK"),
                    ("total", "300"),
                ],
            ),
            row(
                5,
                &[
                    ("order_number", "1003"),
                    ("placed_at", "2099-01-01"),
                    ("email", "c@example.com"),
                    ("currency", "CZK"),
                    ("total", "1"),
                ],
            ),
            row(
                6,
                &[
                    ("order_number", "1004"),
                    ("email", "d@example.com"),
                    ("currency", "CZK"),
                    ("total", "1"),
                    ("street", "Main 1"),
                ],
            ),
        ];
        let mut report = DataImportReport::default();
        let out = validate(&rows, &d(), &mut report);
        assert!(out.is_empty());
        let codes: Vec<(u64, &str)> = report
            .errors
            .iter()
            .map(|e| (e.line, e.code.as_str()))
            .collect();
        assert!(codes.contains(&(3, "invalid_quantity")), "{codes:?}");
        assert!(codes.contains(&(4, "conflicting_order")), "{codes:?}");
        assert!(codes.contains(&(5, "in_future")), "{codes:?}");
        assert!(codes.contains(&(6, "missing")), "{codes:?}");
        assert_eq!(report.invalid_rows, 4);
    }
}
