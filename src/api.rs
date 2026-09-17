use serde_json::Value;

use crate::Error;

// Atlassian answers a rate limit with a Retry-After, so the wait is usually
// told to us; these only bound the cases where it isn't, or where it never
// lets up.
const MAX_RATE_LIMIT_WAITS: u32 = 5;
// Jira caps a page size well below what it accepts and reports the cap it used,
// so this is a request rather than an assumption.
const PAGE: u32 = 50;
// A response that never reports itself as the last page would otherwise loop for
// ever. 200 pages is far past any real instance.
const MAX_PAGES: u32 = 200;
const FALLBACK_RETRY_AFTER: u64 = 5;
const MAX_RETRY_AFTER: u64 = 60;

const EXPIRED: &str = "session rejected — the cookie has probably expired. Reload the Jira tab in \
your browser and copy a fresh Cookie header (see the README).";

// Jira's own web client is what this is impersonating, so it sends what that
// client sends. Without a User-Agent some of Atlassian's edge services answer
// differently than they do for a browser.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

// Built once and cloned per instance: a reqwest Client is a handle to a shared
// connection pool, so cloning it is how it's meant to be shared.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        // An expired session doesn't come back as a 401 everywhere: Jira answers
        // some paths with a redirect to the login page, which reqwest would
        // follow and hand us 200 OK full of HTML. Not following it keeps that
        // case distinguishable from a real answer.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a default reqwest client always builds")
}

// A typed error for a non-success status, so a caller can tell one status from
// another without matching on the text of a message. `list-projects` needs
// exactly that: on-premise Jira has no /project/search, and a 404 there means
// "ask the older endpoint instead" rather than "give up".
#[derive(Debug)]
pub struct HttpStatus {
    pub code: u16,
    detail: String,
}

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Jira's own words first, where it gave any: "No project could be found
        // with key 'bua'." is the answer, and "HTTP 400 Bad Request" is not.
        if self.detail.is_empty() {
            write!(f, "HTTP {}", self.code)
        } else {
            write!(f, "{} (HTTP {})", self.detail, self.code)
        }
    }
}

impl std::error::Error for HttpStatus {}

// True when `e` is a non-success status with this code.
pub fn is_status(e: &Error, code: u16) -> bool {
    e.downcast_ref::<HttpStatus>().is_some_and(|s| s.code == code)
}

pub struct Session {
    client: reqwest::Client,
    cookie: String,
}

impl Session {
    pub fn new(client: &reqwest::Client, cookie: String) -> Self {
        Self { client: client.clone(), cookie }
    }

    pub async fn get(&self, url: &str) -> Result<Value, Error> {
        let mut waits = 0u32;
        loop {
            let response = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                // Verbatim: this is the whole Cookie header as the browser sent
                // it. Picking one cookie out of it was the earlier design and it
                // doesn't work — which cookie carries the session differs
                // between cloud and on-premise, and writes need the XSRF one
                // alongside it.
                .header(reqwest::header::COOKIE, &self.cookie)
                .send()
                .await
                .map_err(with_cause)?;

            let status = response.status();
            // Read this before `text()` consumes the response; on a 429 the
            // body isn't necessarily JSON, so status comes first either way.
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                if status == reqwest::StatusCode::UNAUTHORIZED || status.is_redirection() {
                    return Err(EXPIRED.into());
                }
                let body = response.text().await?;
                if !status.is_success() {
                    return Err(Box::new(HttpStatus { code: status.as_u16(), detail: detail(&body) }));
                }
                return serde_json::from_str(&body).map_err(|e| {
                    // HTML where JSON was expected is the other shape a dead
                    // session takes, so say so rather than reporting a parse
                    // error the caller can do nothing with.
                    if body.trim_start().starts_with('<') {
                        EXPIRED.into()
                    } else {
                        Error::from(format!("could not parse the response as JSON: {e} — {}", snippet(&body)))
                    }
                });
            }

            waits += 1;
            if waits > MAX_RATE_LIMIT_WAITS {
                return Err(format!("rate limited (gave up after {MAX_RATE_LIMIT_WAITS} waits)").into());
            }
            let secs = retry_after.unwrap_or(FALLBACK_RETRY_AFTER).clamp(1, MAX_RETRY_AFTER);
                eprintln!("Rate limited — waiting {secs}s ({waits}/{MAX_RATE_LIMIT_WAITS})...");
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    }

