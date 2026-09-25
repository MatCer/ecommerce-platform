//! Bank transfer (spec §10.4, A10, A25): the receiving account per market, the payment
//! instructions fixed at placement (account, variable symbol, amount, QR payload), statement
//! import and matching, the exceptions queue, payment reminders and the Fio API poller.
//!
//! - The variable symbol is the order number (≤ 10 digits). The unique index
//!   `payment_attempts_variable_symbol` keeps it unique per tenant and receiving account.
//! - Statement lines are identified by the bank's transaction id per account: a duplicate
//!   import inserts nothing and matches nothing twice.
//! - Matching is scoped to the tenant (RLS) and the receiving account: VS + currency select the
//!   attempt; an exact amount pays it (through [`super::apply_outcome`], so money after the
//!   deadline is a late payment: order exception, no restock); a short or excess amount, an
//!   unknown VS, another currency or a transfer for an order already paid waits in the
//!   exceptions queue for a person.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use platform::Error;
use platform::crypto::SecretBox;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::qr::{self, QrKind, Transfer};
use super::statements::{self, Statement, StatementLine};
use super::{AttemptStatus, MethodKind, Outcome};
use crate::audit;
use crate::markets::invalid;
use crate::money::{Currency, Locale, Money, MoneyView};

// ---------------------------------------------------------------------------------------
// Receiving accounts

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BankAccount {
    pub id: Uuid,
    pub market_id: Uuid,
    pub currency: String,
    pub iban: String,
    pub bic: Option<String>,
    pub account_name: String,
    /// The market's current account; retired ones still import and match statements.
    pub active: bool,
    /// A Fio API token is stored (encrypted; never returned).
    pub fio_connected: bool,
    pub fio_synced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BankAccountInput {
    /// Spaces are ignored; the check digits are verified.
    pub iban: String,
    #[serde(default)]
    pub bic: Option<String>,
    /// The account holder (shown to customers, PAY by square beneficiary), 1-70 characters.
    pub account_name: String,
    /// Sets the Fio API token (write-only, stored encrypted); omit to keep the current one.
    #[serde(default)]
    pub fio_token: Option<String>,
    /// Removes the stored Fio API token.
    #[serde(default)]
    pub clear_fio_token: bool,
}

/// ISO 13616 IBAN: normalized (no spaces, upper case) when the check digits hold.
pub fn normalize_iban(raw: &str) -> Option<String> {
    let iban: String = raw
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let valid_shape = (15..=34).contains(&iban.len())
        && iban.bytes().take(2).all(|b| b.is_ascii_uppercase())
        && iban.bytes().skip(2).take(2).all(|b| b.is_ascii_digit())
        && iban.bytes().all(|b| b.is_ascii_alphanumeric());
    if !valid_shape {
        return None;
    }
    let rearranged = iban[4..].bytes().chain(iban[..4].bytes());
    let mut rem: u32 = 0;
    for b in rearranged {
        let v = if b.is_ascii_digit() {
            u32::from(b - b'0')
        } else {
            u32::from(b - b'A') + 10
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    (rem == 1).then_some(iban)
}

fn normalize_bic(raw: &str) -> Option<String> {
    let bic = raw.trim().to_ascii_uppercase();
    let ok = matches!(bic.len(), 8 | 11)
        && bic.bytes().take(6).all(|b| b.is_ascii_uppercase())
        && bic.bytes().skip(6).all(|b| b.is_ascii_alphanumeric());
    ok.then_some(bic)
}

/// What binds a stored token to its row (AES-GCM associated data).
fn token_aad(tenant_id: Uuid, account_id: Uuid) -> Vec<u8> {
    [tenant_id.as_bytes().as_slice(), account_id.as_bytes()].concat()
}

/// The receiving account of a market (the active one).
pub async fn account(tx: &mut TenantTx, market_id: Uuid) -> Result<Option<BankAccount>, Error> {
    let id = sqlx::query_scalar!(
        "SELECT id FROM bank_accounts WHERE market_id = $1 AND active",
        market_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    match id {
        Some(id) => account_by_id(tx, id).await.map(Some),
        None => Ok(None),
    }
}

/// Any account of the tenant, active or retired (statements of a retired account still match
/// the orders placed while it was active).
pub async fn account_by_id(tx: &mut TenantTx, id: Uuid) -> Result<BankAccount, Error> {
    sqlx::query!(
        r#"SELECT id, market_id, currency, iban, bic, account_name, active,
                  fio_token IS NOT NULL AS "fio_connected!", fio_synced_at
           FROM bank_accounts WHERE id = $1"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|r| BankAccount {
        id: r.id,
        market_id: r.market_id,
        currency: r.currency,
        iban: r.iban,
        bic: r.bic,
        account_name: r.account_name,
        active: r.active,
        fio_connected: r.fio_connected,
        fio_synced_at: r.fio_synced_at,
    })
    .ok_or(Error::NotFound)
}

/// Every receiving account of the tenant, active ones first.
pub async fn accounts(tx: &mut TenantTx) -> Result<Vec<BankAccount>, Error> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM bank_accounts ORDER BY active DESC, market_id, created_at DESC"
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(account_by_id(tx, id).await?);
    }
    Ok(out)
}

/// Sets the market's receiving account (payment settings: the caller checks role and fresh
/// authentication). The account currency is the market's. An account's IBAN never changes:
/// a different IBAN retires the current account (kept for importing and matching its
/// statements, with its Fio token) and activates a new or earlier one. Orders placed earlier
/// keep the instructions they were given.
pub async fn configure_account(
    tx: &mut TenantTx,
    actor: &str,
    secrets: Option<&SecretBox>,
    market_id: Uuid,
    input: &BankAccountInput,
) -> Result<BankAccount, Error> {
    const CODE: &str = "invalid_bank_account";
    let iban = normalize_iban(&input.iban)
        .ok_or_else(|| invalid(CODE, "the IBAN is not valid (check digits)"))?;
    let bic = match input
        .bic
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(b) => Some(normalize_bic(b).ok_or_else(|| invalid(CODE, "the BIC is not valid"))?),
        None => None,
    };
    let name = input.account_name.trim();
    if !(1..=70).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err(invalid(CODE, "account_name must be 1-70 characters"));
    }
    if input.fio_token.is_some() && input.clear_fio_token {
        return Err(invalid(CODE, "set or clear the Fio token, not both"));
    }
    // The market row lock serializes concurrent changes of its account.
    let currency = sqlx::query_scalar!(
        "SELECT currency FROM markets WHERE id = $1 FOR UPDATE",
        market_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let existing = sqlx::query!(
        "SELECT id, active FROM bank_accounts WHERE market_id = $1 AND iban = $2",
        market_id,
        iban
    )
    .fetch_optional(&mut **tx)
    .await?;
    let retired = sqlx::query_scalar!(
        "UPDATE bank_accounts SET active = false, updated_at = now()
         WHERE market_id = $1 AND active AND iban <> $2 RETURNING id",
        market_id,
        iban
    )
    .fetch_optional(&mut **tx)
    .await?;
    let id = match existing {
        Some(e) => {
            sqlx::query!(
                "UPDATE bank_accounts SET bic = $2, account_name = $3, active = true,
                     updated_at = now()
                 WHERE id = $1",
                e.id,
                bic,
                name
            )
            .execute(&mut **tx)
            .await?;
            e.id
        }
        None => {
            sqlx::query_scalar!(
                "INSERT INTO bank_accounts (id, tenant_id, market_id, currency, iban, bic, account_name)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
                crate::id::new_id(),
                tx.tenant_id(),
                market_id,
                currency,
                iban,
                bic,
                name
            )
            .fetch_one(&mut **tx)
            .await?
        }
    };
    let token_change = match (&input.fio_token, input.clear_fio_token) {
        (Some(token), _) => {
            let token = token.trim();
            if !(8..=200).contains(&token.len())
                || !token.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(invalid(
                    CODE,
                    "the Fio token must be 8-200 letters or digits",
                ));
            }
            let secrets = secrets.ok_or_else(|| {
                Error::Unavailable("PAYMENTS_SECRET_KEY is not configured".into())
            })?;
            let sealed = secrets
                .seal(token.as_bytes(), &token_aad(tx.tenant_id(), id))
                .map_err(|e| Error::Internal(e.to_string()))?;
            sqlx::query!(
                "UPDATE bank_accounts SET fio_token = $2, fio_synced_at = NULL WHERE id = $1",
                id,
                sealed
            )
            .execute(&mut **tx)
            .await?;
            "set"
        }
        (None, true) => {
            sqlx::query!(
                "UPDATE bank_accounts SET fio_token = NULL, fio_synced_at = NULL WHERE id = $1",
                id
            )
            .execute(&mut **tx)
            .await?;
            "cleared"
        }
        (None, false) => "kept",
    };
    audit::record(
        tx,
        actor,
        "bank_account.configured",
        "bank_account",
        Some(&id.to_string()),
        &json!({ "market_id": market_id, "iban": iban, "bic": bic, "account_name": name,
                 "fio_token": token_change, "retired": retired }),
    )
    .await?;
    account_by_id(tx, id).await
}

