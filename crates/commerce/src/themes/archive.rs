//! Theme source archives (spec A6): `.tar.gz` files validated at the trust boundary.
//!
//! Only regular files (and directories, which are implied) are accepted, at relative paths of
//! plain segments under an allowlisted set of top-level entries. Symlinks, hard links, devices,
//! FIFOs, absolute paths, `..`, duplicates, more than [`MAX_ENTRIES`] entries or more than
//! [`MAX_EXPANDED_BYTES`] of content are rejected with every problem listed, so a merchant sees
//! why an upload was refused. Decompression is capped while reading (gzip bombs).
//!
//! [`write`] produces a deterministic archive (sorted paths, mtime 0, mode 0644, uid/gid 0),
//! which is what the builder unpacks inside the sandbox.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::sync::LazyLock;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use regex::Regex;
use serde_json::Value;

/// A6: the expanded archive is at most 50 MB.
pub const MAX_EXPANDED_BYTES: u64 = 50 * 1024 * 1024;
/// Upload size (compressed), spec §8.1.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 5_000;
const MAX_PATH_LEN: usize = 240;
/// Problems listed in one rejection.
const MAX_PROBLEMS: usize = 20;

pub const TOKENS_FILE: &str = "theme.tokens.json";
/// Platform-owned files (§9.1): if an archive carries them they must equal the platform's
/// (the contract lint checks it); the build always uses the platform's copies.
pub const PLATFORM_FILES: [&str; 3] = ["package.json", "astro.config.mjs", "tsconfig.json"];
/// Theme-owned top-level entries. `checks/` holds optional Playwright functional checks.
const THEME_DIRS: [&str; 3] = ["src", "public", "checks"];
const THEME_FILES: [&str; 2] = [TOKENS_FILE, "README.md"];

/// The files of a theme source: relative path → bytes. Directories are implied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    pub files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid theme archive: {}", .problems.join("; "))]
pub struct ArchiveError {
    pub problems: Vec<String>,
}

impl ArchiveError {
    fn one(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

static SEGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_@+~\[\]-][A-Za-z0-9._@+~\[\]-]*$").unwrap_or_else(|_| unreachable!())
});

/// Why `path` (as written in the archive) is not an acceptable theme file path, if it is not.
pub(crate) fn path_problem(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return Some(format!(
            "{path:?}: path is empty or longer than {MAX_PATH_LEN} characters"
        ));
    }
    if path.starts_with('/') {
        return Some(format!("{path:?}: absolute paths are not allowed"));
    }
    if path.contains('\\') {
        return Some(format!("{path:?}: backslashes are not allowed"));
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.contains(&"..") {
        return Some(format!("{path:?}: '..' is not allowed"));
    }
    if let Some(bad) = segments
        .iter()
        .find(|s| s.is_empty() || **s == "." || !SEGMENT.is_match(s))
    {
        return Some(format!("{path:?}: invalid path segment {bad:?}"));
    }
    let top = segments[0];
    let allowed = if segments.len() == 1 {
        THEME_FILES.contains(&top) || PLATFORM_FILES.contains(&top)
    } else {
        THEME_DIRS.contains(&top)
    };
    if !allowed {
        return Some(format!(
            "{path:?}: not part of a theme (allowed: src/, public/, checks/, {TOKENS_FILE}, \
             README.md and the platform files)"
        ));
    }
    None
}