    // The search endpoint pages by an opaque token instead of an offset, and
    // carries its rows under a name of its own — so the offset walk above
    // doesn't fit it.
    pub async fn get_all_by_token(&self, url: &str, rows: &str, verbose: bool) -> Result<Vec<Value>, Error> {
        let separator = if url.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        let mut token: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let paged = match &token {
                Some(t) => format!("{url}{separator}nextPageToken={}", encode(t)),
                None => url.to_string(),
            };
            if verbose {
                eprintln!("GET {paged}");
            }
            let resp = self.get(&paged).await?;
            let values = resp.get(rows).and_then(Value::as_array).cloned().unwrap_or_default();
            let count = values.len();
            all.extend(values);

            token = resp.get("nextPageToken").and_then(Value::as_str).map(str::to_string);
            // Either signal is enough to stop, and an empty page always is.
            if resp.get("isLast").and_then(Value::as_bool) == Some(true) || token.is_none() || count == 0 {
                break;
            }
        }

        Ok(all)
    }

    // Every paginated Jira endpoint answers with the same envelope
    // ({startAt, maxResults, isLast, values}), so walking it belongs here rather
    // than in each command.
    pub async fn get_all(&self, url: &str, verbose: bool) -> Result<Vec<Value>, Error> {
        let separator = if url.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        let mut start_at = 0u32;

        for _ in 0..MAX_PAGES {
            let paged = format!("{url}{separator}startAt={start_at}&maxResults={PAGE}");
            if verbose {
                eprintln!("GET {paged}");
            }
            let resp = self.get(&paged).await?;
            let values = resp.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
            let count = values.len();
            all.extend(values);

            // Absent isLast means an endpoint that doesn't paginate at all, so
            // what came back is everything.
            if resp.get("isLast").and_then(Value::as_bool) != Some(false) || count == 0 {
                break;
            }
            // The page size the server actually used, which may be smaller than
            // the one asked for — stepping by PAGE would skip rows.
            let step = resp.get("maxResults").and_then(Value::as_u64).unwrap_or(count as u64).max(1);
            start_at += step as u32;
        }

        Ok(all)
    }
}

// Percent-encodes everything but the unreserved set, so a value can be put in a
// query string without changing its meaning — JQL is full of spaces, "=" and
// quotes.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// Jira reports what went wrong in a body of its own shape. Digging the sentence
// out of it is the difference between an error a user can act on and one that
// only says a request failed.
fn detail(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let messages = v
            .get("errorMessages")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))
            .filter(|s| !s.is_empty());
        if let Some(messages) = messages {
            return messages;
        }
        // The per-field spelling, used by the create/edit endpoints.
        if let Some(errors) = v.get("errors").and_then(Value::as_object) {
            let joined = errors
                .iter()
                .filter_map(|(field, m)| m.as_str().map(|m| format!("{field}: {m}")))
                .collect::<Vec<_>>()
                .join("; ");
            if !joined.is_empty() {
                return joined;
            }
        }
        if let Some(message) = v.get("message").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return message.to_string();
        }
    }
    snippet(body)
}

// reqwest reports "error sending request for url (…)" and keeps the reason —
// a DNS failure, a refused connection, an expired certificate — one level down
// in the source chain. On an on-premise host that reason is usually the whole
// answer, so it gets appended rather than dropped.
fn with_cause(e: reqwest::Error) -> Error {
    let mut message = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !message.contains(&cause_text) {
            message.push_str(&format!(": {cause_text}"));
        }
        source = cause.source();
    }
    message.into()
}