// ---------------------------------------------------------------------------------------
// Instructions (fixed at placement)

/// What the customer needs to pay by bank transfer; stored on the attempt at placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Instructions {
    pub iban: String,
    pub bic: Option<String>,
    pub account_name: String,
    pub variable_symbol: String,
    pub amount_minor: i64,
    pub currency: String,
    pub message: String,
    pub qr_kind: Option<QrKind>,
    pub qr_payload: Option<String>,
}

/// The bank details of a new bank-transfer attempt.
#[derive(Debug, Clone)]
pub(crate) struct Details {
    pub account_id: Uuid,
    pub variable_symbol: String,
    pub instructions: Value,
}

/// Instructions for paying order `number` of `amount_minor` into the market's account.
/// `409 bank_account_missing` without a receiving account.
pub(crate) async fn prepare(
    tx: &mut TenantTx,
    market_id: Uuid,
    number: i64,
    amount_minor: i64,
    currency: &str,
    shop_name: &str,
) -> Result<Details, Error> {
    let acc = account(tx, market_id)
        .await?
        .filter(|a| a.currency == currency)
        .ok_or_else(|| Error::Conflict {
            code: "bank_account_missing",
            detail: "bank transfer has no receiving account for this market".into(),
        })?;
    let vs = number.to_string();
    if vs.len() > 10 {
        return Err(Error::Internal(
            "order numbers exceed the variable symbol".into(),
        ));
    }
    let message: String = format!("{shop_name} {vs}").chars().take(60).collect();
    let qr = qr::payload(&Transfer {
        iban: &acc.iban,
        bic: acc.bic.as_deref(),
        amount_minor,
        currency,
        variable_symbol: &vs,
        message: &message,
        beneficiary: &acc.account_name,
    })?;
    let instructions = Instructions {
        iban: acc.iban,
        bic: acc.bic,
        account_name: acc.account_name,
        variable_symbol: vs.clone(),
        amount_minor,
        currency: currency.to_owned(),
        message,
        qr_kind: qr.as_ref().map(|(k, _)| *k),
        qr_payload: qr.map(|(_, p)| p),
    };
    Ok(Details {
        account_id: acc.id,
        variable_symbol: vs,
        instructions: serde_json::to_value(&instructions)
            .map_err(|e| Error::Internal(e.to_string()))?,
    })
}