/// A reader that fails once more than `left` bytes were read, remembering that it did.
struct Capped<R> {
    inner: R,
    left: u64,
    exceeded: bool,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.left {
            self.exceeded = true;
            return Err(io::Error::other("expanded size limit exceeded"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// Parses and validates a `.tar.gz` theme source (A6).
pub fn read(gz: &[u8]) -> Result<Source, ArchiveError> {
    if gz.len() > MAX_UPLOAD_BYTES {
        return Err(ArchiveError::one(format!(
            "the archive is larger than {} MB",
            MAX_UPLOAD_BYTES / 1024 / 1024
        )));
    }
    if !gz.starts_with(&[0x1f, 0x8b]) {
        return Err(ArchiveError::one(
            "not a gzip-compressed tar archive (.tar.gz)",
        ));
    }
    // Content cap + tar headers (512 bytes each, long names use a few more).
    let limit = MAX_EXPANDED_BYTES + (MAX_ENTRIES as u64 + 16) * 2048;
    let mut capped = Capped {
        inner: GzDecoder::new(gz),
        left: limit,
        exceeded: false,
    };
    let result = read_entries(&mut capped);
    if capped.exceeded {
        return Err(ArchiveError::one(format!(
            "the expanded archive is larger than {} MB",
            MAX_EXPANDED_BYTES / 1024 / 1024
        )));
    }
    result
}

fn read_entries<R: Read>(reader: R) -> Result<Source, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let mut problems = Vec::new();
    let mut files = BTreeMap::new();
    let mut total: u64 = 0;
    let mut count = 0usize;
    let broken = |e: io::Error| ArchiveError::one(format!("the archive cannot be read: {e}"));
    for entry in archive.entries().map_err(broken)? {
        let mut entry = entry.map_err(broken)?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(ArchiveError::one(format!(
                "more than {MAX_ENTRIES} entries"
            )));
        }
        let raw = entry.path_bytes().into_owned();
        let Ok(name) = String::from_utf8(raw) else {
            problems.push("a path is not valid UTF-8".to_owned());
            continue;
        };
        let name = name.strip_prefix("./").unwrap_or(&name).to_owned();
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        if kind.is_dir() {
            let dir = name.trim_end_matches('/');
            if !dir.is_empty()
                && dir != "."
                && let Some(p) = path_problem(&format!("{dir}/x"))
            {
                problems.push(p.replace("/x\"", "/\""));
            }
            continue;
        }
        if !(kind.is_file() || kind.is_contiguous()) {
            let what = if kind.is_symlink() {
                "symbolic links are not allowed"
            } else if kind.is_hard_link() {
                "hard links are not allowed"
            } else {
                "only regular files are allowed"
            };
            problems.push(format!("{name:?}: {what}"));
        } else if let Some(p) = path_problem(&name) {
            problems.push(p);
        } else {
            total = total.saturating_add(entry.size());
            if total > MAX_EXPANDED_BYTES {
                return Err(ArchiveError::one(format!(
                    "the expanded archive is larger than {} MB",
                    MAX_EXPANDED_BYTES / 1024 / 1024
                )));
            }
            let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
            entry.read_to_end(&mut bytes).map_err(broken)?;
            if files.insert(name.clone(), bytes).is_some() {
                problems.push(format!("{name:?}: listed more than once"));
            }
        }
        if problems.len() >= MAX_PROBLEMS {
            break;
        }
    }
    if !files.contains_key(TOKENS_FILE) && problems.is_empty() {
        problems.push(format!("{TOKENS_FILE} is missing"));
    }
    if let Some(bytes) = files.get(TOKENS_FILE)
        && let Err(e) = parse_tokens(bytes)
    {
        problems.push(format!("{TOKENS_FILE}: {e}"));
    }
    if problems.is_empty() {
        Ok(Source { files })
    } else {
        problems.truncate(MAX_PROBLEMS);
        Err(ArchiveError { problems })
    }
}

/// A deterministic `.tar.gz` of `source` (same files → same bytes).
pub fn write(source: &Source) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, bytes) in &source.files {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        // Writing to memory cannot fail, and every path was validated on the way in.
        let _ = builder.append_data(&mut header, path, bytes.as_slice());
    }
    builder
        .into_inner()
        .and_then(GzEncoder::finish)
        .unwrap_or_default()
}

/// An artifact's files: relative path → bytes.
pub type ArtifactFiles = Vec<(String, Vec<u8>)>;

/// A built artifact sent by the builder as an uncompressed tar of the artifact directory
/// (`manifest.json`, `server/**`, `client/**`): regular files at valid artifact paths only.
/// Returns the artifact id named by the manifest and the files; `check_artifact` then checks
/// them against the manifest.
pub fn read_artifact_tar(tar_bytes: &[u8]) -> Result<(String, ArtifactFiles), ArchiveError> {
    let mut archive = tar::Archive::new(tar_bytes);
    let mut files = Vec::new();
    let mut total: u64 = 0;
    let broken = |e: io::Error| ArchiveError::one(format!("the artifact cannot be read: {e}"));
    for entry in archive.entries().map_err(broken)? {
        let mut entry = entry.map_err(broken)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() || kind.is_pax_global_extensions() {
            continue;
        }
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| ArchiveError::one("a path is not valid UTF-8"))?;
        let name = name.strip_prefix("./").unwrap_or(&name).to_owned();
        if !(kind.is_file() || kind.is_contiguous()) || !super::artifact_path_valid(&name) {
            return Err(ArchiveError::one(format!("{name:?}: not an artifact file")));
        }
        total = total.saturating_add(entry.size());
        if total > super::MAX_ARTIFACT_BYTES as u64 || files.len() >= 20_000 {
            return Err(ArchiveError::one("the artifact is larger than 50 MB"));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(broken)?;
        files.push((name, bytes));
    }
    let id = files
        .iter()
        .find(|(p, _)| p == "manifest.json")
        .and_then(|(_, b)| serde_json::from_slice::<Value>(b).ok())
        .and_then(|m| m["id"].as_str().map(str::to_owned))
        .ok_or_else(|| ArchiveError::one("manifest.json with an id is missing"))?;
    Ok((id, files))
}

