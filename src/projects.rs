use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::config::Instance;
use crate::{BOLD, CYAN, DIM, Error, Opts, RESET, c};

// Jira caps this well below what it will accept, and answers with the cap it
// actually used — so this is a request, not an assumption.
const PAGE: u32 = 50;
// A page that never reports itself as the last one would otherwise loop for
// ever. 200 pages is far past any real instance.
const MAX_PAGES: u32 = 200;

struct Project {
    key: String,
    name: String,
    kind: String,
    id: String,
}

pub async fn list(instance: &Instance, opts: &Opts) -> Result<(), Error> {
    let session = Session::new(&api::client(), instance.cookie.clone());
    let mut projects = fetch(&session, instance, opts).await?;
    // Sorted by key rather than left in server order: it's the column you scan,
    // and a stable order is what makes two runs comparable.
    projects.sort_by(|a, b| a.key.cmp(&b.key));

    if opts.json {
        let out: Vec<Value> = projects
            .iter()
            .map(|p| json!({"key": p.key, "name": p.name, "type": p.kind, "id": p.id}))
            .collect();
        outln!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if projects.is_empty() {
        eprintln!("no projects visible to this account on {}", instance.name);
        return Ok(());
    }
    print_table(&projects);
    Ok(())
}

fn search_url(base: &str, start_at: u32) -> String {
    format!("{base}/rest/api/latest/project/search?startAt={start_at}&maxResults={PAGE}")
}

// The pre-pagination endpoint, which answers with every project as one array.
// On-premise Jira only has this one.
fn all_url(base: &str) -> String {
    format!("{base}/rest/api/latest/project")
}

async fn fetch(session: &Session, instance: &Instance, opts: &Opts) -> Result<Vec<Project>, Error> {
    let mut projects = Vec::new();
    let mut start_at = 0;

    for page in 0..MAX_PAGES {
        let url = search_url(&instance.url, start_at);
        if opts.verbose {
            eprintln!("GET {url}");
        }
        let resp = match session.get(&url).await {
            Ok(resp) => resp,
            // Only worth falling back on the first page: a 404 midway through a
            // paginated listing is something else entirely, and pretending
            // otherwise would silently restart and duplicate what we have.
            Err(e) if page == 0 && api::is_status(&e, 404) => {
                if opts.verbose {
                    eprintln!("no /project/search here — falling back to /project");
                }
                return fetch_all(session, instance, opts).await;
            }
            Err(e) => return Err(e),
        };

        let values = resp.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
        projects.extend(values.iter().filter_map(project_from));

        if resp.get("isLast").and_then(Value::as_bool) != Some(false) || values.is_empty() {
            break;
        }
        // The server's own page size, which may be smaller than the one asked
        // for — stepping by PAGE instead would skip rows.
        let step = resp.get("maxResults").and_then(Value::as_u64).unwrap_or(values.len() as u64);
        start_at += step.max(1) as u32;
    }

    Ok(projects)
}

async fn fetch_all(session: &Session, instance: &Instance, opts: &Opts) -> Result<Vec<Project>, Error> {
    let url = all_url(&instance.url);
    if opts.verbose {
        eprintln!("GET {url}");
    }
    let resp = session.get(&url).await?;
    let values = resp.as_array().cloned().unwrap_or_default();
    Ok(values.iter().filter_map(project_from).collect())
}

fn project_from(v: &Value) -> Option<Project> {
    let string = |key: &str| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    // A project with no key can't be addressed, so it isn't worth a row.
    let key = string("key")?.to_string();
    Some(Project {
        key,
        name: string("name").unwrap_or_default().to_string(),
        kind: string("projectTypeKey").unwrap_or_default().to_string(),
        // Numeric in some responses and a string in others; both are ids.
        id: string("id")
            .map(str::to_string)
            .or_else(|| v.get("id").and_then(Value::as_u64).map(|n| n.to_string()))
            .unwrap_or_default(),
    })
}

fn print_table(projects: &[Project]) {
    let width = |header: &str, f: &dyn Fn(&Project) -> &str| {
        projects.iter().map(|p| f(p).chars().count()).max().unwrap_or(0).max(header.chars().count())
    };
    let key_w = width("KEY", &|p| &p.key);
    let name_w = width("NAME", &|p| &p.name);
    let type_w = width("TYPE", &|p| &p.kind);

    outln!(
        "{}{:<key_w$}  {:<name_w$}  {:<type_w$}  ID{}",
        c(BOLD),
        "KEY",
        "NAME",
        "TYPE",
        c(RESET)
    );
    for p in projects {
        outln!(
            "{}{:<key_w$}{}  {:<name_w$}  {:<type_w$}  {}{}{}",
            c(CYAN),
            p.key,
            c(RESET),
            p.name,
            p.kind,
            c(DIM),
            p.id,
            c(RESET),
        );
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
            search_url(base, 100),
            "https://digitalklang.atlassian.net/rest/api/latest/project/search?startAt=100&maxResults=50"
        );
        assert_eq!(all_url(base), "https://digitalklang.atlassian.net/rest/api/latest/project");
    }

    #[test]
    fn a_project_is_read_from_the_fields_the_search_returns() {
        let p = project_from(&json!({
            "id": "10250", "key": "ADS", "name": "ADS Beratung",
            "projectTypeKey": "software", "style": "classic"
        }))
        .expect("has a key");

        assert_eq!((p.key.as_str(), p.name.as_str(), p.kind.as_str(), p.id.as_str()),
                   ("ADS", "ADS Beratung", "software", "10250"));
    }

    // On-premise answers with the id as a number rather than a string.
    #[test]
    fn a_numeric_id_is_read_as_well() {
        let p = project_from(&json!({"id": 10250, "key": "ADS"})).expect("has a key");
        assert_eq!(p.id, "10250");
    }

    #[test]
    fn an_entry_without_a_key_is_skipped_since_nothing_can_address_it() {
        assert!(project_from(&json!({"name": "keyless"})).is_none());
    }
}
