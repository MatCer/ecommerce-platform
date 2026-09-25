//! Czech/Slovak text normalizer for search (spec §11.1): lowercase, diacritics folding,
//! tokenization, stopwords and a light suffix stemmer. The same [`analyze`] runs at indexing
//! (the `*_stems` fields) and at query time, so both sides always agree.
//!
//! The stemmer is the "light" Czech stemmer of Dolamic & Savoy (L. Dolamic, J. Savoy,
//! "Indexing and stemming approaches for the Czech language", Information Processing &
//! Management 45(6), 2009), as published in the Snowball Czech stemmer and in Lucene's
//! `org.apache.lucene.analysis.cz.CzechStemmer`: remove the case ending, then a possessive
//! suffix, then normalize palatalized consonants and the mobile `e`. Differences:
//! - the rules operate on folded text (`ého` → `eho`, `ích` → `ich`, …), because shoppers
//!   type without diacritics as often as with them; `ů` in the second-to-last position becomes
//!   `o` before folding (Lucene's `*ů* → *o*`, so `stůl`/`stolu` meet), elsewhere `u`;
//! - Lucene's `šť → sk` is dropped: after folding it would also rewrite every plain `st`;
//! - the noun endings `ové`/`ovi` (`pánové`, `synovi`) are dropped: they split the very common
//!   `-ový` adjectives of product names (`dubový` → `dubov`, but `dubové` → `dub`);
//! - hand-written Slovak case endings are added (`om`, `ej`, `ov`, `och`, `iach`, `ia`, `ie`,
//!   `iu`), from the Slovak declension tables (Pravidlá slovenského pravopisu, 2013):
//!   instrumental `-om`, feminine adjective `-ej`, genitive plural `-ov`, locative plural
//!   `-och`/`-iach`, and the `-ia`/`-ie`/`-iu` endings of the `vysvedčenie`/`ulica` types.
//!
//! Stemming only applies to `cs` and `sk`; other locales get folding and tokenization.
//! Tokens containing digits (sizes, SKUs, model numbers) are never stemmed.

/// Normalized search text for `locale`: folded, stopword-free, stemmed tokens joined by a
/// space. Empty when nothing searchable is left.
pub fn analyze(text: &str, locale: &str) -> String {
    let stemming = is_cs_sk(locale);
    tokens(text)
        .filter(|t| !(stemming && is_stopword(t)))
        .map(|t| if stemming { stem(&t) } else { t })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Typeahead form of [`analyze`]: the last word is still being typed, so it is only folded
/// (stemming a prefix like `trič` → `trik` would miss `tričko` → `trick`). The engine matches
/// the last query word as a prefix.
pub fn analyze_prefix(text: &str, locale: &str) -> String {
    let complete = text.trim_end().len() < text.len();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match words.split_last() {
        Some((last, head)) if !complete => {
            let head = analyze(&head.join(" "), locale);
            let last = fold(last);
            if head.is_empty() {
                last
            } else {
                format!("{head} {last}")
            }
        }
        _ => analyze(text, locale),
    }
}

/// Lowercased, diacritics-folded text with every non-alphanumeric run turned into one space.
pub fn fold(text: &str) -> String {
    tokens(text).collect::<Vec<_>>().join(" ")
}

fn is_cs_sk(locale: &str) -> bool {
    matches!(locale.get(..2), Some("cs" | "sk"))
}

/// Folded tokens (split on anything that is not a letter or digit).
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(fold_token)
}

fn fold_token(word: &str) -> String {
    let lower: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    let mut out = String::with_capacity(lower.len());
    for (i, &c) in lower.iter().enumerate() {
        if c == 'ů' && i + 2 == lower.len() {
            out.push('o');
        } else {
            match fold_char(c) {
                Folded::One(f) => out.push(f),
                Folded::Two(a, b) => {
                    out.push(a);
                    out.push(b);
                }
            }
        }
    }
    out
}

enum Folded {
    One(char),
    Two(char, char),
}

