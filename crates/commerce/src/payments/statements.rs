//! Bank statement parsers (spec §10.4, A25): ISO 20022 **camt.053** XML, **Fio CSV**
//! (the bank's export / API CSV), **GPC/ABO** (the Czech fixed-width format) and the **Fio
//! API** JSON. Pure functions: they turn a file into [`Statement`] lines and never touch the
//! database. Only booked credits matter for matching; debits are reported so the import can
//! count them.
//!
//! Every line needs the bank's own transaction id: it is what makes a re-import (or an
//! overlapping API window) a no-op (A25). A statement without ids is refused.

use std::borrow::Cow;

use chrono::NaiveDate;
use platform::Error;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// ISO 20022 bank-to-customer statement (`camt.053.001.02` … `.08`).
    Camt053,
    /// Fio banka CSV (web export or `transactions.csv` from the API), `;`-separated.
    FioCsv,
    /// GPC (ABO) fixed-width statement of Czech banks.
    Gpc,
}

impl Format {
    pub fn source(self) -> &'static str {
        match self {
            Self::Camt053 => "camt053",
            Self::FioCsv => "fio_csv",
            Self::Gpc => "gpc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementLine {
    pub bank_tx_id: String,
    pub booked_on: NaiveDate,
    /// Positive for credits, negative for debits, in minor units (two decimals).
    pub amount_minor: i64,
    pub currency: String,
    /// Digits only, leading zeros removed.
    pub variable_symbol: Option<String>,
    pub counterparty: Option<String>,
    pub counterparty_name: Option<String>,
    pub message: Option<String>,
    /// The fields as read, kept for the audit trail.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Statement {
    /// The statement's own account when the format names it (IBAN, or the domestic
    /// `prefix-number` for GPC), to refuse a file uploaded to the wrong account.
    pub iban: Option<String>,
    /// Czech domestic account number (GPC): 16 digits, prefix + number, zero padded.
    pub domestic: Option<String>,
    pub lines: Vec<StatementLine>,
}

/// At most this many lines per file (the upload is capped at 1 MB anyway).
pub const MAX_LINES: usize = 10_000;

fn bad(detail: impl Into<String>) -> Error {
    Error::Validation {
        code: "invalid_statement",
        detail: detail.into(),
    }
}

pub fn parse(format: Format, bytes: &[u8]) -> Result<Statement, Error> {
    let statement = match format {
        Format::Camt053 => camt053(bytes)?,
        Format::FioCsv => fio_csv(bytes)?,
        Format::Gpc => gpc(bytes)?,
    };
    if statement.lines.len() > MAX_LINES {
        return Err(bad(format!("at most {MAX_LINES} lines per statement")));
    }
    Ok(statement)
}

// ---------------------------------------------------------------------------------------
// Shared helpers

/// `1 500,00`, `1500.5`, `-20,00` → minor units; more than two decimals is refused.
pub fn parse_amount(s: &str) -> Result<i64, Error> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    let (neg, digits) = match cleaned.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, cleaned.strip_prefix('+').unwrap_or(&cleaned)),
    };
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let valid = !whole.is_empty()
        && whole.len() <= 13
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.len() <= 2
        && frac.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return Err(bad(format!("invalid amount {s:?}")));
    }
    let whole: i64 = whole
        .parse()
        .map_err(|_| bad(format!("invalid amount {s:?}")))?;
    let frac: i64 = format!("{frac:0<2}")
        .parse()
        .map_err(|_| bad(format!("invalid amount {s:?}")))?;
    let minor = whole * 100 + frac;
    Ok(if neg { -minor } else { minor })
}

/// A variable symbol from a free-form reference: `100001`, `VS100001`, `VS:0100001`,
/// `/VS/100001`. Digits only (at most 10), leading zeros removed; `None` when absent or zero.
pub fn variable_symbol(s: &str) -> Option<String> {
    let t = s.trim();
    let digits = if !t.is_empty() && t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()) {
        t
    } else {
        let upper = t.to_ascii_uppercase();
        let at = upper.find("VS")?;
        let rest = &t[at + 2..];
        let rest = rest.trim_start_matches([':', '/', ' ', '.', '-', '=']);
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 || end > 10 {
            return None;
        }
        &rest[..end]
    };
    let v = digits.trim_start_matches('0');
    (!v.is_empty()).then(|| v.to_owned())
}

fn parse_date(s: &str) -> Result<NaiveDate, Error> {
    let t = s.trim();
    let iso = t.get(..10).unwrap_or(t);
    NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(t, "%d.%m.%Y"))
        .map_err(|_| bad(format!("invalid date {s:?}")))
}

