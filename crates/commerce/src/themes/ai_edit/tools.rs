//! The agent's file tools over an in-memory workspace (spec §12.3, A6). Every input is model
//! output, i.e. untrusted: paths are validated like archive entries and restricted to the
//! theme-owned parts of the contract, contents must be UTF-8 text within size limits. The
//! workspace is a path → bytes map written out as regular files only, so links cannot exist.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::themes::archive::{self, MAX_ENTRIES, MAX_EXPANDED_BYTES, TOKENS_FILE};

/// One file the agent may read or write.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// Entries one `list_files` answer carries.
const MAX_LISTED: usize = 1_000;

/// The tool definitions sent on every turn (stable order and text: part of the cached prefix).
pub fn definitions() -> Value {
    let path = json!({
        "type": "string",
        "description": "Relative path, e.g. src/pages/index.astro"
    });
    json!([
        {
            "name": "list_files",
            "description": "Lists the editable theme files (path and size in bytes) under a directory prefix. Use an empty prefix for everything: src/, public/, checks/ and theme.tokens.json.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": { "prefix": { "type": "string", "description": "Directory prefix such as src/components/, or empty" } },
                "required": ["prefix"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_file",
            "description": "Returns the full text of one theme file. Binary files (fonts, images) cannot be read.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": { "path": path },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "write_file",
            "description": "Creates or replaces one text file with the complete new content (not a patch). Allowed: src/**, public/**, theme.tokens.json, and functional checks at checks/<name>.spec.ts. At most 256 kB.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "content": { "type": "string", "description": "The complete file content" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }
        },
        {
            "name": "delete_file",
            "description": "Deletes one theme file.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": { "path": path },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_checks",
            "description": "Builds the current files in the platform sandbox and runs every gate: contract lint, astro check, build, performance budgets (Lighthouse, JS size, page-model calls), axe, the purchase smoke test and your functional checks in checks/*.spec.ts on a preview of the shop. Takes a few minutes. Returns ready or failed with the reasons. Requires at least one functional check you wrote for this change.",
            "strict": true,
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }
        }
    ])
}

/// What a tool call answers (`tool_result` content) and whether it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub content: String,
    pub is_error: bool,
}

impl Outcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// Whether the agent may touch `path`: a valid archive path under `src/` or `public/`, the
/// tokens file, or a functional check `checks/<name>.spec.ts`.
pub fn check_path(path: &str) -> Result<(), String> {
    if let Some(problem) = archive::path_problem(path) {
        return Err(problem);
    }
    let allowed = path == TOKENS_FILE
        || path.starts_with("src/")
        || path.starts_with("public/")
        || path.strip_prefix("checks/").is_some_and(|name| {
            !name.contains('/') && name.len() > 8 && name.ends_with(".spec.ts")
        });
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "{path:?}: only src/**, public/**, theme.tokens.json and checks/<name>.spec.ts can be used"
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    prefix: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}

fn parse<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, Outcome> {
    serde_json::from_value(input.clone()).map_err(|e| Outcome::err(format!("invalid input: {e}")))
}

/// Runs a file tool on `files`. `run_checks` is not a file tool (the loop runs it).
pub fn run(files: &mut BTreeMap<String, Vec<u8>>, name: &str, input: &Value) -> Outcome {
    let result = match name {
        "list_files" => parse::<ListInput>(input).map(|i| list(files, &i.prefix)),
        "read_file" => parse::<PathInput>(input).map(|i| read(files, &i.path)),
        "write_file" => parse::<WriteInput>(input).map(|i| write(files, &i.path, i.content)),
        "delete_file" => parse::<PathInput>(input).map(|i| delete(files, &i.path)),
        other => Err(Outcome::err(format!("unknown tool {other:?}"))),
    };
    result.unwrap_or_else(|e| e)
}