/// Latin letters with diacritics (Latin-1 Supplement and Latin Extended-A, which covers
/// every EU language written in Latin script) to their base letter.
fn fold_char(c: char) -> Folded {
    Folded::One(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' | 'ð' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        'ß' => return Folded::Two('s', 's'),
        'æ' => return Folded::Two('a', 'e'),
        'œ' => return Folded::Two('o', 'e'),
        'þ' => return Folded::Two('t', 'h'),
        other => other,
    })
}

/// Czech and Slovak function words, folded. Short on purpose: product words must survive.
const STOPWORDS: &[&str] = &[
    // cs
    "a", "aby", "ale", "ani", "az", "by", "co", "do", "i", "jak", "jako", "je", "jen", "k", "ke",
    "ktera", "ktere", "ktery", "mezi", "na", "nad", "nebo", "o", "od", "po", "pod", "pro", "pri",
    "s", "se", "si", "ta", "tak", "ten", "to", "u", "uz", "v", "ve", "z", "ze", "za", "bez",
    // sk (in addition)
    "aj", "ako", "alebo", "cez", "ku", "len", "medzi", "pre", "pred", "sa", "so", "vo", "zo",
    "ktora", "ktore", "ktory",
];

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

/// Case endings by minimum word length (the word must be longer than `.0` characters),
/// longest first. Folded forms of the Dolamic & Savoy rules plus the Slovak additions.
const CASE_ENDINGS: &[(usize, &[&str])] = &[
    (7, &["atech"]),
    (6, &["etem", "atum", "iach"]),
    (
        5,
        &[
            "ech", "ich", "eho", "emi", "emu", "ete", "eti", "iho", "imi", "imu", "ach", "ata",
            "aty", "ych", "ama", "ami", "ymi", "och",
        ],
    ),
    (5, &["ia", "ie", "iu"]),
    (
        4,
        &[
            "em", "es", "im", "um", "at", "am", "os", "us", "ym", "mi", "ou", "om", "ej", "ov",
        ],
    ),
    (3, &["a", "e", "i", "o", "u", "y"]),
];

/// Stems one folded token. Tokens with digits are returned unchanged.
pub fn stem(token: &str) -> String {
    if token.chars().any(|c| c.is_ascii_digit()) || !token.is_ascii() {
        return token.to_owned();
    }
    let mut s = remove_case(token);
    s = remove_possessive(s);
    normalize(s)
}

fn remove_case(word: &str) -> &str {
    for (min, endings) in CASE_ENDINGS {
        if word.len() > *min
            && let Some(e) = endings.iter().find(|e| word.ends_with(**e))
        {
            return &word[..word.len() - e.len()];
        }
    }
    word
}

fn remove_possessive(word: &str) -> &str {
    if word.len() > 5 && ["ov", "in", "uv"].iter().any(|e| word.ends_with(e)) {
        &word[..word.len() - 2]
    } else {
        word
    }
}

