use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::boards::{self, Board};
use crate::config::Instance;
use crate::projects;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, c};

// What a row on screen is. Kept apart from Sprint because the backlog belongs
// in this list — Jira shows it under the sprints — but it has no sprint id, no
// dates and no state of its own.
struct Row {
    id: String,
    state: String,
    name: String,
    start: Option<String>,
    end: Option<String>,
    // A sprint's goal, where it has one. Not shown in the table — it's prose,
    // and prose has no column — but it belongs in --json.
    goal: Option<String>,
    board: String,
}

pub const BACKLOG: &str = "backlog";

struct Sprint {
    id: u64,
    origin_board: Option<u64>,
    state: String,
    name: String,
    start: Option<String>,
    end: Option<String>,
    goal: Option<String>,
    board: String,
}

pub async fn list(instance: &Instance, project: &str, opts: &Opts) -> Result<(), Error> {
    let key = projects::normalise_key(project)?;
    let session = Session::new(&api::client(), instance.cookie.clone());

    // Sprints belong to boards, not to projects, and a project can have several
    // boards — GDE has three — so the listing goes through them.
    let boards = boards::own(&session, &instance.url, &key, opts).await?;

    // Boards share sprints: GDE's two Scrum boards both carry sprint 38, and a
    // board that merely filters in the project's issues carries dozens more. So
    // a sprint is collected once, by id, however many boards lead to it.
    let mut seen: BTreeMap<u64, Sprint> = BTreeMap::new();
    for Board { id, name } in &boards {
        match session.get_all(&sprints_url(&instance.url, *id), opts.verbose).await {
            Ok(values) => {
                for value in &values {
                    let Some(sprint) = sprint_from(value, name) else { continue };
                    seen.entry(sprint.id).or_insert(sprint);
                }
            }
            // A board with no sprint support answers 400 — a Kanban board, whose
            // work isn't time-boxed at all. That's an absence of sprints rather
            // than a failure, so the other boards are still asked.
            Err(e) if api::is_status(&e, 400) => {
                if opts.verbose {
                    eprintln!("board {id} ({name}) has no sprints: {e}");
                }
            }
            Err(e) => return Err(e),
        }
    }

    // A sprint's own board is the honest attribution, since several boards can
    // lead to the same sprint. Resolved against the project's boards; a sprint
    // originating outside them keeps the board it was reached through.
    let names: BTreeMap<u64, &str> = boards.iter().map(|b| (b.id, b.name.as_str())).collect();
    let mut sprints: Vec<Sprint> = seen.into_values().collect();
    for sprint in &mut sprints {
        if let Some(name) = sprint.origin_board.and_then(|id| names.get(&id)) {
            sprint.board = (*name).to_string();
        }
    }

    let mut rows: Vec<Row> = sprints
        .iter()
        .map(|s| Row {
            id: s.id.to_string(),
            state: s.state.clone(),
            name: s.name.clone(),
            start: s.start.clone(),
            end: s.end.clone(),
            goal: s.goal.clone(),
            board: s.board.clone(),
        })
        .collect();

    // The backlog is what's on a board and in no sprint. Jira lists it under
    // the sprints and so does this, as one row for the project: the boards that
    // have a backlog share it (GDE's two report the same 83 issues), so a row
    // per board would just repeat itself.
    if let Some(backlog) = backlog_row(&session, &instance.url, &boards, opts).await? {
        rows.push(backlog);
    }

    // Active first, because that's the one being asked about; then what's
    // planned, then the backlog as Jira orders it, then what's done, most
    // recent first.
    rows.sort_by(|a, b| {
        state_rank(&a.state)
            .cmp(&state_rank(&b.state))
            .then_with(|| b.start.cmp(&a.start))
            .then_with(|| a.name.cmp(&b.name))
    });

    if opts.json {
        let out: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({"id": r.id, "state": r.state, "name": r.name, "startDate": r.start,
                       "endDate": r.end, "goal": r.goal, "board": r.board})
            })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if rows.is_empty() {
        // Boards this account may not see are simply absent from the answer, so
        // "no sprints" is also what a dead cookie looks like.
        if boards.is_empty() && !session.is_authenticated(&instance.url).await {
            return Err(api::expired());
        }
        // Not an error: a Kanban project legitimately has none, and the query
        // itself worked. A project key that doesn't exist fails earlier, with
        // Jira's own words.
        eprintln!("no sprints in {key}");
        return Ok(());
    }
    print_table(&rows);
    Ok(())
}

