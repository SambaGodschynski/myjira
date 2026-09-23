# myjira

A little Jira CLI: list projects, sprints, tickets and read a ticket in full,
from the terminal. Plus [`fzf_example.sh`](fzf_example.sh), which walks all of
it as one picker and opens tickets in nvim.

It does not use the official API (registered app, OAuth, API tokens). It
piggybacks on your own logged-in browser session instead, the same way Jira's
own web client talks to its servers. This is unofficial, reverse-engineered
territory: it can break at any moment if Atlassian changes something
internally. Use at your own risk, don't rely on it for anything important.

Sibling project to `myslackcli`, same shape: one static binary, credentials in
`~/.config/<tool>/.env`, plain lines on stdout that pipe into a fuzzy-finder.

## Setup

```bash
cargo build --release
mkdir -p ~/.config/myjira
cp .env.example ~/.config/myjira/.env
chmod 600 ~/.config/myjira/.env
# fill in JIRA_SITE and JIRA_COOKIE as described below, then:
./target/release/myjira list-orgs
```

Credentials live outside the checkout so they can't be committed by accident.
`--env <file>` points at a different file if you'd rather keep it elsewhere.

### Getting the session cookie

The credential is the **whole `Cookie` request header**, copied verbatim — not
one named cookie out of it:

1. Open any Jira page in your browser, e.g. `https://your.atlassian.net/jira`.
2. Open DevTools (F12) → **Network** tab, then reload the page.
3. Click the first `GET` request (the document itself) → **Headers** →
   **Request Headers** → copy the entire value of `Cookie:`. It looks like
   `_ga=GA1.1.…; JSESSIONID=…; atlassian.xsrf.token=…; tenant.session.token=…`.
4. Paste it into `~/.config/myjira/.env` as `JIRA_COOKIE=…`, exactly as copied.

```bash
JIRA_SITE=https://digitalklang.atlassian.net
JIRA_COOKIE=_ga=GA1.1.…; tenant.session.token=…
```

No quotes and no escaping needed: everything after the first `=` is the value,
verbatim to the end of the line. Quotes are accepted and stripped if you prefer
them, and a `#` comment has to sit on its own line rather than trailing a value.
A URL may be given without its scheme, which is how the browser shows it.

It's a full session credential for your own account — don't share it, and keep
the file out of the checkout. When it expires, every command says so in as many
words; grab a fresh one the same way.

### Several orgs

One `JIRA_SITE`/`JIRA_COOKIE` pair is the default org. Every additional org is a
`JIRA_SITE_<NAME>`/`JIRA_COOKIE_<NAME>` pair, where `<NAME>` is what the
commands show it as and what `--org=<name>` takes:

```bash
JIRA_SITE=https://digitalklang.atlassian.net
JIRA_COOKIE=…
JIRA_SITE_GD=https://jira.example.com
JIRA_COOKIE_GD=…
DEFAULT_ORG=digitalklang
```

Each one needs its own cookie: a session is only ever valid for the host that
issued it. On-premise instances work the same way.

Which org a command acts on: `--org=<name>` first, then `DEFAULT_ORG`, then the
only org configured if there is just one. With several orgs and neither of the
first two, a command stops and says so rather than picking one.

## Usage

```bash
myjira list-orgs                    # the configured orgs, resolved live
myjira list-projects                # every project you can see
myjira list-sprints BUA             # that project's sprints, and its backlog
myjira list-tickets BUA             # every ticket in the project
myjira list-tickets BUA 233         # only the ones in that sprint
myjira list-tickets BUA backlog     # the ones on its boards and in no sprint
myjira show BTE-915                 # one ticket in full
myjira --help                       # all flags
```

Global flags: `--org <name>`, `--mirror <dir>`, `--json`, `-v/--verbose`,
`--no-color`, `--env <file>`. Project and ticket keys may be typed in lower
case.

### `list-orgs`

```
  NAME          TYPE   URL                                 ACCOUNT / CLOUD ID
* digitalklang  Cloud  https://digitalklang.atlassian.net  Johannes Unger  2c32557f-…
  gd            -      https://jira.example.com            error sending request…
```

Also the command to reach for when something isn't working: it calls every org
and reports what came back, so it doubles as the check that a cookie is still
good. An org that can't be reached gets its error on its own row, the others are
still listed, and the exit status is non-zero. The `*` marks the org commands
use when they aren't told, and only appears when there's a choice.

### `list-projects`

```
KEY   NAME          TYPE      ID
ADS   ADS Beratung  software  10250
```

Every project the account can see, sorted by key. The numeric id is there
because the agile and board APIs address a project by it rather than by key.

### `list-sprints <PROJECT>`

```
ID       STATE    NAME                            START     END
233      active   BUA Sprint 1 15.11. - 31.12.21  15.11.21  31.12.21
backlog  backlog  Backlog (5)                     -         -
```