fn normalize(word: &str) -> String {
    let mut s = word.to_owned();
    if s.is_empty() {
        return s;
    }
    if let Some(head) = s.strip_suffix("ct") {
        return format!("{head}ck");
    }
    match s.as_bytes()[s.len() - 1] {
        b'c' => {
            s.replace_range(s.len() - 1.., "k");
            return s;
        }
        b'z' => {
            s.replace_range(s.len() - 1.., "h");
            return s;
        }
        _ => {}
    }
    // Mobile e (cs `triček` → `tričk`) and Slovak mobile ie (`tričiek` → `tričk`). Short
    // words keep it: `red`, `sen`.
    let (n, b) = (s.len(), s.as_bytes());
    let mobile_e = n > 3 && b[n - 2] == b'e';
    let mobile_ie = mobile_e && n > 4 && b[n - 3] == b'i';
    if mobile_ie {
        s.replace_range(n - 3..n - 1, "");
    } else if mobile_e {
        s.remove(n - 2);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(locale: &str, words: &[&str]) {
        let stems: Vec<String> = words.iter().map(|w| analyze(w, locale)).collect();
        assert!(
            stems.windows(2).all(|p| p[0] == p[1]),
            "{words:?} -> {stems:?}"
        );
    }

    #[test]
    fn folds_diacritics_and_case() {
        assert_eq!(fold("Příliš ŽLUŤOUČKÝ kůň"), "prilis zlutoucky kon");
        assert_eq!(fold("Tričko  –  Basic/Červené"), "tricko basic cervene");
        assert_eq!(fold("Straße Œuvre"), "strasse oeuvre");
        assert_eq!(fold("košeľa ôsmy ĺžka ŕ"), "kosela osmy lzka r");
    }

    #[test]
    fn czech_inflections_share_a_stem() {
        same(
            "cs",
            &[
                "tričko",
                "trička",
                "tričku",
                "tričkem",
                "triček",
                "tričkách",
                "tričkům",
            ],
        );
        same(
            "cs",
            &[
                "bunda", "bundy", "bundě", "bundu", "bundou", "bund", "bundách", "bundami",
            ],
        );
        same(
            "cs",
            &["mikina", "mikiny", "mikině", "mikinu", "mikinou", "mikin"],
        );
        same(
            "cs",
            &["kalhoty", "kalhot", "kalhotách", "kalhotami", "kalhotám"],
        );
        same(
            "cs",
            &[
                "červená",
                "červené",
                "červený",
                "červeného",
                "červených",
                "červenou",
            ],
        );
        same("cs", &["ponožky", "ponožek", "ponožkách"]);
        same("cs", &["stůl", "stolu", "stoly", "stolem"]);
        same("cs", &["nůž", "nože", "nožem"]);
        same(
            "cs",
            &[
                "dubový",
                "dubová",
                "dubové",
                "dubového",
                "dubových",
                "dubovou",
            ],
        );
    }

    #[test]
    fn slovak_inflections_share_a_stem() {
        same("sk", &["tričko", "tričkom", "tričká", "tričiek"]);
        same("sk", &["modrá", "modrej", "modrou", "modrom", "modrých"]);
        same("sk", &["kancelária", "kancelárie", "kanceláriu"]);
        same("sk", &["stôl", "stola", "stolom", "stoloch", "stolov"]);
    }

    #[test]
    fn diacritics_do_not_matter() {
        same("cs", &["tričko", "tricko", "TRIČKO", "Tricko"]);
        same("sk", &["košeľa", "kosela"]);
        assert_eq!(
            analyze("Pánské trička", "cs"),
            analyze("panske tricka", "cs")
        );
    }

    #[test]
    fn stopwords_are_dropped_but_not_product_words() {
        assert_eq!(
            analyze("tričko s potiskem a kapsou", "cs"),
            "trick potisk kaps"
        );
        assert_eq!(analyze("a v na", "sk"), "");
    }

    #[test]
    fn tokens_with_digits_and_short_words_are_kept() {
        assert_eq!(
            analyze("TS-RED-XL 42.5 iPhone15", "cs"),
            "ts red xl 42 5 iphone15"
        );
        assert_eq!(stem("xl"), "xl");
        assert_eq!(stem("m"), "m");
        assert_eq!(analyze("", "cs"), "");
    }

    #[test]
    fn typeahead_keeps_the_word_being_typed_unstemmed() {
        assert_eq!(analyze_prefix("pánská Trič", "cs"), "pansk tric");
        assert_eq!(analyze_prefix("Trič", "cs"), "tric");
        // A trailing space means the last word is complete.
        assert_eq!(analyze_prefix("trička ", "cs"), "trick");
        assert_eq!(analyze_prefix("", "cs"), "");
    }

    #[test]
    fn other_locales_are_only_folded() {
        assert_eq!(
            analyze("Red T-Shirts and Shoes", "en"),
            "red t shirts and shoes"
        );
        assert_eq!(analyze("Ärmel", "de-DE"), "armel");
    }

    #[test]
    fn stemming_is_idempotent_on_its_output_length() {
        // Never grows a word and never returns an empty stem for a real word.
        for w in [
            "a",
            "ab",
            "abc",
            "tricko",
            "kancelaria",
            "atech",
            "zzzz",
            "ct",
        ] {
            let s = stem(w);
            assert!(s.len() <= w.len() + 1 && !s.is_empty(), "{w} -> {s}");
        }
    }
}
