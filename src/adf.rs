//! Atlassian Document Format to Markdown.
//!
//! Jira Cloud keeps rich text as ADF: a JSON tree, not HTML. Rendering it means
//! walking that tree, which is both simpler and more reliable than asking for
//! `renderedFields` and parsing the HTML back apart.
//!
//! Text is deliberately not escaped. The output is read in a terminal, where a
//! backslash in front of every `*` and `_` costs more than the rare sentence it
//! protects. Nodes ADF has and Markdown hasn't — underline, text colour,
//! sub/superscript — pass their text through unmarked rather than being dropped.

use serde_json::Value;

pub fn to_markdown(doc: &Value) -> String {
    render_blocks(children(doc)).join("\n\n").trim().to_string()
}

fn children(node: &Value) -> &[Value] {
    node.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn kind(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

fn attr(node: &Value, name: &str) -> Option<String> {
    node.get("attrs")?.get(name)?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn render_blocks(nodes: &[Value]) -> Vec<String> {
    nodes.iter().filter_map(render_block).collect()
}

fn render_block(node: &Value) -> Option<String> {
    let block = match kind(node) {
        // An empty paragraph is spacing in the editor, and a blank block here
        // would come out as a stray gap.
        "paragraph" => non_empty(inline(children(node)))?,
        "heading" => {
            let level = node
                .get("attrs")
                .and_then(|a| a.get("level"))
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(level), inline(children(node)))
        }
        "bulletList" => list(node, None),
        "orderedList" => {
            let start = node.get("attrs").and_then(|a| a.get("order")).and_then(Value::as_u64).unwrap_or(1);
            list(node, Some(start))
        }
        "taskList" => task_list(node),
        "codeBlock" => {
            let language = attr(node, "language").unwrap_or_default();
            format!("```{language}\n{}\n```", inline(children(node)))
        }
        "blockquote" => quote(&render_blocks(children(node)).join("\n\n"), ""),
        // Markdown has no panels. A labelled blockquote keeps both the kind and
        // the boundary visible without inventing syntax.
        "panel" => {
            let label = attr(node, "panelType").unwrap_or_else(|| "note".to_string());
            quote(&render_blocks(children(node)).join("\n\n"), &format!("**{label}**"))
        }
        "expand" | "nestedExpand" => {
            let title = attr(node, "title").unwrap_or_else(|| "details".to_string());
            quote(&render_blocks(children(node)).join("\n\n"), &format!("**{title}**"))
        }
        "rule" => "---".to_string(),
        "table" => non_empty(table(node))?,
        // Wrappers around a media node; the node itself carries the file.
        "doc" | "mediaSingle" | "mediaGroup" => non_empty(render_blocks(children(node)).join("\n\n"))?,
        "media" => {
            let name = attr(node, "alt").or_else(|| attr(node, "id")).unwrap_or_default();
            format!("[Attachment: {name}]")
        }
        // Not a block ADF knows, so whatever text it holds is better than
        // dropping it silently.
        _ => non_empty(inline(children(node)))?,
    };
    Some(block)
}

fn non_empty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

// Prefixes every line with "> ", optionally under a label line. A nested
// blockquote is already prefixed, so this just adds another level.
fn quote(body: &str, label: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !label.is_empty() {
        lines.push(format!("> {label}"));
    }
    for line in body.lines() {
        lines.push(if line.is_empty() { ">".to_string() } else { format!("> {line}") });
    }
    lines.join("\n")
}

// `start` is Some for an ordered list, and is where its numbering begins.
// Nested lists need no special handling: a nested list is one of its item's
// blocks, so it arrives here as part of the item body and gets indented with it.
fn list(node: &Value, start: Option<u64>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (index, item) in children(node).iter().enumerate() {
        let marker = match start {
            Some(start) => format!("{}. ", start + index as u64),
            None => "- ".to_string(),
        };
        push_item(&mut lines, &marker, &render_blocks(children(item)).join("\n\n"));
    }
    lines.join("\n")
}

fn task_list(node: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for item in children(node) {
        let done = attr(item, "state").as_deref() == Some("DONE");
        let marker = if done { "- [x] " } else { "- [ ] " };
        let body = match kind(item) {
            // A taskItem holds its text inline rather than in blocks.
            "taskItem" => inline(children(item)),
            _ => render_blocks(children(item)).join("\n\n"),
        };
        push_item(&mut lines, marker, &body);
    }
    lines.join("\n")
}

// The first line carries the marker; the rest is indented to line up under it,
// which is what keeps a nested list nested and a second paragraph inside its
// item.
fn push_item(lines: &mut Vec<String>, marker: &str, body: &str) {
    let padding = " ".repeat(marker.chars().count());
    let mut body_lines = body.lines();
    lines.push(format!("{marker}{}", body_lines.next().unwrap_or("")));
    for line in body_lines {
        lines.push(if line.is_empty() { String::new() } else { format!("{padding}{line}") });
    }
}

fn table(node: &Value) -> String {
    let rows: Vec<Vec<String>> = children(node)
        .iter()
        .filter(|row| kind(row) == "tableRow")
        .map(|row| {
            children(row)
                .iter()
                .map(|cell| {
                    // A cell holds blocks, but a Markdown cell is one line, and
                    // an unescaped pipe would end the cell early.
                    render_blocks(children(cell)).join(" ").replace('\n', " ").replace('|', "\\|")
                })
                .collect()
        })
        .collect();
    let Some(first) = rows.first() else { return String::new() };

    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let row_line = |cells: &[String]| {
        let mut cells = cells.to_vec();
        cells.resize(columns, String::new());
        format!("| {} |", cells.join(" | "))
    };

    // Markdown has no table without a header, so the first row serves as one
    // whether ADF marked its cells as headers or not.
    let mut lines = vec![row_line(first), format!("| {} |", vec!["---"; columns].join(" | "))];
    lines.extend(rows[1..].iter().map(|row| row_line(row)));
    lines.join("\n")
}

fn inline(nodes: &[Value]) -> String {
    nodes.iter().map(inline_node).collect()
}

fn inline_node(node: &Value) -> String {
    match kind(node) {
        "text" => marked(node.get("text").and_then(Value::as_str).unwrap_or(""), node.get("marks")),
        // Two trailing spaces is Markdown's hard break, and invisible in a terminal.
        "hardBreak" => "  \n".to_string(),
        "emoji" => attr(node, "text").or_else(|| attr(node, "shortName")).unwrap_or_default(),
        "mention" => attr(node, "text")
            .or_else(|| attr(node, "id").map(|id| format!("@{id}")))
            .unwrap_or_default(),
        // A lozenge, which reads as a bracketed label.
        "status" => attr(node, "text").map(|text| format!("[{text}]")).unwrap_or_default(),
        "inlineCard" | "blockCard" | "embedCard" => {
            attr(node, "url").map(|url| format!("<{url}>")).unwrap_or_default()
        }
        "date" => attr(node, "timestamp")
            .and_then(|ts| ts.parse::<i64>().ok())
            .map(|ms| date_from_epoch_ms(ms))
            .unwrap_or_default(),
        _ => {
            let nested = inline(children(node));
            if nested.is_empty() {
                node.get("text").and_then(Value::as_str).unwrap_or("").to_string()
            } else {
                nested
            }
        }
    }
}

fn marked(text: &str, marks: Option<&Value>) -> String {
    let Some(marks) = marks.and_then(Value::as_array) else { return text.to_string() };
    // Wrapping nothing in markers would emit "****" and read as literal text.
    if text.is_empty() {
        return String::new();
    }
    let has = |name: &str| marks.iter().any(|m| m.get("type").and_then(Value::as_str) == Some(name));

    // Innermost first. Code is closest to the text because what's inside a code
    // span is literal, so no emphasis markers may end up in there; a link wraps
    // everything, so it goes last.
    let mut out = text.to_string();
    if has("code") {
        out = format!("`{out}`");
    }
    if has("strong") {
        out = format!("**{out}**");
    }
    if has("em") {
        out = format!("*{out}*");
    }
    if has("strike") {
        out = format!("~~{out}~~");
    }
    let href = marks
        .iter()
        .find(|m| m.get("type").and_then(Value::as_str) == Some("link"))
        .and_then(|m| m.get("attrs"))
        .and_then(|a| a.get("href"))
        .and_then(Value::as_str);
    if let Some(href) = href {
        out = format!("[{out}]({href})");
    }
    out
}

// An inline date chip carries epoch milliseconds, and a raw millisecond count
// in the middle of a sentence reads as a bug. Converted here rather than with a
// date library, since this is the only place the tool needs civil dates.
fn date_from_epoch_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil_from_days, with the era starting at 2000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{day:02}.{month:02}.{:02}", year.rem_euclid(100))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(content: Value) -> Value {
        json!({"type": "doc", "version": 1, "content": content})
    }

    fn text(t: &str) -> Value {
        json!({"type": "text", "text": t})
    }

    fn paragraph(content: Value) -> Value {
        json!({"type": "paragraph", "content": content})
    }

    #[test]
    fn paragraphs_are_separated_by_a_blank_line() {
        let d = doc(json!([paragraph(json!([text("one")])), paragraph(json!([text("two")]))]));
        assert_eq!(to_markdown(&d), "one\n\ntwo");
    }

    // The editor uses empty paragraphs for spacing; carrying them over would
    // leave stray gaps.
    #[test]
    fn empty_paragraphs_are_dropped() {
        let d = doc(json!([paragraph(json!([text("one")])), json!({"type": "paragraph"})]));
        assert_eq!(to_markdown(&d), "one");
    }

    #[test]
    fn a_heading_becomes_hashes_at_its_level() {
        let d = doc(json!([json!({"type": "heading", "attrs": {"level": 2}, "content": [text("ACs")]})]));
        assert_eq!(to_markdown(&d), "## ACs");
    }

    #[test]
    fn a_heading_level_beyond_markdown_is_clamped() {
        let d = doc(json!([json!({"type": "heading", "attrs": {"level": 9}, "content": [text("deep")]})]));
        assert_eq!(to_markdown(&d), "###### deep");
    }

    #[test]
    fn marks_nest_from_code_outwards_with_the_link_last() {
        let d = doc(json!([paragraph(json!([json!({
            "type": "text", "text": "x",
            "marks": [{"type": "link", "attrs": {"href": "https://e.com"}}, {"type": "strong"}, {"type": "code"}]
        })]))]));
        assert_eq!(to_markdown(&d), "[**`x`**](https://e.com)");
    }

    // Emphasis markers around nothing would come out as literal asterisks.
    #[test]
    fn marks_on_empty_text_produce_nothing() {
        let d = doc(json!([paragraph(json!([
            json!({"type": "text", "text": "", "marks": [{"type": "strong"}]}),
            text("kept")
        ]))]));
        assert_eq!(to_markdown(&d), "kept");
    }

    // ADF has these, Markdown doesn't; the text must survive anyway.
    #[test]
    fn marks_without_a_markdown_spelling_pass_their_text_through() {
        let d = doc(json!([paragraph(json!([json!({
            "type": "text", "text": "coloured",
            "marks": [{"type": "textColor", "attrs": {"color": "#ff0000"}}, {"type": "underline"}]
        })]))]));
        assert_eq!(to_markdown(&d), "coloured");
    }

    #[test]
    fn an_ordered_list_starts_where_adf_says_it_does() {
        let d = doc(json!([json!({
            "type": "orderedList", "attrs": {"order": 3},
            "content": [
                {"type": "listItem", "content": [paragraph(json!([text("a")]))]},
                {"type": "listItem", "content": [paragraph(json!([text("b")]))]}
            ]
        })]));
        assert_eq!(to_markdown(&d), "3. a\n4. b");
    }

    #[test]
    fn a_nested_list_is_indented_under_its_item() {
        let d = doc(json!([json!({
            "type": "bulletList",
            "content": [{"type": "listItem", "content": [
                paragraph(json!([text("outer")])),
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [paragraph(json!([text("inner")]))]}
                ]}
            ]}]
        })]));
        assert_eq!(to_markdown(&d), "- outer\n\n  - inner");
    }

    #[test]
    fn a_task_list_becomes_checkboxes() {
        let d = doc(json!([json!({
            "type": "taskList",
            "content": [
                {"type": "taskItem", "attrs": {"state": "DONE"}, "content": [text("done")]},
                {"type": "taskItem", "attrs": {"state": "TODO"}, "content": [text("open")]}
            ]
        })]));
        assert_eq!(to_markdown(&d), "- [x] done\n- [ ] open");
    }

    #[test]
    fn a_panel_becomes_a_labelled_blockquote() {
        let d = doc(json!([json!({
            "type": "panel", "attrs": {"panelType": "info"},
            "content": [paragraph(json!([text("mind this")]))]
        })]));
        assert_eq!(to_markdown(&d), "> **info**\n> mind this");
    }

    #[test]
    fn a_code_block_keeps_its_language() {
        let d = doc(json!([json!({
            "type": "codeBlock", "attrs": {"language": "sql"},
            "content": [text("select 1")]
        })]));
        assert_eq!(to_markdown(&d), "```sql\nselect 1\n```");
    }

    #[test]
    fn a_table_gets_the_header_separator_markdown_requires() {
        let cell = |t: &str| json!({"type": "tableCell", "content": [paragraph(json!([text(t)]))]});
        let header = |t: &str| json!({"type": "tableHeader", "content": [paragraph(json!([text(t)]))]});
        let d = doc(json!([json!({
            "type": "table",
            "content": [
                {"type": "tableRow", "content": [header("A"), header("B")]},
                {"type": "tableRow", "content": [cell("1"), cell("2")]}
            ]
        })]));
        assert_eq!(to_markdown(&d), "| A | B |\n| --- | --- |\n| 1 | 2 |");
    }

    // A pipe inside a cell would otherwise end the cell early.
    #[test]
    fn a_pipe_inside_a_cell_is_escaped() {
        let d = doc(json!([json!({
            "type": "table",
            "content": [{"type": "tableRow", "content": [
                {"type": "tableCell", "content": [paragraph(json!([text("a|b")]))]}
            ]}]
        })]));
        assert!(to_markdown(&d).contains(r"a\|b"), "got: {}", to_markdown(&d));
    }

    #[test]
    fn a_short_row_is_padded_so_the_table_stays_rectangular() {
        let cell = |t: &str| json!({"type": "tableCell", "content": [paragraph(json!([text(t)]))]});
        let d = doc(json!([json!({
            "type": "table",
            "content": [
                {"type": "tableRow", "content": [cell("A"), cell("B")]},
                {"type": "tableRow", "content": [cell("1")]}
            ]
        })]));
        assert_eq!(to_markdown(&d), "| A | B |\n| --- | --- |\n| 1 |  |");
    }

    #[test]
    fn the_inline_nodes_jira_sprinkles_around_are_rendered_readably() {
        let d = doc(json!([paragraph(json!([
            json!({"type": "status", "attrs": {"text": "OK", "color": "neutral"}}),
            text(" "),
            json!({"type": "emoji", "attrs": {"shortName": ":smile:", "text": "😄"}}),
            text(" "),
            json!({"type": "mention", "attrs": {"id": "557058", "text": "@Ada"}}),
            text(" "),
            json!({"type": "inlineCard", "attrs": {"url": "https://x.atlassian.net/browse/AKKP-238"}})
        ]))]));
        assert_eq!(to_markdown(&d), "[OK] 😄 @Ada <https://x.atlassian.net/browse/AKKP-238>");
    }

    // An emoji with no unicode text falls back to the name, which still reads.
    #[test]
    fn an_emoji_without_a_character_keeps_its_short_name() {
        let d = doc(json!([paragraph(json!([
            json!({"type": "emoji", "attrs": {"shortName": ":logo_atlassian:"}})
        ]))]));
        assert_eq!(to_markdown(&d), ":logo_atlassian:");
    }

    #[test]
    fn an_attachment_is_named_by_its_filename() {
        let d = doc(json!([json!({
            "type": "mediaSingle", "attrs": {"layout": "center"},
            "content": [{"type": "media", "attrs": {"type": "file", "id": "cbb1", "alt": "shot.png"}}]
        })]));
        assert_eq!(to_markdown(&d), "[Attachment: shot.png]");
    }

    // Without an alt there is still an id, which at least identifies it.
    #[test]
    fn an_attachment_without_a_filename_falls_back_to_its_id() {
        let d = doc(json!([json!({"type": "media", "attrs": {"id": "cbb1"}})]));
        assert_eq!(to_markdown(&d), "[Attachment: cbb1]");
    }

    #[test]
    fn a_hard_break_becomes_a_markdown_line_break() {
        let d = doc(json!([paragraph(json!([
            text("one"),
            json!({"type": "hardBreak"}),
            text("two")
        ]))]));
        assert_eq!(to_markdown(&d), "one  \ntwo");
    }

    // Nothing is more likely to change than ADF's node list, so an unknown node
    // must not swallow the sentence inside it.
    #[test]
    fn an_unknown_node_keeps_the_text_it_contains() {
        let d = doc(json!([paragraph(json!([
            json!({"type": "someFutureThing", "content": [text("still here")]})
        ]))]));
        assert_eq!(to_markdown(&d), "still here");
    }

    #[test]
    fn an_absent_or_empty_document_renders_as_nothing() {
        assert_eq!(to_markdown(&Value::Null), "");
        assert_eq!(to_markdown(&doc(json!([]))), "");
    }

    #[test]
    fn an_inline_date_is_a_date_rather_than_a_millisecond_count() {
        assert_eq!(date_from_epoch_ms(1_638_230_400_000), "30.11.21");
        assert_eq!(date_from_epoch_ms(0), "01.01.70");
        // Leap days, including the century one the usual shortcut gets wrong,
        // and the last day before a century turns over.
        assert_eq!(date_from_epoch_ms(1_582_934_400_000), "29.02.20");
        assert_eq!(date_from_epoch_ms(951_782_400_000), "29.02.00");
        assert_eq!(date_from_epoch_ms(946_598_400_000), "31.12.99");
    }
}
