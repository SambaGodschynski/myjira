# myjira

A little Jira CLI, and the start of a tool suite. Does not use the official API
(registered app, OAuth, API tokens) — it piggybacks on your own logged-in
browser session instead, the same way Jira's own web client talks to its
servers. This is unofficial, reverse-engineered territory: it can break at any
moment if Atlassian changes something internally. Use at your own risk, don't
rely on it for anything important.

Sibling project to `myslackcli`, same shape: one static binary, credentials in
`~/.config/<tool>/.env`, plain lines on stdout that pipe into a fuzzy-finder.

## Setup

Credentials live outside the checkout, so they can't be committed by accident:

```bash
mkdir -p ~/.config/myjira
cp .env.example ~/.config/myjira/.env
chmod 600 ~/.config/myjira/.env
# fill in JIRA_SITE and JIRA_COOKIE as described below
cargo build --release
./target/release/myjira list-orgs
```

`--env <file>` points at a different file if you'd rather keep it elsewhere.

## Getting the session cookie

The credential is the **whole `Cookie` request header**, copied verbatim — not
one named cookie out of it:

1. Open any Jira page in your browser, e.g. `https://your.atlassian.net/jira`.
2. Open DevTools (F12) → **Network** tab, then reload the page.
3. Click the first `GET` request (the document itself) → **Headers** →
   **Request Headers** → copy the entire value of `Cookie:`. It looks like
   `_ga=GA1.1.…; JSESSIONID=…; atlassian.xsrf.token=…; tenant.session.token=…`.
4. Paste it into `~/.config/myjira/.env` as `JIRA_COOKIE=…`, exactly as
   copied — no quotes and no escaping needed.

Why the whole header rather than one cookie: which cookie carries the session
differs between Jira Cloud and on-premise, and writes need the
`atlassian.xsrf.token` alongside it. Passing the header through unchanged is
both simpler and what actually works.

### The .env format

Everything after the first `=` is the value, verbatim to the end of the line.
That's the whole rule, and it's why a pasted Cookie header — full of `;`,
spaces and `=` — needs no quoting. Quotes are accepted and stripped if you
prefer them, `export` prefixes are ignored, and a `#` comment has to sit on its
own line rather than trailing a value. Real environment variables win over the
file, so `JIRA_SITE=… myjira list-orgs` works for a one-off.

This is why the tool parses the file itself instead of using a dotenv library:
the usual ones reject an unquoted value like this one and report a column index
rather than the fix.

It's a full session credential for your own account — don't share it, and keep
the file out of the checkout (see Setup). When it expires, every command says so
in as many words instead of failing obscurely; grab a fresh one the same way.

That last part takes some doing, because Jira doesn't answer an unauthenticated
caller with a `401`. It serves an anonymous view instead: `/project/search`
returns `200` with no rows, and a project or issue the caller may not see comes
back as `404`. Left alone, a dead cookie therefore reads as "no projects
visible" or "No project could be found with key 'BUA'" — about a project that is
plainly there. So wherever a result is empty or absent, the commands ask
`/myself` who we are, and say the session expired when the answer is nobody.
It's one extra call, only ever on a path that already had nothing to show.

### Dead ends, so they don't get retried

- **`cloud.session.token` on `.atlassian.com`** authenticates your Atlassian
  *account* (`admin.atlassian.com/gateway/api/me` answers with it), but Jira
  itself rejects it: the site has its own session on the `.atlassian.net` host.
- **`admin.atlassian.com/gateway/api/adminhub/um/orgs`**, the admin hub's own
  organization list, answers `403 poco: rejected authenticated request` for a
  normal user. It needs org-admin rights, so it's no basis for a tool that
  should work for everyone.

## Several orgs

One `JIRA_SITE`/`JIRA_COOKIE` pair is the default org. Every additional org is a
`JIRA_SITE_<NAME>`/`JIRA_COOKIE_<NAME>` pair, where `<NAME>` is what the
commands show it as and what `--org=<name>` takes:

```bash
JIRA_SITE=https://digitalklang.atlassian.net
JIRA_COOKIE=_ga=…; tenant.session.token=…
JIRA_SITE_GD=https://jira.example.com
JIRA_COOKIE_GD=JSESSIONID=…
```

A URL may be given without its scheme (`digitalklang.atlassian.net`), which is
how the browser shows it; `https://` is filled in, and a trailing slash dropped.

Each one needs its own cookie: a session is only ever valid for the host that
issued it. There's no list to keep in step — a `JIRA_SITE_*` variable *is* the
declaration of an org. On-premise instances work the same way.

### Which org a command acts on

