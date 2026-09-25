//! Streaming feed readers (`quick-xml` events, no DOM): Heureka `SHOP/SHOPITEM` and Google
//! Merchant RSS (`channel/item`) or Atom (`entry`) with `g:` fields.
//!
//! Safety: quick-xml never expands DTD entities (only the five predefined ones and character
//! references), so entity bombs and external entities do nothing. Items, fields and text are
//! capped; a malformed document stops with an error at its position.

use std::io::BufRead;

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;

use super::{FeedItem, Source, parse_price};

pub const MAX_ITEMS: usize = 100_000;
const MAX_FIELDS: usize = 200;
const MAX_TEXT: usize = 100_000;
const MAX_DEPTH: usize = 16;
pub const MAX_IMAGES: usize = 10;
pub const MAX_PARAMS: usize = 50;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid XML at byte {position}: {message}")]
    Xml { position: u64, message: String },
    #[error("the feed has more than {MAX_ITEMS} items")]
    TooManyItems,
    #[error("no items found: is this a {0} feed?")]
    Empty(&'static str),
}

/// Raw item: direct children as (local name, text) and grouped children (PARAM, g:shipping,
/// g:product_detail) as (local name, [(child local name, text)]).
#[derive(Debug, Default)]
struct Raw {
    fields: Vec<(String, String)>,
    groups: Vec<(String, Vec<(String, String)>)>,
}

impl Raw {
    fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, v)| n == name && !v.trim().is_empty())
            .map(|(_, v)| v.trim())
    }

    fn all(&self, name: &str) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(move |(n, v)| n == name && !v.trim().is_empty())
            .map(|(_, v)| v.trim())
    }
}

/// Local name (`g:id` -> `id`): Google feeds may bind the namespace to any prefix.
fn local(name: &[u8]) -> String {
    let name = name.rsplit(|b| *b == b':').next().unwrap_or(name);
    String::from_utf8_lossy(name).into_owned()
}

fn xml_error(reader_pos: u64, e: impl std::fmt::Display) -> ParseError {
    ParseError::Xml {
        position: reader_pos,
        message: e.to_string(),
    }
}

