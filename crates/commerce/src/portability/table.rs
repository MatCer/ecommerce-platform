//! Reading an uploaded CSV (RFC 4180 via the `csv` crate) into a column-mapped table, and the
//! strict cell validators the import kinds share. Pure code: no database, no I/O.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// Largest accepted file.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Data rows (after the header) a file may have.
pub const MAX_ROWS: usize = 50_000;
/// Characters per cell.
pub const MAX_CELL: usize = 1000;
/// Columns per file.
pub const MAX_COLUMNS: usize = 100;

/// A column this import kind understands.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub required: bool,
}

pub const fn field(name: &'static str, required: bool) -> Field {
    Field { name, required }
}

/// A file-level problem: the run fails with this message (fix the file or the mapping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError(pub String);

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The parsed file: headers, and per data row its line number and cells by field name.
#[derive(Debug, Default)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Default, Clone)]
pub struct Row {
    /// 1-based line of the record in the file (the header is line 1).
    pub line: u64,
    cells: HashMap<&'static str, String>,
}

impl Row {
    /// The trimmed cell, `None` when empty or unmapped.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.cells
            .get(field)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    }

    #[cfg(test)]
    pub fn of(line: u64, cells: &[(&'static str, &str)]) -> Self {
        Self {
            line,
            cells: cells.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
        }
    }
}

/// `;` when the header line has more semicolons than commas (Czech/Slovak Excel), else `,`.
fn delimiter(bytes: &[u8]) -> u8 {
    let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let count = |c: u8| first.iter().filter(|b| **b == c).count();
    if count(b';') > count(b',') {
        b';'
    } else {
        b','
    }
}

/// Parses `bytes` and maps its columns to `fields`: `mapping` names the header for a field (an
/// empty name ignores the field); unmapped fields use a header of the same name
/// (case-insensitive). Every required field must
/// resolve; headers must be unique; the file must be UTF-8 and within the limits.
pub fn read(
    bytes: &[u8],
    fields: &[Field],
    mapping: &BTreeMap<String, String>,
) -> Result<Table, FileError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if std::str::from_utf8(bytes).is_err() {
        return Err(FileError(
            "the file is not UTF-8 text; save it as CSV UTF-8".into(),
        ));
    }
    for key in mapping.keys() {
        if !fields.iter().any(|f| f.name == key) {
            return Err(FileError(format!("unknown field {key:?} in the mapping")));
        }
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter(bytes))
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| FileError(format!("unreadable header row: {e}")))?
        .iter()
        .map(|h| h.trim().to_owned())
        .collect();
    if headers.iter().all(String::is_empty) {
        return Err(FileError("the file has no header row".into()));
    }
    if headers.len() > MAX_COLUMNS {
        return Err(FileError(format!("more than {MAX_COLUMNS} columns")));
    }
    for (i, h) in headers.iter().enumerate() {
        if !h.is_empty() && headers[..i].iter().any(|o| o.eq_ignore_ascii_case(h)) {
            return Err(FileError(format!("the column {h:?} appears twice")));
        }
    }
    let mut columns: Vec<(&'static str, usize)> = Vec::new();
    for f in fields {
        let wanted = mapping.get(f.name).map_or(f.name, String::as_str);
        // An empty mapping ignores the field even when a same-named column exists.
        if wanted.trim().is_empty() {
            if f.required {
                return Err(FileError(format!(
                    "the required field {} has no column; map it to one",
                    f.name
                )));
            }
            continue;
        }
        match headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(wanted.trim()))
        {
            Some(i) => columns.push((f.name, i)),
            None if mapping.contains_key(f.name) => {
                return Err(FileError(format!(
                    "the column {wanted:?} mapped to {} is not in the file",
                    f.name
                )));
            }
            None if f.required => {
                return Err(FileError(format!(
                    "the required field {} has no column; map it to one",
                    f.name
                )));
            }
            None => {}
        }
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| FileError(format!("malformed CSV: {e}")))?;
        if record.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if rows.len() == MAX_ROWS {
            return Err(FileError(format!("more than {MAX_ROWS} rows")));
        }
        let line = record.position().map_or(0, csv::Position::line);
        if let Some(long) = record.iter().find(|c| c.chars().count() > MAX_CELL) {
            let head: String = long.chars().take(20).collect();
            return Err(FileError(format!(
                "line {line}: a cell is longer than {MAX_CELL} characters ({head}...)"
            )));
        }
        let cells = columns
            .iter()
            .map(|(name, i)| (*name, record.get(*i).unwrap_or_default().to_owned()))
            .collect();
        rows.push(Row { line, cells });
    }
    Ok(Table { headers, rows })
}

