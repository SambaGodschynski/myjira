use std::collections::HashMap;

use crate::Error;

// One configured Jira instance: a base URL and the browser session that reaches
// it. The cookie belongs to the instance rather than to the tool as a whole
// because a session is only ever valid for the host that issued it — a second
// site means a second cookie, not a second flag.
pub struct Instance {
    pub name: String,
    pub url: String,
    pub cookie: String,
}

// Written out rather than derived: the cookie is a live session credential, and
// a derived Debug would print it in full the first time an Instance ends up in
// a log line, an error or a panic message.
impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("cookie", &format_args!("<{} bytes, redacted>", self.cookie.len()))
            .finish()
    }
}

const SITE: &str = "JIRA_SITE";
const COOKIE: &str = "JIRA_COOKIE";
const DEFAULT_ORG: &str = "DEFAULT_ORG";

// The configured orgs, plus which of them commands act on when they aren't told.
pub struct Config {
    pub instances: Vec<Instance>,
    pub default_org: Option<String>,
}

impl Config {
    // Resolution order: --org wins, then DEFAULT_ORG, then the only org there
    // is. Anything else is ambiguous, and guessing at it would mean acting on
    // the wrong Jira — so it asks instead.
    pub fn pick(&self, requested: Option<&str>) -> Result<&Instance, Error> {
        if let Some(name) = requested {
            return self.find(name).ok_or_else(|| {
                format!("--org={name} is not a configured org{}", self.known()).into()
            });
        }
        if let Some(name) = &self.default_org {
            return self.find(name).ok_or_else(|| {
                format!("{DEFAULT_ORG}={name} is not a configured org{}", self.known()).into()
            });
        }
        if let [only] = &self.instances[..] {
            return Ok(only);
        }
        Err(format!(
            "several orgs are configured, so this command needs to be told which one — pass --org=<name> or set {DEFAULT_ORG}{}",
            self.known()
        )
        .into())
    }

    // Case-insensitively: the name comes from a variable suffix, which is
    // conventionally upper-case, while it's shown and typed in lower-case.
    pub fn find(&self, name: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.name.eq_ignore_ascii_case(name))
    }

    // The name that commands would use without being told, if it can be
    // determined — which is what `list-orgs` marks its rows with.
    pub fn default_name(&self) -> Option<&str> {
        self.pick(None).ok().map(|i| i.name.as_str())
    }

    fn known(&self) -> String {
        let names: Vec<&str> = self.instances.iter().map(|i| i.name.as_str()).collect();
        format!(" (configured: {})", names.join(", "))
    }
}

// Reads the env file, if there is one, and lets real environment variables
// override it — so a one-off `JIRA_SITE=… myjira list-orgs` still works.
pub fn load(env_path: Option<&str>) -> Result<Config, Error> {
    let mut vars = match env_path {
        // Named explicitly, so a path that doesn't load is an error rather than
        // something to shrug off — the alternative is failing later with a
        // confusing "no Jira instance configured".
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
            parse_env(&text)
        }
        // Deliberately not the working directory: these are full session
        // credentials, and a file that sits next to the source is one stray
        // `git add -f` away from being published. Absent is fine — the vars may
        // already be in the environment.
        None => match default_env_path() {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("could not read {}: {e}", path.display()))?;
                parse_env(&text)
            }
            _ => HashMap::new(),
        },
    };
    for (key, value) in std::env::vars() {
        if key.starts_with("JIRA_") || key == DEFAULT_ORG {
            vars.insert(key, value);
        }
    }

    let default_org = vars.get(DEFAULT_ORG).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    Ok(Config { instances: instances_from(&vars)?, default_org })
}

pub fn default_env_path() -> Option<std::path::PathBuf> {
    let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(config_home.join("myjira").join(".env"))
}

// Hand-rolled rather than dotenvy, for one reason: the credential this file
// holds is a whole Cookie header, which is full of ";" and spaces and "=" —
// and dotenvy rejects that unless it's quoted, with an error that names a
// column index and not the fix. Here everything after the first "=" is the
// value, verbatim, so a pasted header works whether or not it's quoted. The
// cost is that a comment has to sit on its own line instead of trailing a
// value, which is no loss in a file of this size.
fn parse_env(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        vars.insert(key.to_string(), unquote(value.trim()).to_string());
    }
    vars
}

// Quoting is optional, so it has to be undone when it's there. Only a matching
// pair counts: a lone quote is far more likely to be part of a cookie value
// than an attempt at quoting.
fn unquote(value: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    value
}