fn non_empty(s: &str, max: usize) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.chars().take(max).collect())
}

// ---------------------------------------------------------------------------------------
// camt.053

/// A tiny element tree: camt.053 is small and read by path, which a streaming parser makes
/// needlessly fiddly. No DTDs or external entities are processed (quick-xml does not).
#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |n, p| n.child(p))
    }

    fn text_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path)
            .map(|n| n.text.trim())
            .filter(|t| !t.is_empty())
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

const MAX_DEPTH: usize = 64;

fn xml_tree(bytes: &[u8]) -> Result<Node, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| bad("camt.053 must be UTF-8"))?;
    let mut reader = quick_xml::Reader::from_str(text);
    let xml = |e: &dyn std::fmt::Display| bad(format!("invalid XML: {e}"));
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| xml(&e))? {
            Event::Start(_) | Event::Empty(_) if stack.len() > MAX_DEPTH => {
                return Err(bad(format!("XML nested deeper than {MAX_DEPTH}")));
            }
            Event::Start(e) => stack.push(element(&e)?),
            Event::Empty(e) => {
                let node = element(&e)?;
                last(&mut stack)?.children.push(node);
            }
            Event::End(_) => {
                let node = stack.pop().ok_or_else(|| bad("unbalanced XML"))?;
                last(&mut stack)?.children.push(node);
            }
            Event::Text(t) => last(&mut stack)?
                .text
                .push_str(&t.decode().map_err(|e| xml(&e))?),
            Event::CData(t) => last(&mut stack)?
                .text
                .push_str(&t.decode().map_err(|e| xml(&e))?),
            Event::GeneralRef(r) => {
                let c: Cow<'_, str> = match r.resolve_char_ref().map_err(|e| xml(&e))? {
                    Some(c) => c.to_string().into(),
                    None => match r.decode().map_err(|e| xml(&e))?.as_ref() {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => return Err(bad(format!("unknown XML entity &{other};"))),
                    },
                };
                last(&mut stack)?.text.push_str(&c);
            }
            Event::DocType(_) => return Err(bad("DOCTYPE is not allowed")),
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(bad("unbalanced XML"));
    }
    stack.pop().ok_or_else(|| bad("empty XML"))
}

fn last(stack: &mut [Node]) -> Result<&mut Node, Error> {
    stack.last_mut().ok_or_else(|| bad("unbalanced XML"))
}

fn element(e: &quick_xml::events::BytesStart<'_>) -> Result<Node, Error> {
    let mut node = Node {
        name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
        ..Node::default()
    };
    for a in e.attributes() {
        let a = a.map_err(|e| bad(format!("invalid XML attribute: {e}")))?;
        node.attrs.push((
            String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned(),
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|e| bad(format!("invalid XML attribute: {e}")))?
                .into_owned(),
        ));
    }
    Ok(node)
}