// The first line of a body, for error messages: these responses can be a whole
// HTML page, and dumping one into the terminal buries the status it came with.
fn snippet(body: &str) -> String {
    let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().take(200).collect();
    if line.chars().count() > 200 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn response(head: &str, json: &str) -> String {
        format!(
            "HTTP/1.1 {head}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{json}",
            json.len()
        )
    }

    fn rate_limited() -> String {
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_string()
    }

    fn ok_body() -> String {
        response("200 OK", r#"{"value":42}"#)
    }

    // Answers each connection with the next canned response, so a test can lay
    // out the exact sequence a caller should survive.
    async fn serve(responses: Vec<String>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in responses {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await; // consume the request first
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}/gateway/api/test")
    }

    fn session() -> Session {
        Session::new(&client(), "a=1; b=2".to_string())
    }

    // The whole point of the credential being a pasted header: it has to reach
    // the server exactly as the browser sent it, not reassembled from parts.
    #[tokio::test]
    async fn the_cookie_header_is_sent_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap();
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = sock.write_all(ok_body().as_bytes()).await;
            let _ = sock.flush().await;
        });

        let cookie = "_ga=GA1.1.1; JSESSIONID=ABC; atlassian.xsrf.token=tok_lin";
        Session::new(&client(), cookie.to_string())
            .get(&format!("http://{addr}/rest/api/3/myself"))
            .await
            .unwrap();

        let request = rx.await.unwrap();
        assert!(request.contains(&format!("cookie: {cookie}")) || request.contains(&format!("Cookie: {cookie}")),
                "the Cookie header should be sent unchanged, got:\n{request}");
    }

    // A host that cannot be reached at all: the message has to carry the reason,
    // not just the fact that the request failed.
    #[tokio::test]
    async fn a_connection_failure_reports_why() {
        // Port 0 is never listening, and reserving a port to then close it is
        // the portable way to get an address nothing answers on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = session().get(&format!("http://{addr}/")).await.expect_err("nothing is listening");
        let err = err.to_string();
        assert!(err.contains("error sending request"), "got: {err}");
        assert!(err.len() > "error sending request for url (…)".len(), "the cause should be appended, got: {err}");
    }

    #[tokio::test]
    async fn a_rate_limit_is_waited_out_rather_than_returned_as_an_error() {
        let url = serve(vec![rate_limited(), ok_body()]).await;
        let started = std::time::Instant::now();

        let resp = session().get(&url).await.expect("should succeed on the retry");

        assert_eq!(resp.get("value").and_then(Value::as_i64), Some(42));
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(1),
            "Retry-After: 1 should have been honoured, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn the_retry_budget_is_bounded_so_a_persistent_limit_cannot_hang_the_run() {
        // One more 429 than the budget allows.
        let url = serve(vec![rate_limited(); MAX_RATE_LIMIT_WAITS as usize + 1]).await;

        let err = session().get(&url).await.expect_err("should give up instead of looping forever");
        assert!(err.to_string().contains("rate limited"), "got: {err}");
    }

    #[tokio::test]
    async fn an_unauthorized_answer_says_the_session_expired() {
        let url = serve(vec![response("401 Unauthorized", r#"{"code":401}"#)]).await;

        let err = session().get(&url).await.expect_err("401 is not a usable answer");
        assert!(err.to_string().contains("expired"), "got: {err}");
    }

    // The other shape a dead session takes: a redirect to the login page, which
    // would otherwise be followed and come back as a page of HTML.
    #[tokio::test]
    async fn a_redirect_to_the_login_page_says_the_same() {
        let url = serve(vec!["HTTP/1.1 302 Found\r\nLocation: https://id.atlassian.com/login\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_string()]).await;

        let err = session().get(&url).await.expect_err("a redirect is not an answer");
        assert!(err.to_string().contains("expired"), "got: {err}");
    }

    // The status has to survive as something a caller can branch on, not just
    // as words in a message.
    #[tokio::test]
    async fn a_not_found_is_recognisable_by_its_status() {
        let url = serve(vec![response("404 Not Found", r#"{"message":"no such thing"}"#)]).await;

        let err = session().get(&url).await.expect_err("404 is an error");
        assert!(is_status(&err, 404), "should carry its status, got: {err}");
        assert!(!is_status(&err, 500), "should not match another status");
    }

    // Jira's sentence is the useful part of an error; the status alone is not.
    #[tokio::test]
    async fn a_jira_error_message_is_lifted_out_of_its_envelope() {
        let body = r#"{"errorMessages":["No project could be found with key 'bua'."],"errors":{}}"#;
        let url = serve(vec![response("400 Bad Request", body)]).await;

        let err = session().get(&url).await.expect_err("400 is an error").to_string();
        assert!(err.starts_with("No project could be found with key 'bua'."), "got: {err}");
        assert!(err.contains("HTTP 400"), "the status should still be there, got: {err}");
    }

    #[tokio::test]
    async fn the_per_field_error_spelling_is_read_too() {
        let body = r#"{"errorMessages":[],"errors":{"summary":"Summary is required"}}"#;
        let url = serve(vec![response("400 Bad Request", body)]).await;

        let err = session().get(&url).await.expect_err("400 is an error").to_string();
        assert!(err.contains("summary: Summary is required"), "got: {err}");
    }

    #[tokio::test]
    async fn pages_are_walked_until_the_last_one_and_stepped_by_the_size_the_server_used() {
        let page = |start: u32, is_last: bool| {
            response(
                "200 OK",
                &format!(
                    r#"{{"startAt":{start},"maxResults":2,"isLast":{is_last},"values":[{{"id":{start}}},{{"id":{}}}]}}"#,
                    start + 1
                ),
            )
        };
        let url = serve(vec![page(0, false), page(2, false), page(4, true)]).await;

        let all = session().get_all(&url, false).await.unwrap();
        let ids: Vec<u64> = all.iter().filter_map(|v| v.get("id").and_then(Value::as_u64)).collect();
        assert_eq!(ids, [0, 1, 2, 3, 4, 5]);
    }

    // An endpoint that doesn't paginate omits isLast; one request is all of it.
    #[test]
    fn encoding_leaves_the_unreserved_set_alone_and_escapes_the_rest() {
        assert_eq!(encode("BUA-1_x.y~z"), "BUA-1_x.y~z");
        assert_eq!(encode(r#"project = "BUA""#), "project%20%3D%20%22BUA%22");
        // Multi-byte characters are encoded per UTF-8 byte, not per character.
        assert_eq!(encode("ä"), "%C3%A4");
    }

    #[tokio::test]
    async fn token_pages_are_followed_until_the_last_one() {
        let page = |rows: &str, rest: &str| response("200 OK", &format!(r#"{{"issues":[{rows}]{rest}}}"#));
        let url = serve(vec![
            page(r#"{"key":"A-1"}"#, r#","nextPageToken":"t1","isLast":false"#),
            page(r#"{"key":"A-2"}"#, r#","isLast":true"#),
        ])
        .await;

        let all = session().get_all_by_token(&url, "issues", false).await.unwrap();
        let keys: Vec<&str> = all.iter().filter_map(|v| v.get("key").and_then(Value::as_str)).collect();
        assert_eq!(keys, ["A-1", "A-2"]);
    }

    // No token means there is no page after this one, whatever isLast says.
    #[tokio::test]
    async fn a_token_page_without_a_next_token_ends_the_walk() {
        let url = serve(vec![response("200 OK", r#"{"issues":[{"key":"A-1"}],"isLast":false}"#)]).await;

        let all = session().get_all_by_token(&url, "issues", false).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn a_response_without_is_last_is_taken_as_complete() {
        let url = serve(vec![response("200 OK", r#"{"values":[{"id":1}]}"#)]).await;

        let all = session().get_all(&url, false).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn a_server_error_reports_the_status_and_the_first_line_of_the_body() {
        let url = serve(vec![response("500 Internal Server Error", r#"{"message":"boom"}"#)]).await;

        let err = session().get(&url).await.expect_err("500 is an error");
        let err = err.to_string();
        assert!(err.contains("500"), "got: {err}");
        assert!(err.contains("boom"), "got: {err}");
    }

    #[tokio::test]
    async fn an_html_page_where_json_was_expected_says_the_session_expired() {
        let html = "<!DOCTYPE html><html><body>log in</body></html>";
        let url = serve(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{html}",
            html.len()
        )])
        .await;

        let err = session().get(&url).await.expect_err("HTML is not a usable answer");
        assert!(err.to_string().contains("expired"), "got: {err}");
    }
}