// Instances are discovered from the configuration rather than listed in it:
// every JIRA_SITE_<NAME> is one, and <NAME> is what it's called. That way
// adding an instance is one pair of lines with no list to keep in step.
fn instances_from(vars: &HashMap<String, String>) -> Result<Vec<Instance>, Error> {
    let mut found: Vec<(Option<String>, String)> = Vec::new();
    for (key, value) in vars {
        let key = key.as_str();
        let value = value.clone();
        if key == SITE {
            found.push((None, value));
        } else if let Some(suffix) = key.strip_prefix(&format!("{SITE}_")) {
            if !suffix.is_empty() {
                found.push((Some(suffix.to_string()), value));
            }
        }
    }

    if found.is_empty() {
        return Err(format!(
            "no Jira instance configured — set {SITE} and {COOKIE} in ~/.config/myjira/.env (see .env.example)"
        )
        .into());
    }

    let mut instances = Vec::new();
    for (suffix, url) in found {
        let cookie_key = match &suffix {
            Some(s) => format!("{COOKIE}_{s}"),
            None => COOKIE.to_string(),
        };
        let site_key = match &suffix {
            Some(s) => format!("{SITE}_{s}"),
            None => SITE.to_string(),
        };

        let url = normalise_url(url.trim());
        if url.is_empty() {
            return Err(format!("{site_key} is empty — it needs a URL like https://your.atlassian.net").into());
        }
        let cookie = vars.get(&cookie_key).map(|c| c.trim().to_string()).unwrap_or_default();
        if cookie.is_empty() {
            return Err(format!("{site_key} is set but {cookie_key} is not — each instance needs its own session cookie").into());
        }

        let name = match &suffix {
            Some(s) => s.to_lowercase(),
            // Unsuffixed: name it after the host's first label, so the default
            // instance still gets something better than "default" to show.
            None => host_label(&url),
        };
        instances.push(Instance { name, url, cookie });
    }

    // Stable output regardless of the environment's own ordering, which is
    // arbitrary — two runs printing the same rows in a different order would
    // make the listing useless to diff.
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(instances)
}

// Accepts the spelling people actually paste. A bare host is the common one:
// the browser shows no scheme in the address bar any more.
fn normalise_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.is_empty() || url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