// One row for the project's backlog, or none where no board has one.
//
// Asking costs a call per board, which is the only way to know: a board that
// doesn't support backlogs says so with a 400, exactly as it does for sprints.
// The count comes back with the answer, so it's shown rather than discarded.
async fn backlog_row(
    session: &Session,
    base: &str,
    boards: &[Board],
    opts: &Opts,
) -> Result<Option<Row>, Error> {
    for Board { id, name } in boards {
        let url = format!("{}?maxResults=1&fields=key", boards::backlog_url(base, *id));
        if opts.verbose {
            eprintln!("GET {url}");
        }
        match session.get(&url).await {
            Ok(resp) => {
                let total = resp.get("total").and_then(Value::as_u64).unwrap_or(0);
                return Ok(Some(Row {
                    id: BACKLOG.to_string(),
                    state: BACKLOG.to_string(),
                    name: format!("Backlog ({total})"),
                    start: None,
                    end: None,
                    goal: None,
                    board: name.clone(),
                }));
            }
            Err(e) if api::is_status(&e, 400) => {
                if opts.verbose {
                    eprintln!("board {id} ({name}) has no backlog: {e}");
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}


fn sprints_url(base: &str, board_id: u64) -> String {
    format!("{base}/rest/agile/1.0/board/{board_id}/sprint")
}


fn state_rank(state: &str) -> u8 {
    match state {
        "active" => 0,
        "future" => 1,
        // Where Jira puts it: under the sprints that are still to come, above
        // the ones already closed.
        BACKLOG => 2,
        _ => 3,
    }
}

fn sprint_from(v: &Value, board: &str) -> Option<Sprint> {
    let string = |key: &str| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    let id = v.get("id").and_then(Value::as_u64)?;
    Some(Sprint {
        id,
        origin_board: v.get("originBoardId").and_then(Value::as_u64),
        state: string("state").unwrap_or("?").to_string(),
        name: string("name").unwrap_or_default().to_string(),
        // A future sprint has no dates yet, so these stay absent rather than
        // being shown as an epoch or a blank that looks like a bug.
        start: string("startDate").map(str::to_string),
        end: string("endDate").map(str::to_string),
        goal: string("goal").map(str::to_string),
        board: board.to_string(),
    })
}

// "2021-11-15T08:46:40.395Z" -> "15.11.21", matching the family's other tools.
// Anything that isn't an ISO date is passed through rather than mangled.
fn short_date(iso: Option<&String>) -> String {
    let Some(iso) = iso else { return "-".to_string() };
    let date = iso.split('T').next().unwrap_or(iso);
    match date.split('-').collect::<Vec<_>>()[..] {
        [year, month, day] if year.len() == 4 => format!("{day}.{month}.{}", &year[2..]),
        _ => iso.clone(),
    }
}

fn print_table(sprints: &[Row]) {
    let width = |header: &str, f: &dyn Fn(&Row) -> String| {
        sprints.iter().map(|s| f(s).chars().count()).max().unwrap_or(0).max(header.chars().count())
    };
    let id_w = width("ID", &|s| s.id.clone());
    let state_w = width("STATE", &|s| s.state.clone());
    let name_w = width("NAME", &|s| s.name.clone());

    // With one board its name is on every row and says nothing; a project with
    // several is the only case where it distinguishes anything.
    let show_board = sprints.iter().map(|s| s.board.as_str()).collect::<std::collections::BTreeSet<_>>().len() > 1;

    let mut header = format!(
        "{}{:<id_w$}  {:<state_w$}  {:<name_w$}  {:<8}  {:<8}",
        c(BOLD),
        "ID",
        "STATE",
        "NAME",
        "START",
        "END"
    );
    if show_board {
        header.push_str("  BOARD");
    }
    header.push_str(c(RESET));
    outln!("{header}");

    for s in sprints {
        let mut line = format!(
            "{}{:<id_w$}{}  {:<state_w$}  {:<name_w$}  {}{:<8}  {:<8}{}",
            c(CYAN),
            s.id,
            c(RESET),
            s.state,
            s.name,
            c(DIM),
            short_date(s.start.as_ref()),
            short_date(s.end.as_ref()),
            c(RESET),
        );
        if show_board {
            line.push_str(&format!("  {}", s.board));
        }
        outln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_urls_are_built_off_the_configured_base() {
        let base = "https://digitalklang.atlassian.net";
        assert_eq!(sprints_url(base, 84), "https://digitalklang.atlassian.net/rest/agile/1.0/board/84/sprint");
    }

    #[test]
    fn a_sprint_is_read_from_the_fields_the_board_endpoint_returns() {
        let s = sprint_from(
            &json!({"id": 233, "state": "active", "name": "BUA Sprint 1",
                    "startDate": "2021-11-15T08:46:40.395Z", "endDate": "2021-12-31T08:43:00.000Z", "goal": ""}),
            "BUA Board",
        )
        .expect("has an id");

        assert_eq!((s.id, s.state.as_str(), s.board.as_str()), (233, "active", "BUA Board"));
        assert_eq!(short_date(s.start.as_ref()), "15.11.21");
        assert_eq!(short_date(s.end.as_ref()), "31.12.21");
        // An empty goal is an absent goal, not a goal that reads as blank.
        assert_eq!(s.goal, None);
    }

    // Team-managed boards hand back sprints that haven't been scheduled yet.
    #[test]
    fn a_sprint_without_dates_shows_them_as_absent() {
        let s = sprint_from(&json!({"id": 39, "state": "future", "name": "Sprint 1"}), "GD Board").unwrap();

        assert_eq!(short_date(s.start.as_ref()), "-");
        assert_eq!(short_date(s.end.as_ref()), "-");
    }

    #[test]
    fn something_that_is_not_an_iso_date_is_passed_through_rather_than_mangled() {
        assert_eq!(short_date(Some(&"not a date".to_string())), "not a date");
    }

    // The order Jira itself shows: the running sprint, then what's planned, then
    // the backlog, then what's done.
    #[test]
    fn the_rows_sort_active_future_backlog_closed() {
        let ranks: Vec<u8> = ["active", "future", BACKLOG, "closed"].iter().map(|s| state_rank(s)).collect();
        assert_eq!(ranks, [0, 1, 2, 3]);
        assert!(ranks.windows(2).all(|w| w[0] < w[1]), "the order has to be strict");
        // An unknown state sorts with the finished ones rather than jumping the queue.
        assert_eq!(state_rank("something-new"), state_rank("closed"));
    }
}