`DEFAULT_ORG=<name>` says which one, and `--org=<name>` overrides it for a
single command. In full, the order is:

1. `--org=<name>`
2. `DEFAULT_ORG`
3. the only org configured, if there is just one

With several orgs and neither of the first two, a command stops and says so
rather than picking one — acting on the wrong Jira is worse than not acting.
`list-orgs` is the exception: it lists them all, and marks the one that would be
used with a `*`.

## Usage

```bash
myjira list-orgs                    # the configured orgs, resolved live
myjira list-projects                # the projects of DEFAULT_ORG
myjira list-projects --org=gd       # …of that org instead
myjira list-sprints BUA             # the sprints of that project's boards
myjira list-tickets BUA             # every ticket in the project
myjira list-tickets BUA 233         # only the ones in that sprint
myjira list-tickets BUA backlog     # the ones on its boards and in no sprint
myjira show BTE-915                 # one ticket in full
myjira list-projects --json         # the same as JSON
myjira --help                       # all flags
```

Global flags: `--org <name>`, `--json`, `-v/--verbose`, `--no-color`,
`--env <file>`.

### list-orgs

```
  NAME          TYPE   URL                                 ACCOUNT / CLOUD ID
* digitalklang  Cloud  https://digitalklang.atlassian.net  Johannes Unger  2c32557f-…
  gd            -      https://jira.example.com            error sending request…
```

Also the command to reach for when something isn't working: it calls every org
and reports what came back, so it doubles as the check that a cookie is still
good. The cloud id is what the other Atlassian APIs address a site by, so it's
part of the normal output rather than something to go looking for. An org that
can't be reached gets its error on its own row — the others are still listed —
and the exit status is non-zero so a script notices. The `*` marks the org that
commands use when they aren't told, and only appears when there's a choice.

### list-projects

```
KEY   NAME          TYPE      ID
ADS   ADS Beratung  software  10250
```

Every project the account can see, paginated through in full and sorted by key.
The numeric id is there because the agile and board APIs address a project by it
rather than by key.

Output is meant to be piped; `fzf` wants to be told about the header row:

```bash
myjira list-projects | fzf --ansi --header-lines=1
```

### list-sprints &lt;PROJECT&gt;

```
ID       STATE    NAME                            START     END
233      active   BUA Sprint 1 15.11. - 31.12.21  15.11.21  31.12.21
backlog  backlog  Backlog (5)                     -         -
```

The **backlog** sits among them, where Jira puts it: under the sprints still to
come, above the ones already closed. It isn't a sprint — its id is the word
itself, which `list-tickets <PROJECT> backlog` takes — but it's the other place
a project's work lives, so leaving it out of this list only hid it. The count
comes back with the row, so it's shown.

Active sprints first, then planned ones, then the backlog, then finished ones
newest-first. A
sprint that hasn't been scheduled shows `-` for its dates rather than a blank
that looks like a bug. The project key may be typed in lower case — Jira's own
lookup is case-sensitive and answers "No project could be found with key 'bua'"
for a project that is plainly there, so the key is upper-cased first. A numeric
project id works too.

A `BOARD` column appears only when the sprints come from more than one board.

**Kanban projects have no sprints**, and there is no equivalent level: a Kanban
board's work isn't time-boxed at all. Its nearest relatives are the backlog and
releases/versions, neither of which is an iteration. So for a Kanban project
this says `no sprints in HS` and exits 0 — the query worked, the answer is
empty. A project key that doesn't exist is a different thing and fails with
Jira's own words and a non-zero status.

### list-tickets &lt;PROJECT&gt; [&lt;SPRINT ID&gt;]

```
KEY    PARENT  TYPE      STATUS                ASSIGNEE     SUMMARY
BUA-1  -       Story     Ready for Acceptance  -            Datenbank-Umsetzung
BUA-7  BUA-2   Sub-Task  In Implementation     -            Daily
BUA-8  BUA-1   Sub-Task  Done                  Former user  QA
```

With a sprint id, only that sprint's tickets; with `backlog`, what's on the
project's boards and in no sprint; without either, the whole project — which is
what a Kanban project needs, having no sprints to name. `list-sprints` is where
the ids come from. Ordered by key, by the server, so the issue number
sorts numerically rather than as text. Finished work is dimmed and work in
progress picked out, by status *category*, so it holds up on a board with
custom status names. An unassigned ticket shows `-`.

`PARENT` is the readable key of the ticket above this one — the story a sub-task
belongs to, or, in a team-managed project, the epic. A ticket at the top of the
hierarchy shows `-`, and the column is left out entirely when nothing in the
result has a parent.

### show &lt;TICKET&gt;

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
2. automatisierte Backups sind verschlüsselt