Active sprints first, then planned ones, then the backlog, then finished ones
newest-first. A sprint that hasn't been scheduled shows `-` for its dates. A
`BOARD` column appears when the sprints come from more than one board.

The **backlog** sits among them, where Jira puts it. Its id is the word itself,
which `list-tickets <PROJECT> backlog` takes.

**Kanban projects have no sprints**, and there is no equivalent level: a Kanban
board's work isn't time-boxed at all. So for one of those this says
`no sprints in HS` and exits 0 — the query worked, the answer is empty. A
project key that doesn't exist fails with Jira's own words and a non-zero
status.

### `list-tickets <PROJECT> [<SPRINT ID>|backlog]`

```
KEY    PARENT  TYPE      STATUS                ASSIGNEE     SUMMARY
BUA-1  -       Story     Ready for Acceptance  -            Datenbank-Umsetzung
BUA-7  BUA-2   Sub-Task  In Implementation     -            Daily
```

With a sprint id, only that sprint's tickets; with `backlog`, what's on the
project's boards and in no sprint; without either, the whole project — which is
what a Kanban project needs. `list-sprints` is where the ids come from.

Sorted by project and then by ticket number — 11 before 102, not the other way
round.

`PARENT` is the readable key of the ticket above this one: the story a sub-task
belongs to, or, in a team-managed project, the epic. Finished work is dimmed and
work in progress picked out, by status *category*, so it holds up against custom
status names.

### `show <TICKET>`

```
--------------------------------------------------------------------------

BTE-915
[TECH] Storage-Verschlüsselung der Datenbank aktivieren -- (4.5h)

Assignee   Johannes Unger
Reporter   Johannes Unger
Estimate   4.5h
Remaining  0h

--------------------------------------------------------------------------

Als Entwickler möchte ich, dass die Datenbank und ihre Backups …

## ACs

1. Die Datenbank liegt auf verschlüsseltem Speicher

--------------------------------------------------------------------------

Carolin Grandl 30.11.21 12:32

- Kann die Urkundenanlage automatisiert aus dem Geltungsbereich entstehen?
  NEIN, nicht im MVP

◼
```

Fields, description and every comment. Jira keeps rich text in its own format,
which is rendered to Markdown here — headings, lists, tables, code blocks and
links all come across, attachments as `[Attachment: shot.png]`, status lozenges
as `[OK]`. So the output goes straight into a renderer:

```bash
myjira show BTE-915 --no-color | glow -
```

### `--mirror <dir>`

Also writes each loaded ticket to disk, so it can be grepped, diffed or opened
later without asking Jira again. The file is exactly what `show` prints:

```bash
myjira show BTE-915 --mirror ~/jira          # one ticket
myjira list-tickets BUA 233 --mirror ~/jira  # every ticket in the sprint
# ~/jira/BUA/BUA-1.md, ~/jira/BUA/BUA-2.md, …
```

The ticket key says both levels — `BTE-915` is project `BTE` — so nothing has to
be looked up, and there is no sprint level. **Write-only, and it never deletes.**
An existing file is overwritten without asking, since being current is the
point, but nothing is removed.

With `list-tickets` this fetches every ticket in the listing, at two calls
apiece, which is why it only happens when asked for: a sprint of 83 took about
half a minute, and a project of 1370 would be closer to ten. Progress is
reported as it goes, a ticket that can't be read is named and skipped rather
than ending the run, and the listing itself prints before the download starts.
Other commands say the flag doesn't apply rather than ignoring it.

### Piping

Output is meant to be piped; `fzf` wants to be told about the header row:

```bash
myjira list-tickets ADS | fzf --ansi --header-lines=1
```

### `fzf_example.sh`

```bash
X_JIRA_BIN=/path/to/myjira/target/release/myjira   # or leave it on PATH
. /path/to/myjira/fzf_example.sh
x_jira
```

Sourced rather than run, works in bash and zsh, needs `fzf` and `nvim`. It walks
projects, then that project's sprints where it has any, then tickets, then the
ticket itself in nvim:

```
projects ──enter──> sprints ──enter──> tickets ──enter──> nvim
   ^                   |                  |                |
   └───────esc─────────┴────────esc───────┴──────:q────────┘
```

Esc steps back exactly one level everywhere. A project with no sprints skips
that level altogether. `ctrl-a` on the sprint list asks for the project's
tickets regardless of sprint, and `ctrl-r` reloads a list from Jira.

`x_jira_open_ticket BTE-915` is the ticket step on its own, for when the key is
already known. Given arguments, `x_jira` just runs them (`x_jira list-projects`).

## Tests

```bash
cargo test
```

---

[`NOTES.md`](NOTES.md) has the reverse-engineering findings behind all this:
which endpoints are used and why, the ones that turned out to be dead ends, and
the places where Jira's answers are misleading enough to need working around.