// ---------------------------------------------------------------------------------------
// Cell validators. `Err` is a stable snake_case code plus a human detail.

pub type CellResult<T> = Result<T, (&'static str, String)>;

/// Free text: no control characters (line breaks included), at most `max` characters.
pub fn text(v: &str, max: usize) -> CellResult<String> {
    if v.chars().any(char::is_control) {
        return Err(("invalid_text", "control characters are not allowed".into()));
    }
    if v.chars().count() > max {
        return Err(("too_long", format!("at most {max} characters")));
    }
    Ok(v.to_owned())
}

pub fn email(v: &str) -> CellResult<String> {
    crate::staff::normalize_email(v)
        .map_err(|_| ("invalid_email", format!("{v:?} is not an email address")))
}

pub fn phone(v: &str) -> CellResult<String> {
    let ok = v.len() <= 40
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_digit() || " +-()/.".contains(c));
    if ok {
        Ok(v.to_owned())
    } else {
        Err(("invalid_phone", format!("{v:?} is not a phone number")))
    }
}

pub fn locale(v: &str) -> CellResult<String> {
    let ok = regex_like_locale(v);
    if ok {
        Ok(v.to_owned())
    } else {
        Err((
            "invalid_locale",
            format!("{v:?} is not a locale like cs or cs-CZ"),
        ))
    }
}

fn regex_like_locale(v: &str) -> bool {
    let b = v.as_bytes();
    let lang = |s: &[u8]| s.len() == 2 && s.iter().all(u8::is_ascii_lowercase);
    match b.len() {
        2 => lang(b),
        5 => lang(&b[..2]) && b[2] == b'-' && b[3..].iter().all(u8::is_ascii_uppercase),
        _ => false,
    }
}

/// ISO 3166-1 alpha-2, any case in, upper case out.
pub fn country(v: &str) -> CellResult<String> {
    let up = v.to_ascii_uppercase();
    if up.len() == 2 && up.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(up)
    } else {
        Err((
            "invalid_country",
            format!("{v:?} is not a two-letter country code"),
        ))
    }
}

pub fn currency(v: &str) -> CellResult<String> {
    let up = v.to_ascii_uppercase();
    if up.len() == 3 && up.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(up)
    } else {
        Err((
            "invalid_currency",
            format!("{v:?} is not a currency code like CZK"),
        ))
    }
}

/// A non-negative amount with at most two decimals (`1 234,50`, `1234.5`) in minor units.
pub fn amount(v: &str) -> CellResult<i64> {
    match crate::payments::statements::parse_amount(v) {
        Ok(m) if m >= 0 => Ok(m),
        _ => Err((
            "invalid_amount",
            format!("{v:?} is not an amount like 1234.50"),
        )),
    }
}

pub fn quantity(v: &str) -> CellResult<i32> {
    match v.parse::<i32>() {
        Ok(q) if (1..=100_000).contains(&q) => Ok(q),
        _ => Err((
            "invalid_quantity",
            format!("{v:?} is not a whole number 1-100000"),
        )),
    }
}

pub fn ip(v: &str) -> CellResult<IpAddr> {
    v.parse()
        .map_err(|_| ("invalid_ip", format!("{v:?} is not an IP address")))
}