--------------------------------------------------------------------------

Carolin Grandl 30.11.21 12:32

- Kann die Urkundenanlage automatisiert aus dem Geltungsbereich entstehen?
  NEIN, nicht im MVP

◼
```

The layout is valid Markdown, which took more than picking the right character
for the rules. Checked against pandoc:

- A rule needs a **blank line above** it. Dashes directly under a line of text
  make that line a heading — `=====` turned the header into an `<h1>`, and
  swapping in `-----` alone only moved it to `<h2>`.
- The opening rule needs a blank line **below** it too: a dashed line with text
  straight underneath is how pandoc spells a table, and it swallowed the whole
  header block into one.
- The key, the summary and the four fields carry **two trailing spaces**.
  Consecutive lines are one paragraph in Markdown, so without that hard break a
  renderer runs all six onto a single line. In a terminal it makes no
  difference.

The key may be typed in lower case. Every comment is fetched, not just the first
page. The width is fixed at 74 columns rather than read off the terminal, since
the output is as often piped as read and a width that moves with the window
makes two runs incomparable.

The body comes out as Markdown, so it can go straight into a renderer:

```bash
myjira show BTE-915 --no-color | glow -
```

## x_jira: the whole thing under fzf

`x_jira` in `~/Nextcloud/home/scripts/myscripts.sh` walks the commands above as
one picker: projects, then that project's sprints where it has any, then
tickets, then the ticket itself in nvim. Esc steps back exactly one level
everywhere, so it needs two keys.

```
projects ──enter──> sprints ──enter──> tickets ──enter──> nvim
   ^                   |                  |                |
   └───────esc─────────┴────────esc───────┴──────:q────────┘
```

A project with no sprints skips that level altogether — its tickets are the only
list there is, and esc from them goes straight back to the projects. `ctrl-a` on
the sprint list asks for the project's tickets regardless of sprint, and
`ctrl-r` reloads a list from Jira. The backlog row travels through the picker
like any sprint, since its id is a word rather than a number.

Three details in there are easy to get wrong:

- **Each level's rows are fetched once and kept.** A project like ADS is 1370
  tickets over fourteen requests; re-fetching them every time a ticket is closed
  made the list feel broken. `ctrl-r` is the only thing that asks again.
- **`--ansi` plus `--header-lines=1`.** The colours the commands emit survive
  into the list, and fzf strips them back out of the line it prints, so the key
  in the first column goes straight to the next command. Each command's own
  column header becomes fzf's header: visible, not selectable.
- **stderr is kept and shown when a level comes back empty.** fzf swallows it
  otherwise — with nothing on stdin it just shows an empty list, which is how
  "this sprint is empty" and "the cookie expired" came to look identical.

Given arguments, `x_jira` just runs them (`x_jira list-projects`); the picker is
what you get for asking for nothing in particular.

## Rich text: ADF, not HTML

Jira Cloud stores descriptions and comments as **ADF** (Atlassian Document
Format) — a JSON tree, not HTML. `/rest/api/3` hands that tree over as-is, so
`adf.rs` walks it into Markdown. The alternative, asking for
`expand=renderedFields` and parsing the HTML back apart, means recovering
structure that was already there and losing it in the process.

What maps to what:

| ADF | Markdown |
| --- | --- |
| heading, paragraph, lists, code block, rule | the obvious spelling |
| `strong`, `em`, `code`, `strike`, `link` marks | `**`, `*`, `` ` ``, `~~`, `[…](…)` |
| `panel` (info/note/warning/…), `expand` | a blockquote under a bold label |
| `table` | a Markdown table; the first row is the header, whether or not ADF said so |
| `status` lozenge | `[OK]` |
| `media` | `[Attachment: shot.png]`, from the node's `alt` |
| `inlineCard`, `blockCard` | `<url>` |
| `emoji`, `mention` | its character or `:short_name:`, and `@Name` |
| `date` | a date rather than the millisecond count ADF carries |
| `taskItem` | `- [x]` / `- [ ]` |

Three decisions worth knowing:

- **Text is not escaped.** The output is read in a terminal, where a backslash
  in front of every `*` and `_` costs more than the rare sentence it protects.
- **Underline, text colour and sub/superscript have no Markdown.** Their text
  passes through unmarked rather than being dropped.
- **An unknown node keeps its text.** ADF's node list is the most likely thing
  here to change, and a node this doesn't recognise must not swallow the
  sentence inside it.

## Notes on the endpoints

Per org, `list-orgs` calls:

`list-orgs`, per org:

| Call | What for | Needs the cookie? |
| --- | --- | --- |
| `/rest/api/3/serverInfo` | flavour (Cloud/Server) and version | readable anonymously |
| `/_edge/tenant_info` | the cloud id | no; cloud only |
| `/rest/api/latest/myself` | which account the session belongs to | yes |

`list-sprints` goes through `/rest/agile/1.0/board?projectKeyOrId=<KEY>` and then
`/rest/agile/1.0/board/<id>/sprint`, with two corrections that matter:

- **Only boards the project owns.** `projectKeyOrId` answers with every board
  whose filter reaches into the project. For `HS` that's four boards, three of
  them owned by `UN` and `DRAG`, and taking their sprints meant `list-sprints HS`
  reported hundreds of sprints for a Kanban project. Each board's `location` says
  who owns it, and only those count.
- **A sprint is collected once, by id.** Boards share sprints — GDE's two Scrum
  boards both carry sprint 38 — so without this every sprint appeared once per
  board that led to it. The `BOARD` column names a sprint's own board
  (`originBoardId`) rather than whichever one it was reached through, since
  several can be.

A board that doesn't support sprints answers `400 The board does not support
sprints`; that's read as "this board has none" and the remaining boards are still
asked. `-v` prints both the skipped boards and that reason.

The **backlog** comes from `/rest/agile/1.0/board/<id>/backlog`, and not from
JQL, because the two don't agree: `project = "BUA" AND sprint IS EMPTY` answers
8 where the board's own backlog holds 5. The board leaves out what it doesn't
show, and the board is what "the backlog" means. It's per board and the boards
that have one share it — GDE's two Scrum boards report the same 83 issues — so
it's one row for the project and the issues are collected once by key. A board
without a backlog says `400 Backlogs are not supported on this board`, which is
handled exactly like the sprint case.

`list-tickets` searches with JQL — `project = "<KEY>"`, plus `AND sprint = <id>`
when one is given — through `/rest/api/3/search/jql`. Three things about it are
worth knowing:

- **`/rest/api/2/search` is gone**, answering `410` on Cloud. Anything written
  against it (the older `jira-checklist-migrator` script, for one) no longer
  runs.
- **It pages by an opaque `nextPageToken`**, not by `startAt`, so it needs its
  own walk alongside the offset one.
- **It never objects.** An unknown project, an unknown sprint id and even
  syntactically broken JQL all come back as `200` with an empty list — so a typo
  would read as "this project has no tickets". Hence one call to
  `/rest/api/3/project/<KEY>` and, when a sprint is named, one to
  `/rest/agile/1.0/sprint/<id>` before searching: their answers are discarded
  and their 404s are the point.

The project key is quoted in the JQL, since a key like `AND` is legal and would
otherwise be read as an operator.

`show` reads `/rest/api/3/issue/<KEY>` and pages
`/rest/api/3/issue/<KEY>/comment` separately — comments arrive under `comments`
rather than `values` and report a `total` instead of an `isLast`, which is why
the offset paginator takes the row name and accepts either signal. Walking it by
`isLast` alone stopped after the first page and silently lost the rest.

`list-projects` uses `/rest/api/latest/project/search`, paged 50 at a time until
it reports itself last, stepping by the page size the server actually used
rather than the one asked for. On-premise Jira has no `/project/search`; a 404
on the *first* page falls back to `/rest/api/latest/project`, which answers with
everything in one array. A 404 later in a paginated run is a different problem
and is reported rather than restarted.

`/rest/api/latest/` rather than `/3/`: on-premise Jira never got past `/2`, and
`latest` resolves to whichever the instance has. A cookie Jira doesn't recognise
is worse than no cookie — it answers `401` rather than falling back to anonymous
— which is why a stale session shows up on the very first call.

Rate limits are waited out rather than returned as an error: Atlassian says how
long to wait and the run honours it, up to five waits, so
`Rate limited — waiting 5s (1/5)` is progress, not a hang.

## Tests

```bash
cargo test
```

The HTTP layer is tested against a canned TCP server: that the Cookie header
goes out verbatim, the rate-limit retry and its budget, the three shapes a dead
session takes (`401`, a redirect to the login page, an HTML page where JSON was
expected), that a Jira error message is lifted out of its envelope instead of
being reported as a bare status, that a status stays recognisable as a number so
a caller can tell a 404 or 400 from a real failure, that the offset paginator
walks pages by the size the server used and the token paginator follows
`nextPageToken` to the last page, and that query values are percent-encoded. The rest covers the `.env`
format, org resolution (`--org` over `DEFAULT_ORG` over the only one), the
board-ownership filter and the response parsing. The ADF renderer is tested
node by node — nesting of marks, nested lists, tables with ragged rows, the
inline chips, and that an unknown node keeps its text.