/// The payment instructions of a bank-transfer order, as the customer sees them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BankTransferView {
    pub iban: String,
    pub bic: Option<String>,
    pub account_name: String,
    pub variable_symbol: String,
    pub amount: MoneyView,
    pub message: String,
    /// `spayd` (CZK) or `pay_by_square` (EUR); `null` when the currency has no QR standard.
    pub qr_kind: Option<QrKind>,
    /// The QR code as an inline SVG element (server-generated; contains no script).
    pub qr_svg: Option<String>,
}

/// The instructions of an attempt for display, the QR rendered as SVG.
pub(crate) fn view(instructions: &Value, locale: Locale) -> Result<BankTransferView, Error> {
    let i: Instructions = serde_json::from_value(instructions.clone())
        .map_err(|e| Error::Internal(format!("stored instructions: {e}")))?;
    let currency = Currency::parse(&i.currency)
        .ok_or_else(|| Error::Internal(format!("stored currency {}", i.currency)))?;
    let label = match i.qr_kind {
        Some(QrKind::Spayd) => "QR platba",
        _ => "PAY by square",
    };
    Ok(BankTransferView {
        qr_svg: i
            .qr_payload
            .as_deref()
            .map(|p| qr::svg(p, label))
            .transpose()?,
        amount: Money::new(i.amount_minor, currency).view(locale),
        iban: i.iban,
        bic: i.bic,
        account_name: i.account_name,
        variable_symbol: i.variable_symbol,
        message: i.message,
        qr_kind: i.qr_kind,
    })
}

// ---------------------------------------------------------------------------------------
// Statement import and matching

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    Matched,
    Unmatched,
    Partial,
    Overpaid,
    Dismissed,
}