fn list(files: &BTreeMap<String, Vec<u8>>, prefix: &str) -> Outcome {
    let entries: Vec<String> = files
        .iter()
        .filter(|(p, _)| p.starts_with(prefix) && check_path(p).is_ok())
        .map(|(p, b)| format!("{p}\t{}", b.len()))
        .collect();
    if entries.is_empty() {
        return Outcome::ok(format!("no files under {prefix:?}"));
    }
    let more = entries.len().saturating_sub(MAX_LISTED);
    let mut out = entries
        .into_iter()
        .take(MAX_LISTED)
        .collect::<Vec<_>>()
        .join("\n");
    if more > 0 {
        out.push_str(&format!("\n… {more} more; use a narrower prefix"));
    }
    Outcome::ok(out)
}

fn read(files: &BTreeMap<String, Vec<u8>>, path: &str) -> Outcome {
    if let Err(e) = check_path(path) {
        return Outcome::err(e);
    }
    let Some(bytes) = files.get(path) else {
        return Outcome::err(format!("{path}: no such file"));
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Outcome::err(format!(
            "{path}: {} bytes, larger than the 256 kB limit",
            bytes.len()
        ));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Outcome::ok(text),
        _ => Outcome::err(format!(
            "{path}: binary file ({} bytes), cannot be read",
            bytes.len()
        )),
    }
}

fn write(files: &mut BTreeMap<String, Vec<u8>>, path: &str, content: String) -> Outcome {
    if let Err(e) = check_path(path) {
        return Outcome::err(e);
    }
    if content.len() > MAX_FILE_BYTES {
        return Outcome::err(format!(
            "{path}: {} bytes, larger than the 256 kB limit",
            content.len()
        ));
    }
    if content.contains('\0') {
        return Outcome::err(format!("{path}: binary content is not allowed"));
    }
    if path == TOKENS_FILE
        && let Err(e) = archive::parse_tokens(content.as_bytes())
    {
        return Outcome::err(format!("{TOKENS_FILE}: {e}"));
    }
    // A path cannot be both a file and a directory.
    let dir = format!("{path}/");
    if files.keys().any(|p| p.starts_with(&dir)) {
        return Outcome::err(format!("{path}: is a directory"));
    }
    if let Some(parent) = path
        .match_indices('/')
        .map(|(i, _)| &path[..i])
        .find(|p| files.contains_key(*p))
    {
        return Outcome::err(format!("{path}: {parent} is a file"));
    }
    let old = files.get(path).map_or(0, Vec::len) as u64;
    let total: u64 =
        files.values().map(|b| b.len() as u64).sum::<u64>() - old + content.len() as u64;
    if total > MAX_EXPANDED_BYTES {
        return Outcome::err("the theme would exceed 50 MB");
    }
    if !files.contains_key(path) && files.len() >= MAX_ENTRIES {
        return Outcome::err(format!("the theme would exceed {MAX_ENTRIES} files"));
    }
    let bytes = content.into_bytes();
    let n = bytes.len();
    let created = files.insert(path.to_owned(), bytes).is_none();
    Outcome::ok(format!(
        "{} {path} ({n} bytes)",
        if created { "created" } else { "wrote" }
    ))
}