/// A point in time in the past (after 1990): RFC 3339 (`2024-03-01T10:00:00Z`), or a local
/// Czech/Slovak time `2024-03-01 10:00[:00]`, `2024-03-01`, `1.3.2024 10:00`, `1.3.2024`.
pub fn past_time(v: &str, now: DateTime<Utc>) -> CellResult<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(v)
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            [
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%d %H:%M",
                "%d.%m.%Y %H:%M:%S",
                "%d.%m.%Y %H:%M",
            ]
            .iter()
            .find_map(|f| NaiveDateTime::parse_from_str(v, f).ok())
            .or_else(|| {
                ["%Y-%m-%d", "%d.%m.%Y"]
                    .iter()
                    .find_map(|f| NaiveDate::parse_from_str(v, f).ok())
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
            })
            .map(crate::invoicing::prague::from_local)
        });
    let floor = NaiveDate::from_ymd_opt(1990, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc());
    match parsed {
        Some(t) if t > now => Err(("in_future", format!("{v:?} is in the future"))),
        Some(t) if floor.is_some_and(|f| t < f) => {
            Err(("invalid_date", format!("{v:?} is before 1990")))
        }
        Some(t) => Ok(t),
        None => Err((
            "invalid_date",
            format!("{v:?} is not a date like 2024-03-01 or 2024-03-01T10:00:00Z"),
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const FIELDS: [Field; 3] = [
        field("email", true),
        field("name", false),
        field("phone", false),
    ];

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn reads_comma_and_semicolon_files_with_a_bom() {
        let t = read(
            b"\xEF\xBB\xBFEmail,Name\na@x.cz,Anna\n\n",
            &FIELDS,
            &map(&[]),
        )
        .unwrap();
        assert_eq!(t.headers, ["Email", "Name"]);
        assert_eq!(t.rows.len(), 1, "blank lines are skipped");
        assert_eq!(t.rows[0].get("email"), Some("a@x.cz"));
        assert_eq!(t.rows[0].line, 2);
        assert_eq!(t.rows[0].get("phone"), None);
        // An empty mapping ignores a same-named column.
        let t = read(b"email,name\na@x.cz,Anna\n", &FIELDS, &map(&[("name", "")])).unwrap();
        assert_eq!(t.rows[0].get("name"), None);

        let t = read(
            "e-mail;jméno\n\"b@x.cz\";\"Bára; Nová\"\n".as_bytes(),
            &FIELDS,
            &map(&[("email", "E-mail"), ("name", "jméno")]),
        )
        .unwrap();
        assert_eq!(t.rows[0].get("name"), Some("Bára; Nová"));
    }

    #[test]
    fn refuses_bad_files() {
        let err = |bytes: &[u8], m: &[(&str, &str)]| read(bytes, &FIELDS, &map(m)).unwrap_err().0;
        assert!(err(b"name\nAnna\n", &[]).contains("required field email"));
        assert!(err(b"email\n\xff\xfe\n", &[]).contains("UTF-8"));
        assert!(err(b"email,Email\na,b\n", &[]).contains("twice"));
        assert!(err(b"email,name\na@x.cz\n", &[]).contains("malformed"));
        assert!(err(b"email\na@x.cz\n", &[("name", "Jmeno")]).contains("not in the file"));
        assert!(err(b"email\na@x.cz\n", &[("bogus", "email")]).contains("unknown field"));
        assert!(err(b"email\na@x.cz\n", &[("email", "")]).contains("required field email"));
        let long = format!("email\n{}\n", "a".repeat(MAX_CELL + 1));
        assert!(err(long.as_bytes(), &[]).contains("longer than"));
        let many = format!("email\n{}", "a@x.cz\n".repeat(MAX_ROWS + 1));
        assert!(err(many.as_bytes(), &[]).contains("more than"));
    }

    #[test]
    fn validates_cells_strictly() {
        assert!(text("a\nb", 10).is_err());
        assert!(text("abc", 2).is_err());
        assert_eq!(email(" Anna@X.cz ").unwrap(), "anna@x.cz");
        assert!(email("nope").is_err());
        assert!(phone("+420 777 123 456").is_ok());
        assert!(phone("call me").is_err());
        assert!(locale("cs-CZ").is_ok() && locale("cs").is_ok());
        assert!(locale("CS").is_err() && locale("cze").is_err());
        assert_eq!(country("cz").unwrap(), "CZ");
        assert!(country("CZE").is_err());
        assert_eq!(amount("1 234,50").unwrap(), 123_450);
        assert!(amount("-5").is_err() && amount("1.234").is_err());
        assert!(quantity("0").is_err() && quantity("2").is_ok());
        assert!(ip("192.0.2.1").is_ok() && ip("2001:db8::1").is_ok() && ip("x").is_err());
    }

    #[test]
    fn parses_past_times_in_several_formats() {
        let now = "2026-09-25T12:00:00Z".parse().unwrap();
        let t = |v: &str| past_time(v, now).map(|t| t.to_rfc3339());
        assert_eq!(
            t("2024-03-01T10:00:00Z").unwrap(),
            "2024-03-01T10:00:00+00:00"
        );
        // Local Czech time: winter UTC+1, summer UTC+2.
        assert_eq!(t("2024-03-01 10:00").unwrap(), "2024-03-01T09:00:00+00:00");
        assert_eq!(t("1.7.2024 10:00").unwrap(), "2024-07-01T08:00:00+00:00");
        assert_eq!(t("2024-07-01").unwrap(), "2024-06-30T22:00:00+00:00");
        assert_eq!(t("2027-01-01").unwrap_err().0, "in_future");
        assert_eq!(t("1980-01-01").unwrap_err().0, "invalid_date");
        assert_eq!(t("yesterday").unwrap_err().0, "invalid_date");
    }
}