impl TxStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Unmatched => "unmatched",
            Self::Partial => "partial",
            Self::Overpaid => "overpaid",
            Self::Dismissed => "dismissed",
        }
    }

    fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "matched" => Self::Matched,
            "unmatched" => Self::Unmatched,
            "partial" => Self::Partial,
            "overpaid" => Self::Overpaid,
            "dismissed" => Self::Dismissed,
            other => return Err(Error::Internal(format!("stored status {other}"))),
        })
    }

    /// Waiting for a person.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Unmatched | Self::Partial | Self::Overpaid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TxReason {
    NoVariableSymbol,
    UnknownVariableSymbol,
    CurrencyMismatch,
    /// The order was paid already (another transfer or method): the money goes back.
    AlreadyPaid,
    AmountShort,
    AmountOver,
    /// Resolved by a person.
    Manual,
}

impl TxReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoVariableSymbol => "no_variable_symbol",
            Self::UnknownVariableSymbol => "unknown_variable_symbol",
            Self::CurrencyMismatch => "currency_mismatch",
            Self::AlreadyPaid => "already_paid",
            Self::AmountShort => "amount_short",
            Self::AmountOver => "amount_over",
            Self::Manual => "manual",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "no_variable_symbol" => Self::NoVariableSymbol,
            "unknown_variable_symbol" => Self::UnknownVariableSymbol,
            "currency_mismatch" => Self::CurrencyMismatch,
            "already_paid" => Self::AlreadyPaid,
            "amount_short" => Self::AmountShort,
            "amount_over" => Self::AmountOver,
            "manual" => Self::Manual,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BankTransaction {
    pub id: Uuid,
    pub bank_account_id: Uuid,
    pub bank_tx_id: String,
    pub booked_on: NaiveDate,
    pub amount_minor: i64,
    pub currency: String,
    pub variable_symbol: Option<String>,
    pub counterparty: Option<String>,
    pub counterparty_name: Option<String>,
    pub message: Option<String>,
    pub source: String,
    pub status: TxStatus,
    pub reason: Option<TxReason>,
    /// The order the money went to (matched, or the candidate of a partial/over payment).
    pub order_id: Option<Uuid>,
    pub order_number: Option<String>,
    /// What the order expected (for partial and over payments).
    pub expected_minor: Option<i64>,
    pub note: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct StatementImport {
    /// New credit lines stored.
    pub imported: u32,
    /// Lines already imported earlier (same bank transaction id): ignored.
    pub duplicates: u32,
    /// Debits (outgoing payments) are not stored.
    pub debits: u32,
    /// New lines that paid an order.
    pub matched: u32,
    /// New lines waiting in the exceptions queue.
    pub exceptions: u32,
}

/// Imports a parsed statement into `account_id` and matches the new lines (A25). The file must
/// be for this account when it names one (`422 statement_account_mismatch`).
pub async fn import(
    tx: &mut TenantTx,
    actor: &str,
    account_id: Uuid,
    source: &str,
    statement: &Statement,
) -> Result<StatementImport, Error> {
    let acc = account_by_id(tx, account_id).await?;
    let mismatch = || Error::Validation {
        code: "statement_account_mismatch",
        detail: "the statement is for another account".into(),
    };
    if statement
        .iban
        .as_deref()
        .is_some_and(|i| normalize_iban(i).as_deref() != Some(acc.iban.as_str()))
    {
        return Err(mismatch());
    }
    if let Some(domestic) = &statement.domestic
        && statements::cz_domestic(&acc.iban) != Some(domestic.as_str())
    {
        return Err(mismatch());
    }
    let mut report = StatementImport::default();
    for line in &statement.lines {
        if line.amount_minor <= 0 {
            report.debits += 1;
            continue;
        }
        match insert_line(tx, account_id, source, line).await? {
            None => report.duplicates += 1,
            Some(id) => {
                report.imported += 1;
                if match_line(tx, id, actor).await? == TxStatus::Matched {
                    report.matched += 1;
                } else {
                    report.exceptions += 1;
                }
            }
        }
    }
    audit::record(
        tx,
        actor,
        "bank_statement.imported",
        "bank_account",
        Some(&account_id.to_string()),
        &json!({ "source": source, "report": report }),
    )
    .await?;
    Ok(report)
}

async fn insert_line(
    tx: &mut TenantTx,
    account_id: Uuid,
    source: &str,
    l: &StatementLine,
) -> Result<Option<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO bank_transactions (id, tenant_id, bank_account_id, bank_tx_id, booked_on,
             amount_minor, currency, variable_symbol, counterparty, counterparty_name, message,
             source, raw, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'unmatched')
         ON CONFLICT (tenant_id, bank_account_id, bank_tx_id) DO NOTHING
         RETURNING id",
        crate::id::new_id(),
        tx.tenant_id(),
        account_id,
        l.bank_tx_id,
        l.booked_on,
        l.amount_minor,
        l.currency,
        l.variable_symbol,
        l.counterparty,
        l.counterparty_name,
        l.message,
        source,
        l.raw
    )
    .fetch_optional(&mut **tx)
    .await?)
}

