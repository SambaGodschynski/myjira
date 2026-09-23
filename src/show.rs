use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::config::Instance;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, adf, color_enabled};

const FIELDS: &str = "summary,assignee,reporter,timetracking,description";
// Fixed rather than read off the terminal: the output is as often piped as read,
// and a width that changes with the window makes two runs incomparable.
const WIDTH: usize = 74;

pub async fn show(instance: &Instance, ticket: &str, opts: &Opts) -> Result<(), Error> {
    let key = normalise_issue_key(ticket)?;
    let session = Session::new(&api::client(), instance.cookie.clone());

    let (issue, comments) = fetch(&session, &instance.url, &key, opts).await?;
    let fields = issue.get("fields");
    let field = |name: &str| fields.and_then(|f| f.get(name));
    let person = |name: &str| {
        field(name)
            .and_then(|p| p.get("displayName"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
    };
    let time = |name: &str| {
        field("timetracking")
            .and_then(|t| t.get(name))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
    };
    let summary = field("summary").and_then(Value::as_str).unwrap_or("");
    let description = field("description").map(adf::to_markdown).unwrap_or_default();

    if let Some(mirror) = &opts.mirror {
        // Plain, not coloured: escape codes are decoration in a terminal and
        // literal rubbish in a file.
        let file = mirror.write_ticket(&key, &rendered(&key, &issue, &comments, false))?;
        eprintln!("mirrored {}", file.display());
    }

    if opts.json {
        let out = json!({
            "key": key,
            "summary": summary,
            "assignee": person("assignee"),
            "reporter": person("reporter"),
            "originalEstimate": time("originalEstimate"),
            "remainingEstimate": time("remainingEstimate"),
            "description": description,
            "comments": comments.iter().map(|comment| json!({
                "author": author(comment),
                "created": comment.get("created").and_then(Value::as_str).unwrap_or(""),
                "body": comment.get("body").map(adf::to_markdown).unwrap_or_default(),
            })).collect::<Vec<Value>>(),
        });
        outln!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    outln!("{}", rendered(&key, &issue, &comments, color_enabled()));
    Ok(())
}

// One ticket, fetched. Separate because `list-tickets --mirror` needs the same
// two calls for every ticket it writes.
async fn fetch(session: &Session, base: &str, key: &str, opts: &Opts) -> Result<(Value, Vec<Value>), Error> {
    if opts.verbose {
        eprintln!("GET {}", issue_url(base, key));
    }
    let issue = match session.get(&issue_url(base, key)).await {
        Ok(issue) => issue,
        Err(e) => {
            // "does not exist or you do not have permission to see it" is what
            // Jira says to an anonymous caller about an issue that is there.
            if api::is_status(&e, 404) && !session.is_authenticated(base).await {
                return Err(api::expired());
            }
            return Err(e);
        }
    };
    let comments = session.get_all_rows(&comments_url(base, key), "comments", opts.verbose).await?;
    Ok((issue, comments))
}

// The document a mirrored file holds, for a ticket `list-tickets` already knows
// the key of. Exactly what `show` writes, so the two can't drift apart.
pub async fn render(session: &Session, base: &str, key: &str, opts: &Opts) -> Result<String, Error> {
    let (issue, comments) = fetch(session, base, key, opts).await?;
    Ok(rendered(key, &issue, &comments, false))
}

fn rendered(key: &str, issue: &Value, comments: &[Value], colour: bool) -> String {
    let fields = issue.get("fields");
    let summary = fields.and_then(|f| f.get("summary")).and_then(Value::as_str).unwrap_or("");
    let description = fields.and_then(|f| f.get("description")).map(adf::to_markdown).unwrap_or_default();
    document(&Ticket { key, summary, description: &description, fields, comments }, colour)
}

// Everything the document is built from, so it can be rendered twice — once
// coloured for the terminal, once plain for a mirrored file — without fetching
// anything twice.
struct Ticket<'a> {
    key: &'a str,
    summary: &'a str,
    description: &'a str,
    fields: Option<&'a Value>,
    comments: &'a [Value],
}

// `colour` rather than the global setting: a file gets the plain spelling even
// while the terminal gets the coloured one.
fn document(ticket: &Ticket, colour: bool) -> String {
    let paint = |code: &'static str| if colour { code } else { "" };
    let field = |name: &str| ticket.fields.and_then(|f| f.get(name));
    let person = |name: &str| {
        field(name)
            .and_then(|p| p.get("displayName"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
    };
    let time = |name: &str| {
        field("timetracking")
            .and_then(|t| t.get(name))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
    };

    let bar = "-".repeat(WIDTH);
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    // Dashes are a horizontal rule in Markdown only with a blank line before
    // them: directly under a line of text they turn that line into a heading
    // instead — the same trap "=====" fell into, one level up.
    let rule = |line: &mut dyn FnMut(String)| {
        line(String::new());
        line(format!("{}{}{}", paint(DIM), bar, paint(RESET)));
        line(String::new());
    };

    // The opening one needs no blank line above it, but it does need one below:
    // a dashed line with text straight under it is how pandoc spells a table,
    // and it swallowed the whole header block into one.
    line(format!("{}{}{}", paint(DIM), bar, paint(RESET)));
    line(String::new());
    // Two trailing spaces: consecutive lines are one paragraph in Markdown, so
    // without a hard break a renderer runs the key, the summary and all four
    // fields together onto a single line. Invisible in a terminal either way.
    line(format!("{}{}{}  ", paint(CYAN), ticket.key, paint(RESET)));
    line(format!("{}{}{}", paint(BOLD), ticket.summary, paint(RESET)));
    line(String::new());
    // Labelled, because two names and two durations in a row are otherwise
    // indistinguishable from each other.
    for (label, value) in [
        ("Assignee", person("assignee")),
        ("Reporter", person("reporter")),
        ("Estimate", time("originalEstimate")),
        ("Remaining", time("remainingEstimate")),
    ] {
        line(format!("{}{label:<10}{} {value}  ", paint(DIM), paint(RESET)));
    }
    rule(&mut line);

    if ticket.description.trim().is_empty() {
        line(format!("{}(no description){}", paint(DIM), paint(RESET)));
    } else {
        line(ticket.description.to_string());
    }

    for comment in ticket.comments {
        rule(&mut line);
        let when = comment.get("created").and_then(Value::as_str).map(stamp).unwrap_or_default();
        line(format!("{}{}{} {}{when}{}", paint(CYAN), author(comment), paint(RESET), paint(DIM), paint(RESET)));
        line(String::new());
        line(comment.get("body").map(adf::to_markdown).unwrap_or_default());
    }

    // Its own paragraph, or a renderer would fold it into the last comment.
    line(String::new());
    out.push('◼');
    out
}

fn issue_url(base: &str, key: &str) -> String {
    format!("{base}/rest/api/3/issue/{key}?fields={FIELDS}")
}

fn comments_url(base: &str, key: &str) -> String {
    format!("{base}/rest/api/3/issue/{key}/comment")
}

fn author(comment: &Value) -> &str {
    comment
        .get("author")
        .and_then(|a| a.get("displayName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("?")
}

// "2021-11-30T12:32:41.067+0100" -> "30.11.21 12:32". Sliced rather than parsed:
// the string is fixed-width up to the minutes, and a timezone-aware conversion
// would only move the displayed time away from what Jira shows.
fn stamp(iso: &str) -> String {
    let (date, time) = match iso.split_once('T') {
        Some((date, time)) => (date, time),
        None => return iso.to_string(),
    };
    let parts: Vec<&str> = date.split('-').collect();
    let minutes: String = time.chars().take(5).collect();
    match parts[..] {
        [year, month, day] if year.len() == 4 => format!("{day}.{month}.{} {minutes}", &year[2..]),
        _ => iso.to_string(),
    }
}

// Jira's own lookup is case-sensitive, and a key typed in lower case is a
// spelling of the same ticket rather than a mistake.
fn normalise_issue_key(raw: &str) -> Result<String, Error> {
    let key = raw.trim();
    let malformed = || Error::from(format!("{key:?} is not a ticket key — those look like BTE-915"));
    // rsplit, because a project key may contain no dash but the separator is the
    // last one either way.
    let Some((project, number)) = key.rsplit_once('-') else { return Err(malformed()) };
    let project_ok = !project.is_empty() && project.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    let number_ok = !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit());
    if !project_ok || !number_ok {
        return Err(malformed());
    }
    Ok(format!("{}-{number}", project.to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_ticket_key_is_upper_cased_and_kept_otherwise() {
        assert_eq!(normalise_issue_key("bte-915").unwrap(), "BTE-915");
        assert_eq!(normalise_issue_key(" BTE-915 ").unwrap(), "BTE-915");
    }

    #[test]
    fn something_that_is_not_a_ticket_key_is_refused() {
        for bad in ["BTE", "915", "BTE-", "-915", "BTE-91a", "BTE 915", "BTE-915/x"] {
            assert!(normalise_issue_key(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn the_urls_are_built_off_the_configured_base() {
        let base = "https://x.atlassian.net";
        assert!(issue_url(base, "BTE-915").starts_with("https://x.atlassian.net/rest/api/3/issue/BTE-915?fields="));
        assert_eq!(comments_url(base, "BTE-915"), "https://x.atlassian.net/rest/api/3/issue/BTE-915/comment");
    }

    #[test]
    fn a_timestamp_is_shortened_to_the_minute() {
        assert_eq!(stamp("2021-11-30T12:32:41.067+0100"), "30.11.21 12:32");
    }

    #[test]
    fn an_unexpected_timestamp_is_passed_through_rather_than_mangled() {
        assert_eq!(stamp("whenever"), "whenever");
    }

    #[test]
    fn a_comment_without_an_author_is_still_attributable_to_something() {
        assert_eq!(author(&json!({"author": {"displayName": " Carolin Grandl"}})), "Carolin Grandl");
        assert_eq!(author(&json!({})), "?");
    }
}
