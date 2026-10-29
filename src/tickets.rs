use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::boards::{self, Board};
use crate::config::Instance;
use crate::mirror::Mirror;
use crate::projects;
use crate::show;
use crate::sprints::BACKLOG;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, YELLOW, c};

// Only what the table and the JSON show. Jira returns every field otherwise,
// which for a few thousand issues is a lot of response to read and discard.
const FIELDS: &str = "summary,status,issuetype,assignee,parent";
// How often a long mirror says where it's got to. Two calls per ticket means a
// large project takes minutes, and silence for minutes reads as a hang.
const PROGRESS_EVERY: usize = 25;
// Caps for the two columns that have no natural width. A summary runs to any
// length, and this Jira has statuses like "Ready for Estimation/Planning" —
// 29 characters that push everything after them off a small screen.
const MAX_SUMMARY: usize = 60;
// The status as a code of this many characters. Four rather than two because
// two collide: of the twenty statuses in this Jira, "In Rewriting" and
// "In Refinement" both reduce to IR, and six of them begin with "Ready for".
// At four, twenty-seven statuses — the real twenty plus seven invented to
// strain the rule — all come out distinct.
const STATUS_CODE: usize = 4;

// What was asked for: the whole project, one sprint, or the backlog. The
// backlog isn't a sprint and can't be expressed in JQL — `sprint IS EMPTY`
// answers 8 for BUA where the board's own backlog holds 5 — so it comes from
// the boards instead.
enum Scope {
    Project,
    Sprint(u64),
    Backlog,
}

struct Ticket {
    key: String,
    parent: Option<String>,
    kind: String,
    status: String,
    status_category: String,
    assignee: Option<String>,
    summary: String,
}

pub async fn list(instance: &Instance, project: &str, sprint: Option<&str>, opts: &Opts) -> Result<(), Error> {
    let key = projects::normalise_key(project)?;
    let scope = match sprint {
        None => Scope::Project,
        Some(raw) if raw.trim().eq_ignore_ascii_case(BACKLOG) => Scope::Backlog,
        Some(raw) => Scope::Sprint(raw.trim().parse::<u64>().map_err(|_| {
            format!("a sprint id is a number or \"{BACKLOG}\", got {raw:?} — myjira list-sprints {key} lists them")
        })?),
    };
    let session = Session::new(&api::client(), instance.cookie.clone());

    // The search endpoint never objects: an unknown project, an unknown sprint
    // and even syntactically broken JQL all come back as 200 with an empty list.
    // So a typo would read as "this project has no tickets". These two calls
    // exist purely so it reads as the typo it is; their answers go unused.
    if opts.verbose {
        eprintln!("checking that {key} exists");
    }
    if let Err(e) = session.get(&project_url(&instance.url, &key)).await {
        // A project an anonymous caller may not see is a 404, not a 401, so
        // "no such project" is what a dead cookie says about a project that is
        // plainly there.
        if api::is_status(&e, 404) && !session.is_authenticated(&instance.url).await {
            return Err(api::expired());
        }
        return Err(e);
    }
    if let Scope::Sprint(id) = scope {
        if opts.verbose {
            eprintln!("checking that sprint {id} exists");
        }
        session
            .get(&sprint_url(&instance.url, id))
            .await
            .map_err(|e| format!("sprint {id}: {e}"))?;
    }


    let mut tickets = match scope {
        Scope::Backlog => backlog(&session, &instance.url, &key, opts).await?,
        Scope::Project => search(&session, &instance.url, &key, None, opts).await?,
        Scope::Sprint(id) => search(&session, &instance.url, &key, Some(id), opts).await?,
    };
    // Sorted here rather than left to whatever produced the list. JQL's
    // ORDER BY key already does this, but the backlog comes from the agile
    // endpoint in board-rank order and was collected by key, which put GDE-102
    // ahead of GDE-11.
    tickets.sort_by(|a, b| number(&a.key).cmp(&number(&b.key)));

    if opts.json {
        let out: Vec<Value> = tickets
            .iter()
            .map(|t| {
                json!({"key": t.key, "parent": t.parent, "type": t.kind, "status": t.status,
                       "statusCategory": t.status_category, "assignee": t.assignee,
                       "summary": t.summary})
            })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&out)?);
    } else if tickets.is_empty() {
        match scope {
            Scope::Sprint(id) => eprintln!("no tickets in {key} sprint {id}"),
            Scope::Backlog => eprintln!("no tickets in the {key} backlog"),
            Scope::Project => eprintln!("no tickets in {key}"),
        }
    } else {
        print_table(&tickets);
    }

    // After the listing is on screen, not before: the download is the slow part
    // and there's no reason to make the list wait behind it.
    if let Some(mirror) = &opts.mirror {
        mirror_all(&session, &instance.url, &tickets, mirror, opts).await?;
    }
    Ok(())
}