/// Matches one stored line (A25) and records the result on it.
async fn match_line(tx: &mut TenantTx, id: Uuid, actor: &str) -> Result<TxStatus, Error> {
    let line = sqlx::query!(
        "SELECT bank_account_id, amount_minor, currency, variable_symbol
         FROM bank_transactions WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    let candidate = match &line.variable_symbol {
        None => None,
        Some(vs) => {
            sqlx::query!(
                "SELECT id, order_id FROM payment_attempts
             WHERE bank_account_id = $1 AND variable_symbol = $2",
                line.bank_account_id,
                vs
            )
            .fetch_optional(&mut **tx)
            .await?
        }
    };
    // Attempts change only under their order's lock: taken before reading the attempt's state,
    // so of two concurrent transfers for one order exactly one pays it (the other is
    // `already_paid`).
    let attempt = match candidate {
        Some(c) => {
            crate::orders::lock(tx, c.order_id).await?;
            sqlx::query!(
                "SELECT id, status, amount_minor, currency FROM payment_attempts WHERE id = $1",
                c.id
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        None => None,
    };
    let (status, reason, attempt_id) = match (&line.variable_symbol, attempt) {
        (None, _) => (TxStatus::Unmatched, Some(TxReason::NoVariableSymbol), None),
        (Some(_), None) => (
            TxStatus::Unmatched,
            Some(TxReason::UnknownVariableSymbol),
            None,
        ),
        (Some(_), Some(a)) if a.currency != line.currency => (
            TxStatus::Unmatched,
            Some(TxReason::CurrencyMismatch),
            Some(a.id),
        ),
        (Some(_), Some(a)) if a.status == "succeeded" => {
            (TxStatus::Unmatched, Some(TxReason::AlreadyPaid), Some(a.id))
        }
        (Some(_), Some(a)) if line.amount_minor < a.amount_minor => {
            (TxStatus::Partial, Some(TxReason::AmountShort), Some(a.id))
        }
        (Some(_), Some(a)) if line.amount_minor > a.amount_minor => {
            (TxStatus::Overpaid, Some(TxReason::AmountOver), Some(a.id))
        }
        (Some(_), Some(a)) => {
            super::apply_outcome(tx, a.id, Outcome::Succeeded, actor).await?;
            (TxStatus::Matched, None, Some(a.id))
        }
    };
    sqlx::query!(
        "UPDATE bank_transactions SET status = $2, reason = $3, attempt_id = $4 WHERE id = $1",
        id,
        status.as_str(),
        reason.map(TxReason::as_str),
        attempt_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(status)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolveAction {
    /// A partial or over payment counts as the order's payment (the difference is settled
    /// outside the platform, e.g. refunded or collected later).
    Accept,
    /// The money belongs to this order (a wrong or missing variable symbol).
    Assign,
    /// Not a payment for an order, or returned to the sender; closes the line.
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveInput {
    pub action: ResolveAction,
    /// The order number, for `assign`.
    #[serde(default)]
    pub order_number: Option<String>,
    /// Why (required for `dismiss`), at most 500 characters.
    #[serde(default)]
    pub note: Option<String>,
}

/// A person resolves an open line (audited).
pub async fn resolve(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &ResolveInput,
) -> Result<BankTransaction, Error> {
    const CODE: &str = "invalid_resolution";
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    if note.as_ref().is_some_and(|n| n.chars().count() > 500) {
        return Err(invalid(CODE, "the note is at most 500 characters"));
    }
    let line = sqlx::query!(
        "SELECT status, attempt_id, currency, bank_account_id, amount_minor
         FROM bank_transactions WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let status = TxStatus::parse(&line.status)?;
    if !status.is_open() {
        return Err(Error::Conflict {
            code: "transaction_resolved",
            detail: "this bank transaction is already resolved".into(),
        });
    }
    let attempt_id = match input.action {
        ResolveAction::Accept => {
            if !matches!(status, TxStatus::Partial | TxStatus::Overpaid) {
                return Err(invalid(
                    CODE,
                    "only partial or over payments can be accepted",
                ));
            }
            let a = line
                .attempt_id
                .ok_or_else(|| Error::Internal("partial payment without attempt".into()))?;
            super::apply_outcome(tx, a, Outcome::Succeeded, actor).await?;
            Some(a)
        }
        ResolveAction::Assign => {
            let number: i64 = input
                .order_number
                .as_deref()
                .map(str::trim)
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| invalid(CODE, "assign needs the order number"))?;
            let a = sqlx::query!(
                "SELECT a.id, a.order_id, a.currency, a.bank_account_id, a.amount_minor
                 FROM payment_attempts a JOIN orders o ON o.id = a.order_id
                 WHERE o.number = $1 AND a.method = 'bank_transfer'
                 ORDER BY a.created_at DESC LIMIT 1",
                number
            )
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| invalid(CODE, "no bank-transfer order with this number"))?;
            // A25: money received on one account never pays an order of another.
            if a.bank_account_id != Some(line.bank_account_id) {
                return Err(invalid(
                    CODE,
                    "the order was to be paid to another receiving account",
                ));
            }
            if a.currency != line.currency {
                return Err(invalid(CODE, "the order is in another currency"));
            }
            crate::orders::lock(tx, a.order_id).await?;
            if line.amount_minor != a.amount_minor {
                // A different amount becomes a partial/over payment of that order: accepting
                // it is a separate, explicit decision.
                let (status, reason) = if line.amount_minor < a.amount_minor {
                    (TxStatus::Partial, TxReason::AmountShort)
                } else {
                    (TxStatus::Overpaid, TxReason::AmountOver)
                };
                sqlx::query!(
                    "UPDATE bank_transactions SET status = $2, reason = $3, attempt_id = $4,
                         note = $5
                     WHERE id = $1",
                    id,
                    status.as_str(),
                    reason.as_str(),
                    a.id,
                    note
                )
                .execute(&mut **tx)
                .await?;
                audit::record(
                    tx,
                    actor,
                    "bank_transaction.assigned",
                    "bank_transaction",
                    Some(&id.to_string()),
                    &json!({ "attempt_id": a.id, "status": status, "note": note }),
                )
                .await?;
                return transaction(tx, id).await;
            }
            super::apply_outcome(tx, a.id, Outcome::Succeeded, actor).await?;
            Some(a.id)
        }
        ResolveAction::Dismiss => {
            if note.is_none() {
                return Err(invalid(CODE, "dismissing needs a note"));
            }
            line.attempt_id
        }
    };
    let next = match input.action {
        ResolveAction::Dismiss => TxStatus::Dismissed,
        _ => TxStatus::Matched,
    };
    sqlx::query!(
        "UPDATE bank_transactions SET status = $2, reason = 'manual', attempt_id = $3, note = $4,
             resolved_by = $5, resolved_at = now()
         WHERE id = $1",
        id,
        next.as_str(),
        attempt_id,
        note,
        actor
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "bank_transaction.resolved",
        "bank_transaction",
        Some(&id.to_string()),
        &json!({ "action": input.action, "from": status, "attempt_id": attempt_id, "note": note }),
    )
    .await?;
    transaction(tx, id).await
}

struct TxRow {
    id: Uuid,
    bank_account_id: Uuid,
    bank_tx_id: String,
    booked_on: NaiveDate,
    amount_minor: i64,
    currency: String,
    variable_symbol: Option<String>,
    counterparty: Option<String>,
    counterparty_name: Option<String>,
    message: Option<String>,
    source: String,
    status: String,
    reason: Option<String>,
    note: Option<String>,
    resolved_by: Option<String>,
    resolved_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    order_id: Option<Uuid>,
    number: Option<i64>,
    expected_minor: Option<i64>,
}

impl TryFrom<TxRow> for BankTransaction {
    type Error = Error;

    fn try_from(r: TxRow) -> Result<Self, Error> {
        Ok(Self {
            id: r.id,
            bank_account_id: r.bank_account_id,
            bank_tx_id: r.bank_tx_id,
            booked_on: r.booked_on,
            amount_minor: r.amount_minor,
            currency: r.currency,
            variable_symbol: r.variable_symbol,
            counterparty: r.counterparty,
            counterparty_name: r.counterparty_name,
            message: r.message,
            source: r.source,
            status: TxStatus::parse(&r.status)?,
            reason: r.reason.as_deref().and_then(TxReason::parse),
            order_id: r.order_id,
            order_number: r.number.map(|n| n.to_string()),
            expected_minor: r.expected_minor,
            note: r.note,
            resolved_by: r.resolved_by,
            resolved_at: r.resolved_at,
            created_at: r.created_at,
        })
    }
}

pub async fn transaction(tx: &mut TenantTx, id: Uuid) -> Result<BankTransaction, Error> {
    sqlx::query_as!(
        TxRow,
        "SELECT t.id, t.bank_account_id, t.bank_tx_id, t.booked_on, t.amount_minor, t.currency,
                t.variable_symbol, t.counterparty, t.counterparty_name, t.message, t.source,
                t.status, t.reason, t.note, t.resolved_by, t.resolved_at, t.created_at,
                a.order_id AS \"order_id?\", o.number AS \"number?\",
                a.amount_minor AS \"expected_minor?\"
         FROM bank_transactions t
         LEFT JOIN payment_attempts a ON a.id = t.attempt_id
         LEFT JOIN orders o ON o.id = a.order_id
         WHERE t.id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

#[derive(Debug, Clone, Default)]
pub struct TxFilter {
    pub status: Option<TxStatus>,
    /// Only lines waiting for a person.
    pub open: bool,
    pub bank_account_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BankTransactionPage {
    pub items: Vec<BankTransaction>,
    /// Pass as `cursor` for the next page; `null` on the last page.
    pub next_cursor: Option<Uuid>,
}

/// Imported lines, newest first (keyset by UUIDv7 id).
pub async fn transactions(
    tx: &mut TenantTx,
    filter: &TxFilter,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<BankTransactionPage, Error> {
    let limit = limit.clamp(1, 100);
    let rows = sqlx::query_as!(
        TxRow,
        "SELECT t.id, t.bank_account_id, t.bank_tx_id, t.booked_on, t.amount_minor, t.currency,
                t.variable_symbol, t.counterparty, t.counterparty_name, t.message, t.source,
                t.status, t.reason, t.note, t.resolved_by, t.resolved_at, t.created_at,
                a.order_id AS \"order_id?\", o.number AS \"number?\",
                a.amount_minor AS \"expected_minor?\"
         FROM bank_transactions t
         LEFT JOIN payment_attempts a ON a.id = t.attempt_id
         LEFT JOIN orders o ON o.id = a.order_id
         WHERE ($1::text IS NULL OR t.status = $1)
           AND (NOT $2 OR t.status IN ('unmatched', 'partial', 'overpaid'))
           AND ($3::uuid IS NULL OR t.bank_account_id = $3)
           AND ($4::uuid IS NULL OR t.id < $4)
         ORDER BY t.id DESC LIMIT $5",
        filter.status.map(TxStatus::as_str),
        filter.open,
        filter.bank_account_id,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let take = usize::try_from(limit).unwrap_or(100);
    let more = rows.len() > take;
    let items = rows
        .into_iter()
        .take(take)
        .map(BankTransaction::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BankTransactionPage {
        next_cursor: if more {
            items.last().map(|t| t.id)
        } else {
            None
        },
        items,
    })
}

// ---------------------------------------------------------------------------------------
// Payment reminders (spec §10.3: day 3 and day 6 of the bank-transfer window)

/// A reminder that is due: the attempt and which one (1 or 2).
pub struct DueReminder {
    pub tenant_id: Uuid,
    pub attempt_id: Uuid,
}

pub async fn due_reminders(db: &sqlx::PgPool, max: i32) -> Result<Vec<DueReminder>, Error> {
    Ok(sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", attempt_id AS "attempt_id!"
           FROM platform.due_payment_reminders($1)"#,
        max
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| DueReminder {
        tenant_id: r.tenant_id,
        attempt_id: r.attempt_id,
    })
    .collect())
}

/// Claims the next reminder of a pending bank-transfer attempt under the order lock:
/// `Some(n)` (1 or 2) when one is due now and was not sent yet, else `None`. The caller sends
/// the email in the same transaction.
pub(crate) async fn claim_reminder(
    tx: &mut TenantTx,
    attempt_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<(Uuid, i16)>, Error> {
    let order_id = super::attempt(tx, attempt_id).await?.order_id;
    let order = crate::orders::lock(tx, order_id).await?;
    let a = sqlx::query!(
        "SELECT status, method, reminders_sent, created_at, expires_at FROM payment_attempts
         WHERE id = $1",
        attempt_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let n = a.reminders_sent + 1;
    let due_after = match n {
        1 => Duration::days(3),
        2 => Duration::days(6),
        _ => return Ok(None),
    };
    let due = a.method == MethodKind::BankTransfer.as_str()
        && AttemptStatus::parse(&a.status) == AttemptStatus::Pending
        && order.status == "pending"
        && a.created_at + due_after <= now
        && a.expires_at.is_none_or(|e| e > now);
    if !due {
        return Ok(None);
    }
    sqlx::query!(
        "UPDATE payment_attempts SET reminders_sent = $2, updated_at = now() WHERE id = $1",
        attempt_id,
        n
    )
    .execute(&mut **tx)
    .await?;
    crate::orders::event(
        tx,
        order_id,
        "payment_reminder_sent",
        &json!({ "attempt_id": attempt_id, "reminder": n }),
        "system",
    )
    .await?;
    Ok(Some((order_id, n)))
}

// ---------------------------------------------------------------------------------------
// Fio API polling

/// An account to poll.
pub struct FioAccount {
    pub tenant_id: Uuid,
    pub bank_account_id: Uuid,
}

pub async fn fio_accounts(db: &sqlx::PgPool) -> Result<Vec<FioAccount>, Error> {
    Ok(sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", bank_account_id AS "bank_account_id!"
           FROM platform.fio_bank_accounts()"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| FioAccount {
        tenant_id: r.tenant_id,
        bank_account_id: r.bank_account_id,
    })
    .collect())
}

/// The window overlaps the previous poll: transaction ids make the overlap harmless, and a
/// crash between download and commit loses nothing.
const FIO_OVERLAP_DAYS: i64 = 3;
const FIO_FIRST_DAYS: i64 = 30;
/// A statement answer is small; anything bigger is refused before it is buffered.
const FIO_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Downloads the account's recent transactions from the Fio API and imports them. The token
/// is decrypted only for the request and never logged (it is part of the URL, so request
/// errors are reported without it).
pub async fn poll_fio(
    db: &sqlx::PgPool,
    http: &reqwest::Client,
    secrets: &SecretBox,
    base_url: &str,
    acc: &FioAccount,
    now: DateTime<Utc>,
) -> Result<StatementImport, Error> {
    let mut tx = platform::db::tenant_tx(db, acc.tenant_id).await?;
    let row = sqlx::query!(
        "SELECT fio_token, fio_synced_at FROM bank_accounts WHERE id = $1",
        acc.bank_account_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    tx.commit().await?;
    let sealed = row.fio_token.ok_or(Error::NotFound)?;
    let token = secrets
        .open(&sealed, &token_aad(acc.tenant_id, acc.bank_account_id))
        .map_err(|e| Error::Internal(format!("fio token: {e}")))?;
    let token = String::from_utf8(token).map_err(|_| Error::Internal("fio token".into()))?;
    let from = row
        .fio_synced_at
        .map_or(now - Duration::days(FIO_FIRST_DAYS), |s| {
            s - Duration::days(FIO_OVERLAP_DAYS)
        })
        .date_naive();
    let url = format!(
        "{}/v1/rest/periods/{token}/{from}/{}/transactions.json",
        base_url.trim_end_matches('/'),
        now.date_naive()
    );
    let res = http
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| Error::Unavailable(format!("fio api: {}", e.without_url())))?;
    if !res.status().is_success() {
        return Err(Error::Unavailable(format!(
            "fio api: HTTP {}",
            res.status()
        )));
    }
    let mut res = res;
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| Error::Unavailable(format!("fio api: {}", e.without_url())))?
    {
        if body.len() + chunk.len() > FIO_MAX_BYTES {
            return Err(Error::Unavailable(format!(
                "fio api: response over {FIO_MAX_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let statement = statements::fio_json(&body)?;
    let mut tx = platform::db::tenant_tx(db, acc.tenant_id).await?;
    let report = import(
        &mut tx,
        "fio_api",
        acc.bank_account_id,
        "fio_api",
        &statement,
    )
    .await?;
    sqlx::query!(
        "UPDATE bank_accounts SET fio_synced_at = $2 WHERE id = $1",
        acc.bank_account_id,
        now
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iban_check_digits() {
        assert_eq!(
            normalize_iban("cz65 0800 0000 1920 0014 5399").as_deref(),
            Some("CZ6508000000192000145399")
        );
        assert_eq!(
            normalize_iban("SK9611000000002918599669").as_deref(),
            Some("SK9611000000002918599669")
        );
        assert!(
            normalize_iban("CZ6508000000192000145398").is_none(),
            "check digits"
        );
        assert!(normalize_iban("CZ65").is_none());
        assert!(normalize_iban("CZ65-0800-0000-1920-0014-5399").is_none());
    }

    #[test]
    fn bic_shape() {
        assert_eq!(normalize_bic("gibaczpx").as_deref(), Some("GIBACZPX"));
        assert_eq!(normalize_bic("TATRSKBXXXX").as_deref(), Some("TATRSKBXXXX"));
        assert!(normalize_bic("GIBA").is_none());
        assert!(normalize_bic("12BACZPX").is_none());
    }
}
