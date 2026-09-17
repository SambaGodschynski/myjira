use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::config::Instance;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, adf, c};

const FIELDS: &str = "summary,assignee,reporter,timetracking,description";
// Fixed rather than read off the terminal: the output is as often piped as read,
// and a width that changes with the window makes two runs incomparable.
const WIDTH: usize = 74;

pub async fn show(instance: &Instance, ticket: &str, opts: &Opts) -> Result<(), Error> {
    let key = normalise_issue_key(ticket)?;
    let session = Session::new(&api::client(), instance.cookie.clone());

    if opts.verbose {
        eprintln!("GET {}", issue_url(&instance.url, &key));
    }
    let issue = session.get(&issue_url(&instance.url, &key)).await?;
    let comments = session
        .get_all_rows(&comments_url(&instance.url, &key), "comments", opts.verbose)
        .await?;

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

    let rule = |ch: char| outln!("{}{}{}", c(DIM), ch.to_string().repeat(WIDTH), c(RESET));

    rule('=');
    outln!("{}{}{}", c(CYAN), key, c(RESET));
    outln!("{}{}{}", c(BOLD), summary, c(RESET));
    outln!("");
    // Labelled, because two names and two durations in a row are otherwise
    // indistinguishable from each other.
    for (label, value) in [
        ("Assignee", person("assignee")),
        ("Reporter", person("reporter")),
        ("Estimate", time("originalEstimate")),
        ("Remaining", time("remainingEstimate")),
    ] {
        outln!("{}{:<10}{} {}", c(DIM), label, c(RESET), value);
    }
    rule('=');

    if description.trim().is_empty() {
        outln!("{}(no description){}", c(DIM), c(RESET));
    } else {
        outln!("{description}");
    }

    for comment in &comments {
        rule('-');
        let stamp = comment.get("created").and_then(Value::as_str).map(stamp).unwrap_or_default();
        outln!("{}{}{} {}{}{}", c(CYAN), author(comment), c(RESET), c(DIM), stamp, c(RESET));
        outln!("");
        let body = comment.get("body").map(adf::to_markdown).unwrap_or_default();
        outln!("{body}");
    }

    outln!("◼");
    Ok(())
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
