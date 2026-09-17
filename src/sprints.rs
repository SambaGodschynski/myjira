use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::config::Instance;
use crate::projects;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, c};

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
    let all_boards = session.get_all(&boards_url(&instance.url, &key), opts.verbose).await?;
    let boards = own_boards(&all_boards, &key, opts);

    // Boards share sprints: GDE's two Scrum boards both carry sprint 38, and a
    // board that merely filters in the project's issues carries dozens more. So
    // a sprint is collected once, by id, however many boards lead to it.
    let mut seen: BTreeMap<u64, Sprint> = BTreeMap::new();
    for (id, name) in &boards {
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
    let names: BTreeMap<u64, &str> = boards.iter().map(|(id, name)| (*id, name.as_str())).collect();
    let mut sprints: Vec<Sprint> = seen.into_values().collect();
    for sprint in &mut sprints {
        if let Some(name) = sprint.origin_board.and_then(|id| names.get(&id)) {
            sprint.board = (*name).to_string();
        }
    }

    // Active first, because that's the one being asked about; then what's
    // planned; then what's done, most recent first.
    sprints.sort_by(|a, b| {
        state_rank(&a.state)
            .cmp(&state_rank(&b.state))
            .then_with(|| b.start.cmp(&a.start))
            .then_with(|| a.name.cmp(&b.name))
    });

    if opts.json {
        let out: Vec<Value> = sprints
            .iter()
            .map(|s| {
                json!({"id": s.id, "state": s.state, "name": s.name, "startDate": s.start,
                       "endDate": s.end, "goal": s.goal, "board": s.board})
            })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if sprints.is_empty() {
        // Not an error: a Kanban project legitimately has none, and the query
        // itself worked. A project key that doesn't exist fails earlier, with
        // Jira's own words.
        eprintln!("no sprints in {key}");
        return Ok(());
    }
    print_table(&sprints);
    Ok(())
}

// `projectKeyOrId` answers with every board whose filter reaches into the
// project, which for HS means three boards belonging to UN and DRAG and all of
// their sprints. "The sprints of project X" can only mean the boards the project
// actually owns, which is what `location` says.
fn own_boards(boards: &[Value], key: &str, opts: &Opts) -> Vec<(u64, String)> {
    let mut own = Vec::new();
    for board in boards {
        let Some(id) = board.get("id").and_then(Value::as_u64) else { continue };
        let name = board.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
        let location = board.get("location");
        let owner = location.and_then(|l| l.get("projectKey")).and_then(Value::as_str);
        // A numeric argument is a project id, which is what location carries it as.
        let owner_id = location
            .and_then(|l| l.get("projectId"))
            .and_then(|id| id.as_u64().map(|n| n.to_string()));

        if owner == Some(key) || owner_id.as_deref() == Some(key) {
            own.push((id, name));
        } else if opts.verbose {
            eprintln!("board {id} ({name}) belongs to {} — skipped", owner.unwrap_or("nothing this can tell"));
        }
    }
    own
}

fn boards_url(base: &str, key: &str) -> String {
    format!("{base}/rest/agile/1.0/board?projectKeyOrId={key}")
}

fn sprints_url(base: &str, board_id: u64) -> String {
    format!("{base}/rest/agile/1.0/board/{board_id}/sprint")
}


fn state_rank(state: &str) -> u8 {
    match state {
        "active" => 0,
        "future" => 1,
        _ => 2,
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

fn print_table(sprints: &[Sprint]) {
    let width = |header: &str, f: &dyn Fn(&Sprint) -> String| {
        sprints.iter().map(|s| f(s).chars().count()).max().unwrap_or(0).max(header.chars().count())
    };
    let id_w = width("ID", &|s| s.id.to_string());
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
        assert_eq!(
            boards_url(base, "BUA"),
            "https://digitalklang.atlassian.net/rest/agile/1.0/board?projectKeyOrId=BUA"
        );
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

    fn board(id: u64, name: &str, project_key: &str) -> Value {
        json!({"id": id, "name": name, "type": "scrum",
               "location": {"projectId": 10178, "projectKey": project_key}})
    }

    // The bug this filter fixes: HS is a Kanban project, but projectKeyOrId=HS
    // also returns three boards owned by UN and DRAG, whose hundreds of sprints
    // have nothing to do with HS.
    #[test]
    fn only_the_boards_the_project_owns_are_used() {
        let boards = vec![
            board(4, "HS Board", "HS"),
            board(12, "Unicorns Board", "UN"),
            board(15, "Unicorns Board", "DRAG"),
        ];
        let opts = Opts { verbose: false, json: false };

        assert_eq!(own_boards(&boards, "HS", &opts), vec![(4, "HS Board".to_string())]);
    }

    #[test]
    fn a_numeric_project_id_matches_the_location_id() {
        let boards = vec![board(4, "HS Board", "HS")];
        let opts = Opts { verbose: false, json: false };

        assert_eq!(own_boards(&boards, "10178", &opts).len(), 1);
    }

    // A board with no location can't be shown to belong to the project, so it
    // isn't assumed to.
    #[test]
    fn a_board_without_a_location_is_not_claimed_by_the_project() {
        let boards = vec![json!({"id": 9, "name": "Nowhere Board", "type": "scrum"})];
        let opts = Opts { verbose: false, json: false };

        assert!(own_boards(&boards, "HS", &opts).is_empty());
    }

    #[test]
    fn active_sorts_before_future_and_both_before_closed() {
        let mut ranks: Vec<u8> = ["closed", "active", "future"].iter().map(|s| state_rank(s)).collect();
        ranks.sort();
        assert_eq!(ranks, [0, 1, 2]);
        // An unknown state sorts with the finished ones rather than jumping the queue.
        assert_eq!(state_rank("something-new"), state_rank("closed"));
    }
}