fn delete(files: &mut BTreeMap<String, Vec<u8>>, path: &str) -> Outcome {
    if let Err(e) = check_path(path) {
        return Outcome::err(e);
    }
    match files.remove(path) {
        Some(_) => Outcome::ok(format!("deleted {path}")),
        None => Outcome::err(format!("{path}: no such file")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "src/pages/index.astro".to_owned(),
                b"<h1>Hi</h1>\n".to_vec(),
            ),
            (
                "src/fonts/a.woff2".to_owned(),
                vec![0x77, 0x4f, 0x46, 0x32, 0, 0xff],
            ),
            ("package.json".to_owned(), b"{}".to_vec()),
            ("README.md".to_owned(), b"# x".to_vec()),
        ])
    }

    fn call(files: &mut BTreeMap<String, Vec<u8>>, name: &str, input: Value) -> Outcome {
        run(files, name, &input)
    }

    #[test]
    fn paths_stay_inside_the_theme_contract() {
        for ok in [
            "src/pages/index.astro",
            "public/robots.txt",
            "theme.tokens.json",
            "checks/size-guide.spec.ts",
        ] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "../etc/passwd",
            "src/../package.json",
            "/src/a.astro",
            "src\\a.astro",
            "src/./a.astro",
            "src//a.astro",
            "package.json",
            "astro.config.mjs",
            "README.md",
            "checks/a.ts",
            "checks/.spec.ts",
            "checks/sub/a.spec.ts",
            "node_modules/x/index.js",
            "srcx/a.astro",
            "",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_text_only_within_scope() {
        let mut f = ws();
        let r = call(
            &mut f,
            "read_file",
            json!({"path": "src/pages/index.astro"}),
        );
        assert_eq!(r, Outcome::ok("<h1>Hi</h1>\n"));
        assert!(
            call(&mut f, "read_file", json!({"path": "src/fonts/a.woff2"}))
                .content
                .contains("binary")
        );
        assert!(call(&mut f, "read_file", json!({"path": "package.json"})).is_error);
        assert!(call(&mut f, "read_file", json!({"path": "src/nope.astro"})).is_error);
        assert!(
            call(
                &mut f,
                "read_file",
                json!({"path": "src/pages/index.astro", "x": 1})
            )
            .is_error
        );
        assert!(call(&mut f, "read_file", json!({})).is_error);
        f.insert("src/big.txt".into(), vec![b'a'; MAX_FILE_BYTES + 1]);
        assert!(call(&mut f, "read_file", json!({"path": "src/big.txt"})).is_error);
    }

    #[test]
    fn lists_only_editable_files() {
        let mut f = ws();
        let r = call(&mut f, "list_files", json!({"prefix": ""}));
        assert_eq!(r.content, "src/fonts/a.woff2\t6\nsrc/pages/index.astro\t12");
        assert!(!call(&mut f, "list_files", json!({"prefix": "public/"})).is_error);
    }

    #[test]
    fn writes_are_validated() {
        let mut f = ws();
        let r = call(
            &mut f,
            "write_file",
            json!({"path": "src/components/Note.astro", "content": "<p>x</p>\n"}),
        );
        assert_eq!(
            r,
            Outcome::ok("created src/components/Note.astro (9 bytes)")
        );
        assert!(
            !call(
                &mut f,
                "write_file",
                json!({"path": "checks/note.spec.ts", "content": "test"})
            )
            .is_error
        );
        for (path, content) in [
            ("package.json", "{}"),
            ("../x", "x"),
            ("src/pages/index.astro/x", "x"),
            ("src/pages", "x"),
            ("src/a.astro", "a\0b"),
            (
                "theme.tokens.json",
                "{\"colors\": {\"x\": \"url(//evil)\"}}",
            ),
            ("theme.tokens.json", "nope"),
        ] {
            let r = call(
                &mut f,
                "write_file",
                json!({"path": path, "content": content}),
            );
            assert!(r.is_error, "{path}: {r:?}");
        }
        let big = "a".repeat(MAX_FILE_BYTES + 1);
        assert!(
            call(
                &mut f,
                "write_file",
                json!({"path": "src/big.txt", "content": big})
            )
            .is_error
        );
        assert!(call(&mut f, "write_file", json!({"path": "src/x.astro"})).is_error);
        assert_eq!(f.get("package.json").map(Vec::as_slice), Some(&b"{}"[..]));
    }

    #[test]
    fn deletes_within_scope() {
        let mut f = ws();
        assert!(
            !call(
                &mut f,
                "delete_file",
                json!({"path": "src/pages/index.astro"})
            )
            .is_error
        );
        assert!(!f.contains_key("src/pages/index.astro"));
        assert!(call(&mut f, "delete_file", json!({"path": "package.json"})).is_error);
        assert!(call(&mut f, "delete_file", json!({"path": "src/none"})).is_error);
        assert!(call(&mut f, "rm_rf", json!({})).is_error);
    }
}