fn host_label(url: &str) -> String {
    let host = url.split("://").nth(1).unwrap_or(url);
    let host = host.split('/').next().unwrap_or(host);
    let label = host.split(':').next().unwrap_or(host).split('.').next().unwrap_or(host);
    if label.is_empty() { host.to_string() } else { label.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // The regression this format exists for: a Cookie header pasted straight
    // out of DevTools, with no quotes, semicolons and spaces throughout, and
    // "=" inside nearly every one of its parts.
    #[test]
    fn an_unquoted_cookie_header_survives_verbatim() {
        let header = "_ga=GA1.1.1; atl-bsc-consent-token-fallback=0031111010; tenant.session.token=eyJra.iOi.Jn9";
        let parsed = parse_env(&format!("JIRA_SITE=x.atlassian.net\nJIRA_COOKIE={header}\n"));

        assert_eq!(parsed.get("JIRA_COOKIE").map(String::as_str), Some(header));
        assert_eq!(parsed.get("JIRA_SITE").map(String::as_str), Some("x.atlassian.net"));
    }

    #[test]
    fn quoting_is_accepted_too_and_undone() {
        let parsed = parse_env("JIRA_COOKIE=\"a=1; b=2\"\nJIRA_SITE='x.atlassian.net'\n");

        assert_eq!(parsed.get("JIRA_COOKIE").map(String::as_str), Some("a=1; b=2"));
        assert_eq!(parsed.get("JIRA_SITE").map(String::as_str), Some("x.atlassian.net"));
    }

    // A single quote appears inside real cookie values often enough that a lone
    // one must not be mistaken for quoting.
    #[test]
    fn an_unmatched_quote_is_part_of_the_value() {
        assert_eq!(unquote("\"a=1; b=2"), "\"a=1; b=2");
        assert_eq!(unquote("a=1; b=2\""), "a=1; b=2\"");
    }

    #[test]
    fn comments_blank_lines_and_an_export_prefix_are_handled() {
        let parsed = parse_env("# a comment\n\nexport JIRA_SITE=x.atlassian.net\n");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("JIRA_SITE").map(String::as_str), Some("x.atlassian.net"));
    }

    #[test]
    fn a_site_without_its_cookie_names_both_variables() {
        let err = instances_from(&vars(&[("JIRA_SITE_GD", "https://jira.example.com")]))
            .expect_err("a site with no cookie cannot be used");
        let err = err.to_string();

        assert!(err.contains("JIRA_SITE_GD"), "got: {err}");
        assert!(err.contains("JIRA_COOKIE_GD"), "got: {err}");
    }

    #[test]
    fn nothing_configured_says_what_to_set() {
        let err = instances_from(&vars(&[])).expect_err("no instance is an error");
        assert!(err.to_string().contains("JIRA_SITE"), "got: {err}");
    }

    #[test]
    fn the_suffix_names_the_instance_and_the_rows_come_out_sorted() {
        let instances = instances_from(&vars(&[
            ("JIRA_SITE_GD", "https://jira.example.com"),
            ("JIRA_COOKIE_GD", "a=1"),
            ("JIRA_SITE", "https://digitalklang.atlassian.net"),
            ("JIRA_COOKIE", "b=2"),
        ]))
        .unwrap();

        let names: Vec<&str> = instances.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["digitalklang", "gd"]);
        assert_eq!(instances[1].cookie, "a=1");
    }

    fn config(names: &[&str], default_org: Option<&str>) -> Config {
        Config {
            instances: names
                .iter()
                .map(|n| Instance {
                    name: n.to_string(),
                    url: format!("https://{n}.atlassian.net"),
                    cookie: "a=1".to_string(),
                })
                .collect(),
            default_org: default_org.map(str::to_string),
        }
    }

    #[test]
    fn org_given_on_the_command_line_wins_over_the_configured_default() {
        let config = config(&["dgk", "gd"], Some("dgk"));
        assert_eq!(config.pick(Some("gd")).unwrap().name, "gd");
    }

    #[test]
    fn without_an_org_the_configured_default_is_used() {
        let config = config(&["dgk", "gd"], Some("gd"));
        assert_eq!(config.pick(None).unwrap().name, "gd");
        assert_eq!(config.default_name(), Some("gd"));
    }

    // The name comes from a variable suffix, which is conventionally upper-case,
    // but it's shown and typed in lower-case.
    #[test]
    fn the_org_name_is_matched_regardless_of_case() {
        let config = config(&["dgk", "gd"], None);
        assert_eq!(config.pick(Some("DGK")).unwrap().name, "dgk");
    }

    #[test]
    fn a_single_org_needs_neither_a_flag_nor_a_default() {
        let config = config(&["dgk"], None);
        assert_eq!(config.pick(None).unwrap().name, "dgk");
        assert_eq!(config.default_name(), Some("dgk"));
    }

    // Acting on the wrong Jira is worse than not acting, so an ambiguous
    // configuration is an error rather than a guess.
    #[test]
    fn several_orgs_and_no_default_is_an_error_that_names_both_ways_out() {
        let config = config(&["dgk", "gd"], None);
        let err = config.pick(None).expect_err("ambiguous").to_string();

        assert!(err.contains("--org"), "got: {err}");
        assert!(err.contains("DEFAULT_ORG"), "got: {err}");
        assert!(err.contains("dgk, gd"), "should list what is configured, got: {err}");
        assert_eq!(config.default_name(), None);
    }

    #[test]
    fn an_unknown_org_says_so_and_lists_the_known_ones() {
        let config = config(&["dgk", "gd"], None);
        let err = config.pick(Some("nope")).expect_err("unknown org").to_string();
        assert!(err.contains("--org=nope"), "got: {err}");
        assert!(err.contains("dgk, gd"), "got: {err}");
    }

    // Typo in the config file rather than on the command line: the message has
    // to point at the file, not at a flag the user didn't pass.
    #[test]
    fn a_default_org_naming_something_unconfigured_points_at_the_variable() {
        let config = config(&["dgk"], Some("gd"));
        let err = config.pick(None).expect_err("unknown default").to_string();
        assert!(err.contains("DEFAULT_ORG=gd"), "got: {err}");
    }

    #[test]
    fn default_org_is_read_from_the_file_and_blank_counts_as_unset() {
        assert_eq!(parse_env("DEFAULT_ORG=gd\n").get("DEFAULT_ORG").map(String::as_str), Some("gd"));
        let blank = parse_env("DEFAULT_ORG=\n");
        assert_eq!(blank.get("DEFAULT_ORG").map(String::as_str), Some(""));
    }

    // Guards the redaction above: this is the type that carries the session, so
    // it must not be printable in full.
    #[test]
    fn debug_output_does_not_contain_the_cookie() {
        let instance = Instance {
            name: "x".to_string(),
            url: "https://x.atlassian.net".to_string(),
            cookie: "tenant.session.token=secret".to_string(),
        };

        let printed = format!("{instance:?}");
        assert!(!printed.contains("secret"), "the cookie must not be printable: {printed}");
        assert!(printed.contains("redacted"), "got: {printed}");
    }

    #[test]
    fn a_bare_host_gets_the_scheme_it_was_pasted_without() {
        assert_eq!(normalise_url("digitalklang.atlassian.net"), "https://digitalklang.atlassian.net");
    }

    #[test]
    fn a_trailing_slash_is_dropped_so_paths_dont_end_up_doubled() {
        assert_eq!(normalise_url("https://x.atlassian.net/"), "https://x.atlassian.net");
    }

    #[test]
    fn an_explicit_scheme_is_left_alone_including_plain_http_for_on_premise() {
        assert_eq!(normalise_url("http://jira.internal:8080"), "http://jira.internal:8080");
    }

    #[test]
    fn the_default_instance_is_named_after_its_host() {
        assert_eq!(host_label("https://digitalklang.atlassian.net"), "digitalklang");
        assert_eq!(host_label("http://jira.internal:8080"), "jira");
    }
}