/// Calls `f` for every item element (`item_tags`, by local name) in document order.
fn read_items<R: BufRead>(
    input: R,
    item_tags: &[&str],
    mut f: impl FnMut(Raw),
) -> Result<usize, ParseError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut count = 0usize;
    // Inside an item: element path below the item and the text of the current leaf.
    let mut item: Option<Raw> = None;
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut group: Vec<(String, String)> = Vec::new();
    let mut depth = 0usize;
    loop {
        let pos = reader.buffer_position();
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| xml_error(pos, e))?;
        match event {
            Event::Start(e) => {
                depth += 1;
                if depth > MAX_DEPTH + 8 {
                    return Err(xml_error(pos, "elements nested too deeply"));
                }
                let name = local(e.name().as_ref());
                if let Some(_raw) = item.as_mut() {
                    if path.len() >= MAX_DEPTH {
                        return Err(xml_error(pos, "elements nested too deeply"));
                    }
                    path.push(name);
                    text.clear();
                } else if item_tags.contains(&name.as_str()) {
                    if count >= MAX_ITEMS {
                        return Err(ParseError::TooManyItems);
                    }
                    item = Some(Raw::default());
                    path.clear();
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                let Some(raw) = item.as_mut() else { continue };
                match path.len() {
                    // The item itself ends.
                    0 => {
                        count += 1;
                        if let Some(raw) = item.take() {
                            f(raw);
                        }
                    }
                    // A direct child: a field, or the end of a group.
                    1 => {
                        let name = path.pop().unwrap_or_default();
                        if group.is_empty() {
                            if raw.fields.len() < MAX_FIELDS {
                                raw.fields.push((name, std::mem::take(&mut text)));
                            }
                        } else if raw.groups.len() < MAX_FIELDS {
                            raw.groups.push((name, std::mem::take(&mut group)));
                        } else {
                            group.clear();
                        }
                        text.clear();
                    }
                    // A grandchild (PARAM/PARAM_NAME, g:shipping/g:price, ...).
                    _ => {
                        let name = path.pop().unwrap_or_default();
                        if group.len() < MAX_FIELDS {
                            group.push((name, std::mem::take(&mut text)));
                        }
                        text.clear();
                    }
                }
            }
            Event::Text(t) if item.is_some() && !path.is_empty() => {
                let s = t.decode().map_err(|e| xml_error(pos, e))?;
                push_capped(&mut text, &s);
            }
            Event::CData(t) if item.is_some() && !path.is_empty() => {
                let s = t.decode().map_err(|e| xml_error(pos, e))?;
                push_capped(&mut text, &s);
            }
            Event::GeneralRef(r) if item.is_some() && !path.is_empty() => {
                if let Some(c) = r.resolve_char_ref().map_err(|e| xml_error(pos, e))? {
                    push_capped(&mut text, c.encode_utf8(&mut [0; 4]));
                } else {
                    let name = r.decode().map_err(|e| xml_error(pos, e))?;
                    // Custom (DTD) entities are never expanded.
                    if let Some(v) = resolve_predefined_entity(&name) {
                        push_capped(&mut text, v);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if item.is_some() {
        return Err(xml_error(
            reader.buffer_position(),
            "unexpected end of the document",
        ));
    }
    Ok(count)
}

fn push_capped(text: &mut String, s: &str) {
    if text.len() < MAX_TEXT {
        let room = MAX_TEXT - text.len();
        let cut = s
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|end| *end <= room)
            .last()
            .unwrap_or(0);
        text.push_str(&s[..cut]);
    }
}

fn opt(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn stock(s: Option<&str>) -> Option<i32> {
    s.and_then(|v| v.trim().parse::<i64>().ok())
        .map(|n| i32::try_from(n.clamp(0, 1_000_000)).unwrap_or(0))
}

fn heureka_item(raw: &Raw) -> FeedItem {
    let price = raw.get("PRICE_VAT").and_then(parse_price);
    let mut category: Vec<String> = raw
        .get("CATEGORYTEXT")
        .map(|c| {
            c.split('|')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // Heureka's own taxonomy starts with the portal name.
    if category
        .first()
        .is_some_and(|c| c.to_ascii_lowercase().starts_with("heureka."))
    {
        category.remove(0);
    }
    let images = raw
        .all("IMGURL")
        .chain(raw.all("IMGURL_ALTERNATIVE"))
        .take(MAX_IMAGES)
        .map(str::to_owned)
        .collect();
    let params = raw
        .groups
        .iter()
        .filter(|(n, _)| n == "PARAM")
        .filter_map(|(_, kids)| {
            let get = |k: &str| {
                kids.iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.trim().to_owned())
            };
            Some((get("PARAM_NAME")?, get("VAL")?))
        })
        .filter(|(n, v)| !n.is_empty() && !v.is_empty())
        .take(MAX_PARAMS)
        .collect();
    FeedItem {
        item_id: raw.get("ITEM_ID").unwrap_or_default().to_owned(),
        group_id: opt(raw.get("ITEMGROUP_ID")),
        name: raw
            .get("PRODUCTNAME")
            .or(raw.get("PRODUCT"))
            .unwrap_or_default()
            .to_owned(),
        description: raw.get("DESCRIPTION").unwrap_or_default().to_owned(),
        url: opt(raw.get("URL")),
        images,
        price_minor: price.as_ref().map(|p| p.0),
        currency: price.and_then(|p| p.1),
        ean: opt(raw.get("EAN")),
        brand: opt(raw.get("MANUFACTURER")),
        category,
        params,
        stock: stock(raw.get("STOCK_QUANTITY")),
    }
}

/// Google attributes imported as parameters, with display names per locale.
const GOOGLE_PARAMS: [(&str, &str, &str, &str); 6] = [
    ("color", "Barva", "Farba", "Color"),
    ("size", "Velikost", "Veľkosť", "Size"),
    ("material", "Materiál", "Materiál", "Material"),
    ("pattern", "Vzor", "Vzor", "Pattern"),
    ("gender", "Pohlaví", "Pohlavie", "Gender"),
    ("age_group", "Věková skupina", "Veková skupina", "Age group"),
];

fn google_item(raw: &Raw, locale: &str) -> FeedItem {
    let regular = raw.get("price").and_then(parse_price);
    // The current selling price; the regular price is not a reduction basis (A18).
    let sale = raw.get("sale_price").and_then(parse_price);
    let price = sale.or(regular);
    let category = raw
        .get("product_type")
        .map(|c| {
            c.split('>')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let images = raw
        .all("image_link")
        .chain(raw.all("additional_image_link"))
        .take(MAX_IMAGES)
        .map(str::to_owned)
        .collect();
    let params = GOOGLE_PARAMS
        .iter()
        .filter_map(|(key, cs, sk, en)| {
            let name = match locale.split('-').next() {
                Some("cs") => cs,
                Some("sk") => sk,
                _ => en,
            };
            raw.get(key).map(|v| ((*name).to_owned(), v.to_owned()))
        })
        .collect();
    FeedItem {
        item_id: raw.get("id").unwrap_or_default().to_owned(),
        group_id: opt(raw.get("item_group_id")),
        name: raw.get("title").unwrap_or_default().to_owned(),
        description: raw.get("description").unwrap_or_default().to_owned(),
        url: opt(raw.get("link")),
        images,
        price_minor: price.as_ref().map(|p| p.0),
        currency: price.and_then(|p| p.1),
        ean: opt(raw.get("gtin")),
        brand: opt(raw.get("brand")),
        category,
        params,
        stock: stock(raw.get("quantity")),
    }
}

/// Reads every item of a feed. `locale` names Google attribute parameters.
pub fn read<R: BufRead>(
    input: R,
    source: Source,
    locale: &str,
    mut f: impl FnMut(FeedItem),
) -> Result<usize, ParseError> {
    let n = match source {
        Source::Heureka => read_items(input, &["SHOPITEM"], |raw| f(heureka_item(&raw)))?,
        Source::Google => read_items(input, &["item", "entry"], |raw| {
            f(google_item(&raw, locale));
        })?,
    };
    if n == 0 {
        return Err(ParseError::Empty(source.as_str()));
    }
    Ok(n)
}

/// All items of a feed in memory (dry runs and imports read the stored file once).
pub fn read_all(bytes: &[u8], source: Source, locale: &str) -> Result<Vec<FeedItem>, ParseError> {
    let mut items = Vec::new();
    read(bytes, source, locale, |i| items.push(i))?;
    Ok(items)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const HEUREKA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<SHOP>
  <SHOPITEM>
    <ITEM_ID>TS-RED-M</ITEM_ID>
    <PRODUCTNAME>Tričko &amp; více &#x10D;ervená M</PRODUCTNAME>
    <DESCRIPTION><![CDATA[<p>Bavlna <b>100 %</b></p>]]></DESCRIPTION>
    <URL>https://old.example/tricko?v=1</URL>
    <IMGURL>https://old.example/a.jpg</IMGURL>
    <IMGURL_ALTERNATIVE>https://old.example/b.jpg</IMGURL_ALTERNATIVE>
    <PRICE_VAT>299,90</PRICE_VAT>
    <MANUFACTURER>Basic</MANUFACTURER>
    <CATEGORYTEXT>Heureka.cz | Oblečení | Trička</CATEGORYTEXT>
    <EAN>8594001021499</EAN>
    <ITEMGROUP_ID>TS</ITEMGROUP_ID>
    <PARAM><PARAM_NAME>Barva</PARAM_NAME><VAL>červená</VAL></PARAM>
    <PARAM><PARAM_NAME>Velikost</PARAM_NAME><VAL>M</VAL></PARAM>
    <STOCK_QUANTITY>7</STOCK_QUANTITY>
  </SHOPITEM>
  <SHOPITEM><ITEM_ID>X</ITEM_ID><PRODUCTNAME>Bez ceny</PRODUCTNAME></SHOPITEM>
</SHOP>"#;

    #[test]
    fn heureka_items() {
        let items = read_all(HEUREKA.as_bytes(), Source::Heureka, "cs").unwrap();
        assert_eq!(items.len(), 2);
        let i = &items[0];
        assert_eq!(i.item_id, "TS-RED-M");
        assert_eq!(i.name, "Tričko & více červená M");
        assert_eq!(i.description, "<p>Bavlna <b>100 %</b></p>");
        assert_eq!(i.price_minor, Some(29_990));
        assert_eq!(i.category, ["Oblečení", "Trička"]);
        assert_eq!(i.images.len(), 2);
        assert_eq!(i.group_id.as_deref(), Some("TS"));
        assert_eq!(
            i.params,
            [
                ("Barva".to_owned(), "červená".to_owned()),
                ("Velikost".to_owned(), "M".to_owned())
            ]
        );
        assert_eq!(i.stock, Some(7));
        assert_eq!(items[1].price_minor, None);
    }

    #[test]
    fn google_rss_items() {
        let xml = r#"<rss version="2.0" xmlns:g="http://base.google.com/ns/1.0"><channel>
          <title>Shop</title>
          <item>
            <g:id>SKU-1</g:id><g:title>Mikina</g:title><g:link>https://old.example/mikina</g:link>
            <g:image_link>https://old.example/m.jpg</g:image_link>
            <g:price>899.00 CZK</g:price><g:sale_price>799.00 CZK</g:sale_price>
            <g:item_group_id>M</g:item_group_id><g:product_type>Oblečení &gt; Mikiny</g:product_type>
            <g:color>šedá</g:color><g:size>L</g:size><g:gtin>8594001021499</g:gtin>
            <g:shipping><g:country>CZ</g:country><g:price>99 CZK</g:price></g:shipping>
          </item>
        </channel></rss>"#;
        let items = read_all(xml.as_bytes(), Source::Google, "cs").unwrap();
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!(i.item_id, "SKU-1");
        assert_eq!(i.price_minor, Some(79_900), "the current (sale) price");
        assert_eq!(i.currency.as_deref(), Some("CZK"));
        assert_eq!(i.category, ["Oblečení", "Mikiny"]);
        assert_eq!(
            i.params,
            [
                ("Barva".to_owned(), "šedá".to_owned()),
                ("Velikost".to_owned(), "L".to_owned())
            ]
        );
    }

    #[test]
    fn hostile_documents_fail_safely() {
        // Entity expansion is never performed (billion laughs, external entities).
        let bomb = r#"<?xml version="1.0"?><!DOCTYPE SHOP [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;"><!ENTITY x SYSTEM "file:///etc/passwd">]>
<SHOP><SHOPITEM><ITEM_ID>&b;&x;</ITEM_ID><PRODUCTNAME>n</PRODUCTNAME></SHOPITEM></SHOP>"#;
        let items = read_all(bomb.as_bytes(), Source::Heureka, "cs").unwrap();
        assert_eq!(items[0].item_id, "");
        assert!(matches!(
            read_all(
                b"<SHOP><SHOPITEM><ITEM_ID>1</ITEM_ID>",
                Source::Heureka,
                "cs"
            ),
            Err(ParseError::Xml { .. })
        ));
        assert!(matches!(
            read_all(b"<SHOP></SHOP>", Source::Heureka, "cs"),
            Err(ParseError::Empty(_))
        ));
        assert!(read_all(b"<rss><chan", Source::Google, "cs").is_err());
        let deep = format!(
            "<SHOP><SHOPITEM>{}{}</SHOPITEM></SHOP>",
            "<a>".repeat(40),
            "</a>".repeat(40)
        );
        assert!(read_all(deep.as_bytes(), Source::Heureka, "cs").is_err());
    }
}