fn camt053(bytes: &[u8]) -> Result<Statement, Error> {
    let root = xml_tree(bytes)?;
    let stmts = root
        .at(&["Document", "BkToCstmrStmt"])
        .ok_or_else(|| bad("not a camt.053 document (Document/BkToCstmrStmt)"))?;
    let mut out = Statement::default();
    for stmt in stmts.children("Stmt") {
        let iban = stmt.text_at(&["Acct", "Id", "IBAN"]).map(str::to_owned);
        if out.iban.is_some() && out.iban != iban {
            return Err(bad("the statements are for different accounts"));
        }
        out.iban = iban;
        let account_ccy = stmt.text_at(&["Acct", "Ccy"]);
        for entry in stmt.children("Ntry") {
            // Only booked entries: `<Sts>BOOK</Sts>` (v02) or `<Sts><Cd>BOOK</Cd></Sts>` (v08).
            let status = entry
                .text_at(&["Sts", "Cd"])
                .or_else(|| entry.text_at(&["Sts"]));
            if status.is_some_and(|s| s != "BOOK") || entry.text_at(&["RvslInd"]) == Some("true") {
                continue;
            }
            let sign = match entry.text_at(&["CdtDbtInd"]) {
                Some("CRDT") => 1,
                Some("DBIT") => -1,
                other => return Err(bad(format!("unknown CdtDbtInd {other:?}"))),
            };
            let booked = entry
                .text_at(&["BookgDt", "Dt"])
                .or_else(|| entry.text_at(&["BookgDt", "DtTm"]))
                .or_else(|| entry.text_at(&["ValDt", "Dt"]))
                .ok_or_else(|| bad("an entry has no booking date"))?;
            let booked_on = parse_date(booked)?;
            let entry_id = entry
                .text_at(&["AcctSvcrRef"])
                .or_else(|| entry.text_at(&["NtryRef"]));
            let details: Vec<&Node> = entry
                .children("NtryDtls")
                .flat_map(|d| d.children("TxDtls"))
                .collect();
            let empty = Node::default();
            let parts: Vec<&Node> = if details.len() > 1 {
                details
            } else {
                vec![details.first().copied().unwrap_or(&empty)]
            };
            let batch = parts.len() > 1;
            for (i, tx) in parts.iter().enumerate() {
                let amount_node = if batch {
                    tx.at(&["Amt"])
                        .or_else(|| tx.at(&["AmtDtls", "TxAmt", "Amt"]))
                        .ok_or_else(|| bad("a batch entry has no transaction amount"))?
                } else {
                    entry
                        .at(&["Amt"])
                        .ok_or_else(|| bad("an entry has no amount"))?
                };
                let currency = amount_node
                    .attr("Ccy")
                    .or(account_ccy)
                    .ok_or_else(|| bad("an amount has no currency"))?;
                let tx_id = tx.text_at(&["Refs", "AcctSvcrRef"]);
                let bank_tx_id = match (batch, tx_id, entry_id) {
                    (true, Some(id), _) => id.to_owned(),
                    (true, None, Some(e)) => format!("{e}/{}", i + 1),
                    (false, _, Some(e)) => e.to_owned(),
                    (false, Some(id), None) => id.to_owned(),
                    _ => return Err(bad("an entry has no bank reference (AcctSvcrRef)")),
                };
                let reference = tx
                    .children("RmtInf")
                    .flat_map(|r| r.children("Strd"))
                    .find_map(|s| s.text_at(&["CdtrRefInf", "Ref"]));
                let unstructured: Vec<&str> = tx
                    .children("RmtInf")
                    .flat_map(|r| r.children("Ustrd"))
                    .map(|u| u.text.trim())
                    .filter(|u| !u.is_empty())
                    .collect();
                let vs = reference
                    .and_then(variable_symbol)
                    .or_else(|| {
                        tx.text_at(&["Refs", "EndToEndId"])
                            .and_then(variable_symbol)
                    })
                    .or_else(|| {
                        unstructured.iter().find_map(|u| {
                            u.to_ascii_uppercase()
                                .contains("VS")
                                .then(|| variable_symbol(u))
                                .flatten()
                        })
                    });
                let party = if sign > 0 { "Dbtr" } else { "Cdtr" };
                let counterparty = tx
                    .text_at(&["RltdPties", &format!("{party}Acct"), "Id", "IBAN"])
                    .or_else(|| {
                        tx.text_at(&["RltdPties", &format!("{party}Acct"), "Id", "Othr", "Id"])
                    });
                let counterparty_name = tx
                    .text_at(&["RltdPties", party, "Nm"])
                    .or_else(|| tx.text_at(&["RltdPties", party, "Pty", "Nm"]));
                let amount = parse_amount(&amount_node.text)?;
                out.lines.push(StatementLine {
                    bank_tx_id: bank_tx_id.chars().take(100).collect(),
                    booked_on,
                    amount_minor: sign * amount,
                    currency: currency.to_owned(),
                    variable_symbol: vs,
                    counterparty: counterparty.and_then(|c| non_empty(c, 100)),
                    counterparty_name: counterparty_name.and_then(|c| non_empty(c, 200)),
                    message: non_empty(&unstructured.join(" "), 500),
                    raw: json!({
                        "entry_ref": entry_id, "tx_ref": tx_id, "amount": amount_node.text.trim(),
                        "currency": currency, "booked": booked, "reference": reference,
                    }),
                });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Fio CSV

/// Splits one `;`-separated line with `"` quoting (`""` inside quotes is a quote).
fn csv_fields(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn column(header: &[String], names: &[&str]) -> Option<usize> {
    header
        .iter()
        .position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
}

fn fio_csv(bytes: &[u8]) -> Result<Statement, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| bad("the CSV must be UTF-8"))?;
    let text = text.trim_start_matches('\u{feff}');
    let mut out = Statement::default();
    let mut header: Option<Vec<String>> = None;
    let mut cols = [None; 9];
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = csv_fields(line, ';');
        let Some(h) = &header else {
            // The preamble: `"iban";"CZ…"` and friends, until the column header.
            if fields
                .first()
                .is_some_and(|f| f.trim().eq_ignore_ascii_case("iban"))
            {
                out.iban = fields.get(1).and_then(|v| non_empty(v, 34));
            }
            if column(&fields, &["ID pohybu", "ID operace", "ID transaction"]).is_some() {
                cols = [
                    column(&fields, &["ID pohybu", "ID operace", "ID transaction"]),
                    column(&fields, &["Datum", "Date"]),
                    column(&fields, &["Objem", "Volume", "Amount", "Částka"]),
                    column(&fields, &["Měna", "Currency"]),
                    column(&fields, &["VS"]),
                    column(&fields, &["Protiúčet", "Counter account"]),
                    column(&fields, &["Kód banky", "Bank code"]),
                    column(&fields, &["Název protiúčtu", "Counter account name"]),
                    column(&fields, &["Zpráva pro příjemce", "Message for recipient"]),
                ];
                if cols[..4].iter().any(Option::is_none) {
                    return Err(bad(
                        "the CSV header needs ID pohybu, Datum, Objem and Měna columns",
                    ));
                }
                header = Some(fields);
            }
            continue;
        };
        let get = |i: Option<usize>| i.and_then(|i| fields.get(i)).map(String::as_str);
        let id = get(cols[0]).map(str::trim).unwrap_or_default();
        if id.is_empty() {
            return Err(bad(format!("line {}: no transaction id", n + 1)));
        }
        let account = match (get(cols[5]), get(cols[6])) {
            (Some(a), Some(b)) if !a.trim().is_empty() && !b.trim().is_empty() => {
                format!("{}/{}", a.trim(), b.trim())
            }
            (Some(a), _) => a.trim().to_owned(),
            _ => String::new(),
        };
        let raw: serde_json::Map<String, Value> = h
            .iter()
            .zip(fields.iter())
            .map(|(k, v)| (k.trim().to_owned(), Value::String(v.clone())))
            .collect();
        out.lines.push(StatementLine {
            bank_tx_id: id.chars().take(100).collect(),
            booked_on: parse_date(get(cols[1]).unwrap_or_default())?,
            amount_minor: parse_amount(get(cols[2]).unwrap_or_default())?,
            currency: get(cols[3]).unwrap_or_default().trim().to_owned(),
            variable_symbol: get(cols[4]).and_then(variable_symbol),
            counterparty: non_empty(&account, 100),
            counterparty_name: get(cols[7]).and_then(|v| non_empty(v, 200)),
            message: get(cols[8]).and_then(|v| non_empty(v, 500)),
            raw: Value::Object(raw),
        });
    }
    if header.is_none() {
        return Err(bad(
            "no Fio CSV column header (ID pohybu;Datum;Objem;Měna;…)",
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// GPC (ABO)

/// ISO 4217 numeric codes used in GPC files.
fn gpc_currency(code: &str) -> Option<&'static str> {
    match code.trim_start_matches('0') {
        "203" => Some("CZK"),
        "978" => Some("EUR"),
        "840" => Some("USD"),
        "985" => Some("PLN"),
        "348" => Some("HUF"),
        _ => None,
    }
}

/// Fixed-width GPC: `074` header, `075` transaction records (128 characters). Positions below
/// are 1-based as in the bank specifications. Text fields are Windows-1250; only ASCII is kept.
fn gpc(bytes: &[u8]) -> Result<Statement, Error> {
    let mut out = Statement::default();
    for (n, raw_line) in bytes.split(|b| *b == b'\n').enumerate() {
        let line: String = raw_line
            .iter()
            .map(|b| if b.is_ascii() { char::from(*b) } else { '?' })
            .collect();
        let line = line.trim_end_matches(['\r', ' ']);
        let field = |from: usize, to: usize| line.get(from - 1..to).unwrap_or_default();
        match line.get(..3) {
            Some("074") => out.domestic = non_empty(field(4, 19), 16),
            Some("075") => {
                if line.len() < 128 {
                    return Err(bad(format!(
                        "line {}: a 075 record has 128 characters",
                        n + 1
                    )));
                }
                let sign = match field(61, 61) {
                    "2" | "4" => 1,
                    "1" | "5" => -1,
                    other => return Err(bad(format!("line {}: accounting code {other:?}", n + 1))),
                };
                let amount: i64 = field(49, 60)
                    .parse()
                    .map_err(|_| bad(format!("line {}: invalid amount", n + 1)))?;
                let date = field(123, 128);
                let booked_on = NaiveDate::parse_from_str(date, "%d%m%y")
                    .map_err(|_| bad(format!("line {}: invalid date {date:?}", n + 1)))?;
                let currency = gpc_currency(field(119, 122))
                    .ok_or_else(|| bad(format!("line {}: unknown currency code", n + 1)))?;
                let id = field(36, 48).trim_start_matches('0');
                if id.is_empty() {
                    return Err(bad(format!("line {}: no transaction id", n + 1)));
                }
                let counterparty = field(20, 35).trim_start_matches('0');
                let bank = field(74, 77);
                out.lines.push(StatementLine {
                    bank_tx_id: id.to_owned(),
                    booked_on,
                    amount_minor: sign * amount,
                    currency: currency.to_owned(),
                    variable_symbol: variable_symbol(field(62, 71)),
                    counterparty: non_empty(&format!("{counterparty}/{bank}"), 100)
                        .filter(|_| !counterparty.is_empty()),
                    counterparty_name: non_empty(field(98, 117), 200),
                    message: None,
                    raw: json!({ "record": line }),
                });
            }
            _ => {}
        }
    }
    if out.domestic.is_none() && out.lines.is_empty() {
        return Err(bad("no GPC records (074/075)"));
    }
    Ok(out)
}

/// The Czech domestic account (prefix + number, 16 digits) inside a CZ IBAN.
pub fn cz_domestic(iban: &str) -> Option<&str> {
    iban.strip_prefix("CZ").and_then(|r| r.get(6..22))
}

// ---------------------------------------------------------------------------------------
// Fio API JSON (`/v1/rest/periods/{token}/{from}/{to}/transactions.json`)

#[derive(Deserialize)]
struct FioDoc {
    #[serde(rename = "accountStatement")]
    statement: FioStatement,
}

#[derive(Deserialize)]
struct FioStatement {
    info: FioInfo,
    #[serde(rename = "transactionList")]
    list: FioList,
}

#[derive(Deserialize)]
struct FioInfo {
    iban: Option<String>,
    currency: Option<String>,
}

#[derive(Deserialize)]
struct FioList {
    #[serde(default)]
    transaction: Vec<serde_json::Map<String, Value>>,
}

pub fn fio_json(bytes: &[u8]) -> Result<Statement, Error> {
    let doc: FioDoc =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid Fio API response: {e}")))?;
    let info = doc.statement.info;
    let mut out = Statement {
        iban: info.iban,
        ..Statement::default()
    };
    for tx in doc.statement.list.transaction {
        // Columns are `{"value": …, "name": …, "id": n}` or null.
        let col = |n: u32| {
            tx.get(&format!("column{n}"))
                .and_then(|c| c.get("value"))
                .filter(|v| !v.is_null())
        };
        let text = |n: u32| {
            col(n).map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        };
        let id = text(22).ok_or_else(|| bad("a Fio transaction has no id (column22)"))?;
        let amount = col(1)
            .map(|v| match v {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .ok_or_else(|| bad("a Fio transaction has no amount (column1)"))?;
        let currency = text(14)
            .or_else(|| info.currency.clone())
            .ok_or_else(|| bad("a Fio transaction has no currency"))?;
        let account = match (text(2), text(3)) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            (Some(a), None) => a,
            _ => String::new(),
        };
        out.lines.push(StatementLine {
            bank_tx_id: id.chars().take(100).collect(),
            booked_on: parse_date(&text(0).ok_or_else(|| bad("a Fio transaction has no date"))?)?,
            amount_minor: parse_amount(&amount)?,
            currency,
            variable_symbol: text(5).as_deref().and_then(variable_symbol),
            counterparty: non_empty(&account, 100),
            counterparty_name: text(10).as_deref().and_then(|v| non_empty(v, 200)),
            message: text(16).as_deref().and_then(|v| non_empty(v, 500)),
            raw: Value::Object(tx.clone()),
        });
    }
    if out.lines.len() > MAX_LINES {
        return Err(bad(format!("at most {MAX_LINES} lines per statement")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../fixtures/bank/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("1 500,00").unwrap(), 150_000);
        assert_eq!(parse_amount("1500.5").unwrap(), 150_050);
        assert_eq!(parse_amount("-20,00").unwrap(), -2000);
        assert_eq!(parse_amount("12").unwrap(), 1200);
        for bad in ["", "1.234", "abc", "1,2,3", "--1", "1e5"] {
            assert!(parse_amount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn variable_symbols() {
        assert_eq!(variable_symbol("100001").as_deref(), Some("100001"));
        assert_eq!(variable_symbol("0000100001").as_deref(), Some("100001"));
        assert_eq!(variable_symbol("VS:100001").as_deref(), Some("100001"));
        assert_eq!(variable_symbol("/VS/100001/SS/").as_deref(), Some("100001"));
        assert_eq!(
            variable_symbol("platba vs 42 dekuji").as_deref(),
            Some("42")
        );
        assert_eq!(variable_symbol("0000000000"), None);
        assert_eq!(variable_symbol("12345678901"), None);
        assert_eq!(variable_symbol("no symbol"), None);
    }

    #[test]
    fn camt053_credits_debits_and_batches() {
        let s = parse(Format::Camt053, &fixture("camt053.xml")).unwrap();
        assert_eq!(s.iban.as_deref(), Some("CZ6508000000192000145399"));
        let lines: Vec<_> = s
            .lines
            .iter()
            .map(|l| {
                (
                    l.bank_tx_id.as_str(),
                    l.amount_minor,
                    l.variable_symbol.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                ("20260925-0001", 48_050, Some("100001")),
                ("20260925-0002", -120_000, None),
                ("B-1", 10_000, Some("100002")),
                ("B-2", 20_000, Some("100003")),
                ("20260925-0004", 5_000, Some("100004")),
            ]
        );
        let first = &s.lines[0];
        assert_eq!(first.currency, "CZK");
        assert_eq!(
            first.counterparty.as_deref(),
            Some("CZ5855000000001265098001")
        );
        assert_eq!(first.counterparty_name.as_deref(), Some("Jan Novák & syn"));
        assert_eq!(first.booked_on.to_string(), "2026-09-25");
    }

    #[test]
    fn camt053_refuses_doctype_and_garbage() {
        let xxe = br#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><Document><BkToCstmrStmt/></Document>"#;
        assert!(parse(Format::Camt053, xxe).is_err());
        assert!(parse(Format::Camt053, b"<Document><Other/></Document>").is_err());
        assert!(parse(Format::Camt053, b"not xml <<").is_err());
        let deep = format!("{}{}", "<a>".repeat(100), "</a>".repeat(100));
        assert!(parse(Format::Camt053, deep.as_bytes()).is_err());
    }

    #[test]
    fn fio_csv_export() {
        let s = parse(Format::FioCsv, &fixture("fio.csv")).unwrap();
        assert_eq!(s.iban.as_deref(), Some("CZ7920100000002000000000"));
        assert_eq!(s.lines.len(), 3);
        let l = &s.lines[0];
        assert_eq!(l.bank_tx_id, "26962199069");
        assert_eq!(l.amount_minor, 150_000);
        assert_eq!(l.variable_symbol.as_deref(), Some("100001"));
        assert_eq!(l.counterparty.as_deref(), Some("2900233333/2010"));
        assert_eq!(l.counterparty_name.as_deref(), Some("Novák; Jan"));
        assert_eq!(s.lines[1].amount_minor, -20_000);
        assert_eq!(s.lines[2].variable_symbol, None);
        assert!(parse(Format::FioCsv, b"a;b\n1;2").is_err());
    }

    #[test]
    fn gpc_records() {
        let s = parse(Format::Gpc, &fixture("statement.gpc")).unwrap();
        assert_eq!(s.domestic.as_deref(), Some("0000002000000000"));
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[0].amount_minor, 150_000);
        assert_eq!(s.lines[0].variable_symbol.as_deref(), Some("100001"));
        assert_eq!(s.lines[0].currency, "CZK");
        assert_eq!(s.lines[0].booked_on.to_string(), "2026-09-25");
        assert_eq!(s.lines[1].amount_minor, -5_000);
        assert_eq!(
            cz_domestic("CZ7920100000002000000000"),
            Some("0000002000000000")
        );
    }

    #[test]
    fn fio_api_json() {
        let s = fio_json(&fixture("fio-api.json")).unwrap();
        assert_eq!(s.iban.as_deref(), Some("CZ7920100000002000000000"));
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[0].bank_tx_id, "10000000002");
        assert_eq!(s.lines[0].amount_minor, 150_000);
        assert_eq!(s.lines[0].variable_symbol.as_deref(), Some("100001"));
        assert_eq!(s.lines[0].booked_on.to_string(), "2026-09-25");
        assert_eq!(s.lines[1].amount_minor, 12_345);
        assert!(fio_json(b"{}").is_err());
    }
}