// ---------------------------------------------------------------------------------------
// Design tokens (`theme.tokens.json`, A6): the same allowlist as theme-kit's `validateTokens`.

static KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]{0,31}$").unwrap_or_else(|_| unreachable!()));
static COLOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(#[0-9a-fA-F]{6}|oklch\(\s*[0-9.]+%?\s+[0-9.]+\s+[0-9.]+\s*\))$")
        .unwrap_or_else(|_| unreachable!())
});
static FONT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[A-Za-z0-9 ,'"-]{1,120}$"#).unwrap_or_else(|_| unreachable!()));
static LENGTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(0|[0-9]{1,3}(\.[0-9]{1,3})?(rem|px))$").unwrap_or_else(|_| unreachable!())
});

/// Validates design tokens: exactly the groups `colors`, `fonts`, `radius` (+ `$schema`), each
/// an object of allowlisted keys and values. Returns the normalized tokens (without `$schema`).
pub fn validate_tokens(input: &Value) -> Result<Value, String> {
    let obj = input.as_object().ok_or("expected an object")?;
    let extra: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !["colors", "fonts", "radius", "$schema"].contains(k))
        .collect();
    if !extra.is_empty() {
        return Err(format!("unknown keys {}", extra.join(", ")));
    }
    let mut out = serde_json::Map::new();
    for (group, value) in [("colors", &*COLOR), ("fonts", &*FONT), ("radius", &*LENGTH)] {
        let g = obj
            .get(group)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{group}: expected an object"))?;
        if g.len() > 64 {
            return Err(format!("{group}: at most 64 entries"));
        }
        for (k, v) in g {
            if !KEY.is_match(k) {
                return Err(format!("{group}: invalid key {k:?}"));
            }
            match v.as_str() {
                Some(s) if value.is_match(s) => {}
                _ => return Err(format!("{group}.{k}: invalid value {v}")),
            }
        }
        out.insert(group.to_owned(), Value::Object(g.clone()));
    }
    Ok(Value::Object(out))
}

/// Parses and validates a `theme.tokens.json` file.
pub fn parse_tokens(bytes: &[u8]) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not JSON: {e}"))?;
    validate_tokens(&v)
}

