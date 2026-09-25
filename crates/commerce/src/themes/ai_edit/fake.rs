//! The deterministic theme-editing agent of the fake provider (tests, local stacks without
//! `ANTHROPIC_API_KEY`, spec §16 M3 "the fake provider returns a scripted patch"): it lists the
//! pages, reads the home page, adds a note quoting the request plus a functional check for it,
//! runs the checks and reports. Answers are raw Messages API responses, so they travel through
//! the same parser and loop as the real model's.

use serde_json::{Value, json};

/// Where the note goes: right after the opening `<Base …>` tag of the home page.
pub const PAGE: &str = "src/pages/index.astro";
pub const CHECK: &str = "checks/ai-edit.spec.ts";
/// The note's marker attribute (the functional check looks for it).
pub const MARKER: &str = "data-ai-edit";

fn tool(turn: usize, i: usize, name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": format!("toolu_fake_{turn}_{i}"), "name": name, "input": input })
}

fn tools(turn: usize, calls: Vec<(&str, Value)>) -> Value {
    let content: Vec<Value> = std::iter::once(json!({ "type": "text", "text": "Working on it." }))
        .chain(
            calls
                .into_iter()
                .enumerate()
                .map(|(i, (n, input))| tool(turn, i, n, input)),
        )
        .collect();
    json!({ "stop_reason": "tool_use", "content": content })
}

fn done(text: &str) -> Value {
    json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": text }] })
}

/// The merchant's request from the first user message's `<data>` block.
fn request(messages: &[Value]) -> String {
    let text = messages
        .first()
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    text.split_once("<data>\n")
        .and_then(|(_, rest)| rest.rsplit_once("\n</data>"))
        .and_then(|(json, _)| serde_json::from_str::<Value>(json).ok())
        .and_then(|d| d["request"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The first tool result of the last user message.
fn last_result(messages: &[Value]) -> (String, bool) {
    let block = messages
        .last()
        .and_then(|m| m["content"].as_array())
        .and_then(|c| c.iter().find(|b| b["type"] == "tool_result"));
    (
        block
            .and_then(|b| b["content"].as_str())
            .unwrap_or_default()
            .to_owned(),
        block.is_some_and(|b| b["is_error"] == true),
    )
}

/// The note's text: the request, whitespace collapsed, at most 120 characters.
pub fn note_text(request: &str) -> String {
    let text = request.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("AI edit: {}", text.chars().take(120).collect::<String>())
}

/// A JS string literal that is also safe inside an Astro template expression.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('{', "\\u007b")
        .replace('}', "\\u007d")
}

/// Inserts the note after the `<Base …>` line; `None` when the page has no such line.
pub fn patch_page(page: &str, note: &str) -> Option<String> {
    let mut out = String::with_capacity(page.len() + 200);
    let mut done = false;
    for line in page.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.trim_start().starts_with("<Base ") && line.trim_end().ends_with('>') {
            out.push_str(&format!(
                "  <p class=\"container-shop mt-4 text-sm text-muted-foreground\" {MARKER}>{{{}}}</p>\n",
                js_string(note)
            ));
            done = true;
        }
    }
    done.then_some(out)
}

pub fn check_spec(note: &str) -> String {
    format!(
        "import {{ expect, test }} from \"@playwright/test\";\n\n\
         test(\"the AI edit note is on the home page\", async ({{ page }}) => {{\n  \
         await page.goto(\"/\");\n  \
         await expect(page.locator(\"[{MARKER}]\")).toHaveText({});\n\
         }});\n",
        js_string(note)
    )
}

/// The scripted agent: one step per assistant turn so far.
pub fn agent(messages: &[Value]) -> Value {
    let turn = messages.iter().filter(|m| m["role"] == "assistant").count();
    match turn {
        0 => tools(
            turn,
            vec![("list_files", json!({ "prefix": "src/pages/" }))],
        ),
        1 => tools(turn, vec![("read_file", json!({ "path": PAGE }))]),
        2 => {
            let (page, failed) = last_result(messages);
            let note = note_text(&request(messages));
            match patch_page(&page, &note).filter(|_| !failed) {
                Some(patched) => tools(
                    turn,
                    vec![
                        ("write_file", json!({ "path": PAGE, "content": patched })),
                        (
                            "write_file",
                            json!({ "path": CHECK, "content": check_spec(&note) }),
                        ),
                    ],
                ),
                None => done(
                    "The home page has no <Base> layout line to anchor the change; nothing was changed.",
                ),
            }
        }
        3 => tools(turn, vec![("run_checks", json!({}))]),
        _ => {
            let (report, _) = last_result(messages);
            let ready =
                serde_json::from_str::<Value>(&report).is_ok_and(|r| r["status"] == "ready");
            if ready {
                done(
                    "Added a note with your request to the home page and a functional check that finds it. All checks passed.",
                )
            } else {
                done("The checks failed; the demo agent does not repair changes.")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_is_escaped_for_astro_and_ts() {
        let note = note_text("Add {evil} </p><script>x</script>\n  \"quoted\"");
        assert_eq!(
            note,
            "AI edit: Add {evil} </p><script>x</script> \"quoted\""
        );
        let page =
            "---\nconst a = 1;\n---\n\n<Base shop={shop} seo={home.seo}>\n  <h1>x</h1>\n</Base>\n";
        let patched = patch_page(page, &note).unwrap();
        let line = patched.lines().nth(5).unwrap();
        assert!(
            line.starts_with("  <p class=") && line.ends_with("</p>"),
            "{line}"
        );
        assert!(!line.contains("<script") && !line.contains("{evil}"));
        assert_eq!(line.matches('{').count(), 1);
        assert!(patch_page("<h1>no layout</h1>\n", &note).is_none());
        let spec = check_spec(&note);
        assert!(spec.contains("toHaveText(\"AI edit: Add \\u007bevil\\u007d"));
    }

    #[test]
    fn reads_the_request_from_the_data_block() {
        let first = json!({"role": "user", "content": platform::ai::user_content("Do it.", &json!({"request": "a <b> c"}))});
        assert_eq!(request(&[first]), "a <b> c");
    }
}
