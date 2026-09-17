//! Which boards a project owns, and what hangs off them.
//!
//! Sprints and backlogs both belong to boards rather than to projects, so both
//! `list-sprints` and `list-tickets … backlog` start here.

use serde_json::Value;

use crate::api::Session;
use crate::{Error, Opts};

pub struct Board {
    pub id: u64,
    pub name: String,
}

pub async fn own(session: &Session, base: &str, key: &str, opts: &Opts) -> Result<Vec<Board>, Error> {
    let all = session.get_all(&boards_url(base, key), opts.verbose).await?;
    Ok(own_boards(&all, key, opts))
}

// `projectKeyOrId` answers with every board whose filter reaches into the
// project, which for HS means three boards belonging to UN and DRAG and all of
// their sprints. "The sprints of project X" can only mean the boards the project
// actually owns, which is what `location` says.
pub fn own_boards(boards: &[Value], key: &str, opts: &Opts) -> Vec<Board> {
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
            own.push(Board { id, name });
        } else if opts.verbose {
            eprintln!("board {id} ({name}) belongs to {} — skipped", owner.unwrap_or("nothing this can tell"));
        }
    }
    own
}

fn boards_url(base: &str, key: &str) -> String {
    format!("{base}/rest/agile/1.0/board?projectKeyOrId={key}")
}

// A board's backlog: the issues on it that aren't in a sprint. Not the same as
// JQL's `sprint IS EMPTY`, which for BUA answers 8 where the board's own
// backlog holds 5 — the board leaves out what it doesn't show.
pub fn backlog_url(base: &str, board_id: u64) -> String {
    format!("{base}/rest/agile/1.0/board/{board_id}/backlog")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

        let own = own_boards(&boards, "HS", &opts);
        assert_eq!(own.len(), 1);
        assert_eq!((own[0].id, own[0].name.as_str()), (4, "HS Board"));
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
    fn the_urls_are_built_off_the_configured_base() {
        let base = "https://x.atlassian.net";
        assert_eq!(boards_url(base, "BUA"), "https://x.atlassian.net/rest/agile/1.0/board?projectKeyOrId=BUA");
        assert_eq!(backlog_url(base, 84), "https://x.atlassian.net/rest/agile/1.0/board/84/backlog");
    }
}