/// Pretty JSON with a trailing newline, as the default theme stores it.
pub fn tokens_file(tokens: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(tokens).unwrap_or_default();
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;

    fn tokens() -> Vec<u8> {
        tokens_file(&json!({
            "colors": { "buy": "#ff8800", "background": "oklch(0.975 0.003 250)" },
            "fonts": { "sans": "system-ui, sans-serif" },
            "radius": { "md": "0.5rem" }
        }))
    }

    fn source(files: &[(&str, &[u8])]) -> Source {
        Source {
            files: files
                .iter()
                .map(|(p, b)| ((*p).to_owned(), b.to_vec()))
                .collect(),
        }
    }

    /// A raw archive, bypassing the `tar` crate's own path checks (hostile input).
    fn raw(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, body, link) in entries {
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(*kind);
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            let bytes = h.as_mut_bytes();
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(l) = link {
                bytes[157..157 + l.len()].copy_from_slice(l.as_bytes());
            }
            h.set_cksum();
            out.extend_from_slice(h.as_bytes());
            out.extend_from_slice(body);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.resize(out.len() + 1024, 0);
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all(&out).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn round_trip_is_deterministic() {
        let t = tokens();
        let s = source(&[
            (TOKENS_FILE, &t),
            ("src/pages/index.astro", b"---\n---\n<h1>Hi</h1>\n"),
            ("src/pages/c/[...slug].astro", b"x"),
            ("public/favicon.svg", b"<svg/>"),
            ("package.json", b"{}"),
        ]);
        let a = write(&s);
        assert_eq!(a, write(&s));
        assert_eq!(read(&a).unwrap(), s);
    }

    #[test]
    fn rejects_hostile_entries_with_reasons() {
        let t = tokens();
        let bad = raw(&[
            (TOKENS_FILE, tar::EntryType::Regular, &t, None),
            (
                "src/evil",
                tar::EntryType::Symlink,
                b"",
                Some("/etc/passwd"),
            ),
            (
                "src/hard",
                tar::EntryType::Link,
                b"",
                Some("theme.tokens.json"),
            ),
            ("../escape.astro", tar::EntryType::Regular, b"x", None),
            ("src/../../escape", tar::EntryType::Regular, b"x", None),
            ("/etc/cron.d/x", tar::EntryType::Regular, b"x", None),
            (
                "node_modules/astro/index.js",
                tar::EntryType::Regular,
                b"x",
                None,
            ),
            ("src/dev", tar::EntryType::Char, b"", None),
        ]);
        let err = read(&bad).unwrap_err().problems.join("\n");
        for needle in [
            "\"src/evil\": symbolic links are not allowed",
            "\"src/hard\": hard links are not allowed",
            "\"../escape.astro\": '..' is not allowed",
            "\"src/../../escape\": '..' is not allowed",
            "\"/etc/cron.d/x\": absolute paths are not allowed",
            "\"node_modules/astro/index.js\": not part of a theme",
            "\"src/dev\": only regular files are allowed",
        ] {
            assert!(err.contains(needle), "missing {needle:?} in\n{err}");
        }
    }

    #[test]
    fn rejects_oversized_and_bombs() {
        // 51 MB of zeros compresses to ~50 kB: rejected while decompressing.
        let t = tokens();
        let mut big = GzEncoder::new(Vec::new(), Compression::fast());
        let mut h = tar::Header::new_gnu();
        h.set_size(51 * 1024 * 1024);
        h.set_mode(0o644);
        h.set_path("public/big.bin").unwrap();
        h.set_cksum();
        big.write_all(h.as_bytes()).unwrap();
        let zeros = vec![0u8; 1024 * 1024];
        for _ in 0..51 {
            big.write_all(&zeros).unwrap();
        }
        let big = big.finish().unwrap();
        assert!(big.len() < 1024 * 1024);
        let err = read(&big).unwrap_err();
        assert!(err.problems[0].contains("larger than 50 MB"), "{err}");

        let s = source(&[(TOKENS_FILE, &t)]);
        assert!(read(&write(&s)).is_ok());
        assert!(read(b"PK\x03\x04zip").unwrap_err().problems[0].contains("gzip"));
        assert!(
            read(&vec![0x1f; MAX_UPLOAD_BYTES + 1])
                .unwrap_err()
                .problems[0]
                .contains("larger than 20 MB")
        );
    }

    #[test]
    fn requires_valid_tokens() {
        let s = source(&[("src/pages/index.astro", b"x")]);
        assert_eq!(
            read(&write(&s)).unwrap_err().problems,
            vec![format!("{TOKENS_FILE} is missing")]
        );
        let s = source(&[(
            TOKENS_FILE,
            br#"{"colors":{"buy":"url(x)"},"fonts":{},"radius":{}}"#,
        )]);
        assert!(read(&write(&s)).unwrap_err().problems[0].contains("colors.buy: invalid value"));
    }

    #[test]
    fn token_schema() {
        let ok =
            json!({"$schema": "x", "colors": {"a": "#00ff00"}, "fonts": {}, "radius": {"sm": "0"}});
        assert_eq!(
            validate_tokens(&ok).unwrap(),
            json!({"colors": {"a": "#00ff00"}, "fonts": {}, "radius": {"sm": "0"}})
        );
        for bad in [
            json!({"colors": {}, "fonts": {}, "radius": {}, "js": {}}),
            json!({"colors": {"A": "#000000"}, "fonts": {}, "radius": {}}),
            json!({"colors": {"a": "red; background:url(x)"}, "fonts": {}, "radius": {}}),
            json!({"colors": {}, "fonts": {"a": "x;}"}, "radius": {}}),
            json!({"colors": {}, "fonts": {}, "radius": {"a": "1em"}}),
            json!({"colors": {}, "fonts": {}}),
        ] {
            assert!(validate_tokens(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn path_rules() {
        for ok in [
            "src/pages/p/[slug].astro",
            "src/pages/c/[...slug].astro",
            "public/fonts/archivo-latin-ext.woff2",
            "checks/buy-box.spec.ts",
            "README.md",
            "astro.config.mjs",
        ] {
            assert_eq!(path_problem(ok), None, "{ok}");
        }
        for bad in [
            "src//x",
            "src/./x",
            "src/.hidden",
            "dist/index.html",
            ".astro/types.d.ts",
            "src/a b.astro",
            "wrangler.json",
        ] {
            assert!(path_problem(bad).is_some(), "{bad}");
        }
    }
}
