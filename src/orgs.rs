use serde_json::{Value, json};

use crate::api::{self, Session};
use crate::config::{Config, Instance};
use crate::{BOLD, CYAN, DIM, Error, Opts, RED, RESET, c};

// What each instance is asked about itself. serverInfo says what it is and
// which flavour, tenant_info supplies the cloud id the other APIs address a
// site by, and myself is the one call that needs the cookie — so it doubles as
// the check that the session actually works.
fn server_info_url(base: &str) -> String {
    format!("{base}/rest/api/3/serverInfo")
}
fn tenant_info_url(base: &str) -> String {
    format!("{base}/_edge/tenant_info")
}
// Cloud is on /3, on-premise never got past /2, and /latest resolves to
// whichever of the two the instance has.
fn myself_url(base: &str) -> String {
    format!("{base}/rest/api/latest/myself")
}

struct Resolved {
    kind: String,
    version: String,
    cloud_id: Option<String>,
    account: String,
}

pub async fn list(config: &Config, opts: &Opts) -> Result<(), Error> {
    let instances = &config.instances;
    let client = api::client();
    let mut rows: Vec<(&Instance, Result<Resolved, String>)> = Vec::new();
    for instance in instances {
        let session = Session::new(&client, instance.cookie.clone());
        rows.push((instance, resolve(&session, instance, opts).await));
    }

    if opts.json {
        let out: Vec<Value> = rows
            .iter()
            .map(|(instance, resolved)| match resolved {
                Ok(r) => json!({
                    "name": instance.name,
                    "default": config.default_name() == Some(instance.name.as_str()),
                    "url": instance.url,
                    "type": r.kind,
                    "version": r.version,
                    "cloudId": r.cloud_id,
                    "account": r.account,
                }),
                Err(e) => json!({ "name": instance.name, "url": instance.url, "error": e }),
            })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_table(&rows, config.default_name());
    }

    // The table is printed either way — a dead session on one instance is not a
    // reason to withhold what the others answered — but the exit status still
    // has to say that something didn't work, or a script would never notice.
    let failed = rows.iter().filter(|(_, r)| r.is_err()).count();
    if failed > 0 {
        return Err(format!("{failed} of {} instance(s) could not be reached", rows.len()).into());
    }
    Ok(())
}

async fn resolve(session: &Session, instance: &Instance, opts: &Opts) -> Result<Resolved, String> {
    if opts.verbose {
        eprintln!("resolving {} ({})", instance.name, instance.url);
    }

    // Anonymous callers may read this one, but a cookie Jira doesn't recognise
    // is worse than no cookie: it answers 401 rather than falling back to
    // anonymous. So this call failing is the first sign of a stale session.
    let info = session.get(&server_info_url(&instance.url)).await.map_err(|e| e.to_string())?;
    let kind = string(&info, "deploymentType").unwrap_or_else(|| "unknown".to_string());
    let version = string(&info, "version").unwrap_or_default();

    // Only cloud has a tenant, and a missing one is not worth failing over: it
    // costs the id column, and on-premise legitimately has none.
    let cloud_id = match session.get(&tenant_info_url(&instance.url)).await {
        Ok(tenant) => string(&tenant, "cloudId"),
        Err(_) => None,
    };

    let me = session.get(&myself_url(&instance.url)).await.map_err(|e| e.to_string())?;
    let account = string(&me, "displayName")
        .or_else(|| string(&me, "emailAddress"))
        .or_else(|| string(&me, "name"))
        .unwrap_or_else(|| "?".to_string());

    Ok(Resolved { kind, version, cloud_id, account })
}

fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

fn print_table(rows: &[(&Instance, Result<Resolved, String>)], default: Option<&str>) {
    let cell = |r: &Result<Resolved, String>, pick: fn(&Resolved) -> &str| match r {
        Ok(v) => pick(v).to_string(),
        Err(_) => "-".to_string(),
    };
    let width = |header: &str, f: &dyn Fn(&(&Instance, Result<Resolved, String>)) -> String| {
        rows.iter().map(|row| f(row).chars().count()).max().unwrap_or(0).max(header.chars().count())
    };

    let name_w = width("NAME", &|(i, _)| i.name.clone());
    let type_w = width("TYPE", &|(_, r)| cell(r, |v| &v.kind));
    let url_w = width("URL", &|(i, _)| i.url.clone());

    // The marker column only earns its place once there is a choice to make.
    let marked = default.is_some() && rows.len() > 1;
    let mark = |name: &str| match marked {
        true if default == Some(name) => "* ",
        true => "  ",
        false => "",
    };

    outln!(
        "{}{}{:<name_w$}  {:<type_w$}  {:<url_w$}  {}{}",
        c(BOLD),
        if marked { "  " } else { "" },
        "NAME",
        "TYPE",
        "URL",
        "ACCOUNT / CLOUD ID",
        c(RESET)
    );

    for (instance, resolved) in rows {
        // Assembled in full before anything is written, so a row can never
        // reach the terminal half-printed.
        let mut line = format!(
            "{}{}{:<name_w$}{}  {:<type_w$}  {}{:<url_w$}{}  ",
            mark(&instance.name),
            c(CYAN),
            instance.name,
            c(RESET),
            cell(resolved, |v| &v.kind),
            c(DIM),
            instance.url,
            c(RESET),
        );
        match resolved {
            Ok(r) => {
                line.push_str(&r.account);
                if let Some(id) = &r.cloud_id {
                    line.push_str(&format!("  {}{id}{}", c(DIM), c(RESET)));
                }
            }
            // The error belongs on the row rather than in a separate block:
            // with several instances configured, it has to be obvious which one
            // it's about.
            Err(e) => line.push_str(&format!("{}{e}{}", c(RED), c(RESET))),
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
        assert_eq!(server_info_url(base), "https://digitalklang.atlassian.net/rest/api/3/serverInfo");
        assert_eq!(tenant_info_url(base), "https://digitalklang.atlassian.net/_edge/tenant_info");
        // /latest rather than /3: an on-premise instance only has /2.
        assert_eq!(myself_url(base), "https://digitalklang.atlassian.net/rest/api/latest/myself");
    }

    #[test]
    fn a_missing_or_blank_field_reads_as_absent_rather_than_as_an_empty_string() {
        let v = json!({"displayName": "", "version": "1001.0.0"});
        assert_eq!(string(&v, "displayName"), None);
        assert_eq!(string(&v, "nope"), None);
        assert_eq!(string(&v, "version").as_deref(), Some("1001.0.0"));
    }
}