// Each ticket costs two calls — the issue and its comments — which is why this
// only happens when it's asked for. A ticket that can't be read is reported and
// skipped: one of those is no reason to abandon the rest of a long run.
async fn mirror_all(
    session: &Session,
    base: &str,
    tickets: &[Ticket],
    mirror: &Mirror,
    opts: &Opts,
) -> Result<(), Error> {
    let total = tickets.len();
    let mut written = 0usize;
    let mut failed = 0usize;

    for (index, ticket) in tickets.iter().enumerate() {
        match show::render(session, base, &ticket.key, opts).await {
            Ok(document) => {
                let file = mirror.write_ticket(&ticket.key, &document)?;
                written += 1;
                if opts.verbose {
                    eprintln!("mirrored {}", file.display());
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", ticket.key);
            }
        }
        if !opts.verbose && (index + 1) % PROGRESS_EVERY == 0 && index + 1 < total {
            eprintln!("mirrored {}/{total} tickets...", index + 1);
        }
    }

    match failed {
        0 => eprintln!("mirrored {written} ticket(s)"),
        _ => eprintln!("mirrored {written} ticket(s), {failed} could not be read"),
    }
    Ok(())
}

async fn search(
    session: &Session,
    base: &str,
    key: &str,
    sprint: Option<u64>,
    opts: &Opts,
) -> Result<Vec<Ticket>, Error> {
    let issues = session
        .get_all_by_token(&search_url(base, key, sprint), "issues", opts.verbose)
        .await?;
    Ok(issues.iter().filter_map(ticket_from).collect())
}

// The backlog is per board, and the boards that have one share it — GDE's two
// Scrum boards report the same 83 issues — so it's collected once by key. A
// board without a backlog answers 400, the same way it does for sprints, and is
// skipped rather than taken for a failure.
async fn backlog(session: &Session, base: &str, key: &str, opts: &Opts) -> Result<Vec<Ticket>, Error> {
    let mut seen: BTreeMap<String, Ticket> = BTreeMap::new();
    for Board { id, name } in boards::own(session, base, key, opts).await? {
        let url = format!("{}?fields={FIELDS}", boards::backlog_url(base, id));
        match session.get_all_rows(&url, "issues", opts.verbose).await {
            Ok(issues) => {
                for issue in &issues {
                    let Some(ticket) = ticket_from(issue) else { continue };
                    seen.entry(ticket.key.clone()).or_insert(ticket);
                }
            }
            Err(e) if api::is_status(&e, 400) => {
                if opts.verbose {
                    eprintln!("board {id} ({name}) has no backlog: {e}");
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(seen.into_values().collect())
}

// Ordered by key on the server: it's the column you scan, and Jira sorts the
// issue number numerically, which a sort on the formatted key would not.
fn search_url(base: &str, key: &str, sprint: Option<u64>) -> String {
    let mut jql = format!("project = \"{key}\"");
    if let Some(id) = sprint {
        jql.push_str(&format!(" AND sprint = {id}"));
    }
    jql.push_str(" ORDER BY key ASC");
    format!("{base}/rest/api/3/search/jql?jql={}&fields={FIELDS}&maxResults=100", api::encode(&jql))
}

fn project_url(base: &str, key: &str) -> String {
    format!("{base}/rest/api/3/project/{key}")
}

fn sprint_url(base: &str, id: u64) -> String {
    format!("{base}/rest/agile/1.0/sprint/{id}")
}

// A ticket key in the order a ticket number means: by project, then by number
// and not by the digits it happens to be written with. A key that isn't shaped
// like one sorts last rather than sorting wrongly.
fn number(key: &str) -> (&str, u64) {
    match key.rsplit_once('-') {
        Some((project, number)) => (project, number.parse().unwrap_or(u64::MAX)),
        None => (key, u64::MAX),
    }
}

fn ticket_from(v: &Value) -> Option<Ticket> {
    let key = v.get("key").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let fields = v.get("fields");
    // Each of these is an object with a name, and any of them can be absent —
    // an unassigned ticket has assignee: null, which is a normal state.
    let named = |field: &str| {
        fields
            .and_then(|f| f.get(field))
            .and_then(|o| o.get("name"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(Ticket {
        key: key.to_string(),
        // The readable key rather than the numeric id. For a sub-task this is
        // the story above it; in a team-managed project it's the epic.
        parent: fields
            .and_then(|f| f.get("parent"))
            .and_then(|p| p.get("key"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        kind: named("issuetype").unwrap_or_default(),
        status: named("status").unwrap_or_default(),
        status_category: fields
            .and_then(|f| f.get("status"))
            .and_then(|s| s.get("statusCategory"))
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        assignee: fields
            .and_then(|f| f.get("assignee"))
            .and_then(|a| a.get("displayName"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        summary: fields
            .and_then(|f| f.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn print_table(tickets: &[Ticket]) {
    let width = |header: &str, f: &dyn Fn(&Ticket) -> &str| {
        tickets.iter().map(|t| f(t).chars().count()).max().unwrap_or(0).max(header.chars().count())
    };
    let key_w = width("KEY", &|t| &t.key);
    // A flat project has nothing in this column, and a column of "-" says
    // nothing worth the width.
    let show_parent = tickets.iter().any(|t| t.parent.is_some());
    let parent_w = width("PARENT", &|t| t.parent.as_deref().unwrap_or("-"));
    let type_w = width("TYPE", &|t| &t.kind);
    // Capped rather than fitted: these two are what make a line too wide for a
    // small screen, and a cut-off word still reads.
    let summary_w = width("SUMMARY", &|t| &t.summary).min(MAX_SUMMARY);
    // The header is what sets this column's width; a code is never wider.
    let status_w = STATUS_CODE.max("ST".len());

    // What identifies a ticket comes first and what describes it last, so a
    // narrow terminal cuts off the status and the assignee rather than the
    // summary. That order is why the summary is padded at all: columns after it
    // only line up if it has a width.
    let mut header = format!("{}{:<key_w$}  ", c(BOLD), "KEY");
    if show_parent {
        header.push_str(&format!("{:<parent_w$}  ", "PARENT"));
    }
    header.push_str(&format!(
        "{:<type_w$}  {:<summary_w$}  {:<status_w$}  ASSIGNEE{}",
        "TYPE",
        "SUMMARY",
        "ST",
        c(RESET)
    ));
    outln!("{header}");

    for t in tickets {
        // Work that is finished is the least interesting thing in the list, so
        // it's dimmed and what's in progress is picked out.
        let status_colour = match t.status_category.as_str() {
            "Done" => c(DIM),
            "In Progress" => c(YELLOW),
            _ => "",
        };
        let mut line = format!("{}{:<key_w$}{}  ", c(CYAN), t.key, c(RESET));
        if show_parent {
            line.push_str(&format!(
                "{}{:<parent_w$}{}  ",
                c(DIM),
                t.parent.as_deref().unwrap_or("-"),
                c(RESET)
            ));
        }
        // The assignee goes last and unpadded: it is the one column nothing
        // follows, so a width would only add trailing blanks.
        line.push_str(&format!(
            "{:<type_w$}  {:<summary_w$}  {}{:<status_w$}{}  {}",
            t.kind,
            cut(&t.summary, summary_w),
            status_colour,
            status_code(&t.status),
            c(RESET),
            t.assignee.as_deref().unwrap_or("-"),
        ));
        outln!("{line}");
    }
}

// A short code for a status, built rather than looked up: a Jira administrator
// invents statuses whenever they like, and a hand-kept table would be wrong the
// first time one of them did.
//
// Words that carry no meaning in a status name are dropped, and then, in order:
// a name that fits whole is used whole ("To Do" -> ToDo, "In QA" -> InQA); a
// single word is cut ("Backlog" -> Back); otherwise the initials, padded out of
// the last word when there are too few of them — which is what tells
// "In Rewriting" (IRew) from "In Refinement" (IRef).
fn status_code(status: &str) -> String {
    const FILLER: [&str; 5] = ["for", "by", "the", "of", "and"];
    let words: Vec<&str> = status
        .split(|ch: char| !ch.is_alphabetic())
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.to_lowercase().as_str()))
        .collect();
    let Some(last) = words.last() else { return cut_hard(status, STATUS_CODE) };

    let joined: String = words.concat();
    if joined.chars().count() <= STATUS_CODE {
        return joined;
    }
    if words.len() == 1 {
        return cut_hard(words[0], STATUS_CODE);
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if initials.chars().count() >= STATUS_CODE {
        return cut_hard(&initials, STATUS_CODE);
    }
    // One initial short of the width, so the last word fills the rest.
    let keep = initials.chars().count() - 1;
    let head: String = initials.chars().take(keep).collect();
    let tail: String = last.chars().take(STATUS_CODE - keep).collect();
    format!("{head}{tail}")
}

fn cut_hard(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

// Cuts to `width` characters, marking the cut. The ellipsis takes the place of
// a character rather than being added to the end, so the column keeps its width
// — which is the only reason the columns after it line up.
fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_jql_names_the_project_and_orders_by_key() {
        let url = search_url("https://x.atlassian.net", "BUA", None);
        assert!(url.contains("jql=project%20%3D%20%22BUA%22%20ORDER%20BY%20key%20ASC"), "got: {url}");
        assert!(url.contains("fields=summary,status,issuetype,assignee,parent"), "got: {url}");
    }

    #[test]
    fn a_sprint_is_added_to_the_jql_rather_than_filtered_afterwards() {
        let url = search_url("https://x.atlassian.net", "BUA", Some(233));
        assert!(url.contains("AND%20sprint%20%3D%20233"), "got: {url}");
    }

    // The key is quoted because a project key may collide with a JQL keyword.
    #[test]
    fn the_project_key_is_quoted_in_the_jql() {
        let url = search_url("https://x.atlassian.net", "AND", None);
        assert!(url.contains("%22AND%22"), "got: {url}");
    }

    #[test]
    fn the_validation_urls_are_built_off_the_configured_base() {
        assert_eq!(project_url("https://x.atlassian.net", "BUA"), "https://x.atlassian.net/rest/api/3/project/BUA");
        assert_eq!(sprint_url("https://x.atlassian.net", 233), "https://x.atlassian.net/rest/agile/1.0/sprint/233");
    }

    // The bug this fixes: the backlog was collected into a map keyed by the
    // whole key, so GDE-102 came before GDE-11.
    // Four characters, because two collide: "In Rewriting" and "In Refinement"
    // both reduce to IR, and six statuses here begin with "Ready for".
    #[test]
    fn a_status_becomes_a_code_of_four_characters() {
        for (status, code) in [
            ("Done", "Done"),
            ("Open", "Open"),
            ("Backlog", "Back"),
            ("Ready for Grooming", "RGro"),
            ("Ready for Acceptance", "RAcc"),
            ("Ready for Code Review", "RCRe"),
            ("Ready for Estimation/Planning", "REPl"),
            ("In Implementation", "IImp"),
            ("Selected for Development", "SDev"),
            ("Sub-Bug: Ready for QA", "SBRQ"),
            // Short enough to keep whole, which reads better than initials.
            ("To Do", "ToDo"),
            ("In QA", "InQA"),
        ] {
            assert_eq!(status_code(status), code, "{status:?}");
        }
    }

    // The pair that two letters cannot tell apart.
    #[test]
    fn statuses_that_start_alike_still_differ() {
        assert_eq!(status_code("In Rewriting"), "IRew");
        assert_eq!(status_code("In Refinement"), "IRef");
        assert_ne!(status_code("Ready for Refinement"), status_code("In Refinement"));
    }

    // An administrator can call a status anything, including nothing useful.
    #[test]
    fn an_unusual_status_still_yields_something() {
        assert_eq!(status_code(""), "");
        assert_eq!(status_code("---"), "---");
        assert_eq!(status_code("Won't Do"), "WtDo");
        assert!(status_code("Rückmeldung").chars().count() <= 4);
    }

    #[test]
    fn a_long_value_is_cut_and_marked_without_growing_the_column() {
        assert_eq!(cut("kurz", 10), "kurz");
        assert_eq!(cut("genau zehn", 10), "genau zehn");
        // The ellipsis replaces a character, so the result still fits.
        assert_eq!(cut("etwas deutlich laengeres", 10).chars().count(), 10);
        assert_eq!(cut("etwas deutlich laengeres", 10), "etwas deu…");
        // Umlauts count as one character, not as their bytes.
        assert_eq!(cut("Rückmeldung zur Schätzung", 12).chars().count(), 12);
    }

    // A cut that lands on a space would otherwise leave "wort …".
    #[test]
    fn a_cut_does_not_leave_a_dangling_space_before_the_ellipsis() {
        assert_eq!(cut("abcdefgh ijkl", 10), "abcdefgh…");
    }

    #[test]
    fn tickets_sort_by_number_and_not_by_its_digits() {
        let mut keys = ["GDE-11", "GDE-102", "GDE-2", "GDE-10", "GDE-1"];
        keys.sort_by(|a, b| number(a).cmp(&number(b)));
        assert_eq!(keys, ["GDE-1", "GDE-2", "GDE-10", "GDE-11", "GDE-102"]);
    }

    #[test]
    fn the_project_comes_before_the_number() {
        let mut keys = ["BUA-2", "ADS-10", "BUA-1", "ADS-2"];
        keys.sort_by(|a, b| number(a).cmp(&number(b)));
        assert_eq!(keys, ["ADS-2", "ADS-10", "BUA-1", "BUA-2"]);
    }

    // Nothing here produces such a key, but sorting must not reorder wrongly if
    // something ever does.
    #[test]
    fn a_key_that_is_not_shaped_like_one_sorts_last_within_its_project() {
        let mut keys = ["GDE-x", "GDE-2", "GDE-10"];
        keys.sort_by(|a, b| number(a).cmp(&number(b)));
        assert_eq!(keys, ["GDE-2", "GDE-10", "GDE-x"]);
        assert_eq!(number("nodash"), ("nodash", u64::MAX));
    }

    #[test]
    fn a_ticket_is_read_from_the_nested_fields_the_search_returns() {
        let t = ticket_from(&json!({
            "key": "BUA-1",
            "fields": {
                "summary": "Datenbank-Umsetzung",
                "issuetype": {"name": "Story"},
                "status": {"name": "Done", "statusCategory": {"name": "Done"}},
                "assignee": {"displayName": "Johannes Unger"},
                "parent": {"id": "37503", "key": "BUA-1", "fields": {"summary": "above"}}
            }
        }))
        .expect("has a key");

        assert_eq!((t.key.as_str(), t.kind.as_str(), t.status.as_str()), ("BUA-1", "Story", "Done"));
        assert_eq!(t.status_category, "Done");
        assert_eq!(t.assignee.as_deref(), Some("Johannes Unger"));
        assert_eq!(t.summary, "Datenbank-Umsetzung");
        // The readable key, not the numeric id that sits next to it.
        assert_eq!(t.parent.as_deref(), Some("BUA-1"));
    }

    // A story at the top of the hierarchy has no parent, which is normal.
    #[test]
    fn a_ticket_without_a_parent_reads_as_absent() {
        let t = ticket_from(&json!({"key": "BUA-1", "fields": {"summary": "x"}})).unwrap();
        assert_eq!(t.parent, None);
    }

    // An unassigned ticket is a normal ticket, not a broken one.
    #[test]
    fn a_null_assignee_reads_as_absent() {
        let t = ticket_from(&json!({"key": "BUA-2", "fields": {"summary": "x", "assignee": null}})).unwrap();
        assert_eq!(t.assignee, None);
    }

    #[test]
    fn an_entry_without_a_key_is_skipped_since_nothing_can_address_it() {
        assert!(ticket_from(&json!({"fields": {"summary": "keyless"}})).is_none());
    }
}
