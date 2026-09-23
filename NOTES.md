# Notes

What was found out building this, kept because none of it is guessable from the
code and all of it cost real effort. Useful the day something breaks.

## Auth

The credential is the whole `Cookie` header from a Jira page's own request,
passed through unchanged. Picking one cookie out of it was the first design and
it doesn't work: which cookie carries the session differs between Cloud and
on-premise, and writes need `atlassian.xsrf.token` alongside it.

Two dead ends, so they don't get retried:

- **`cloud.session.token` on `.atlassian.com`** authenticates your Atlassian
  *account* — `admin.atlassian.com/gateway/api/me` answers with it — but Jira
  itself rejects it. The site has its own session on the `.atlassian.net` host,
  a different domain, which is also why DevTools only shows you one of the two
  at a time.
- **`admin.atlassian.com/gateway/api/adminhub/um/orgs`**, the admin hub's own
  organization list, answers `403 poco: rejected authenticated request` for a
  normal user, through every combination of User-Agent, Referer, Origin and
  XSRF header. It needs org-admin rights, so it's no basis for a tool that
  should work for everyone. That's why `list-orgs` lists *configured* orgs
  rather than discovering them.

No automatic site discovery was found. `available-products` answered 404 on
every guessed path. If it matters, the thing to do is open the app switcher in
the browser and watch the Network tab.

`https://<site>.atlassian.net/_edge/tenant_info` returns the cloudId with no
authentication at all, which is how `list-orgs` fills that column.

## The .env format

Parsed by hand rather than with a dotenv library: everything after the first `=`
is the value, verbatim to the end of the line. A Cookie header is full of `;`,
spaces and `=`, and dotenvy rejects that unless it's quoted — reporting a column
index rather than the fix. That failure also used to be swallowed on the default
path, so a perfectly good file read as "no Jira instance configured".

`Instance` carries a hand-written `Debug` that redacts the cookie. A derived one
would print a live session into the first log line or panic message.

## Endpoints

`list-orgs`, per org:

| Call | What for | Needs the cookie? |
| --- | --- | --- |
| `/rest/api/3/serverInfo` | flavour (Cloud/Server) and version | readable anonymously |
| `/_edge/tenant_info` | the cloud id | no; cloud only |
| `/rest/api/latest/myself` | which account the session belongs to | yes |

`/rest/api/latest/` rather than `/3/` wherever both exist: on-premise Jira never
got past `/2`, and `latest` resolves to whichever the instance has.

`list-projects` uses `/rest/api/latest/project/search`, paged 50 at a time until
it reports itself last, stepping by the page size the server actually used
rather than the one asked for. On-premise Jira has no `/project/search`; a 404
falls back to `/rest/api/latest/project`, which answers with everything in one
array.

`list-sprints` goes through `/rest/agile/1.0/board?projectKeyOrId=<KEY>` and then
`/rest/agile/1.0/board/<id>/sprint`, with two corrections that matter:

- **Only boards the project owns.** `projectKeyOrId` answers with every board
  whose filter reaches into the project. For `HS` that's four boards, three of
  them owned by `UN` and `DRAG`, and taking their sprints meant `list-sprints HS`
  reported hundreds of sprints for a Kanban project. Each board's `location`
  says who owns it, and only those count.
- **A sprint is collected once, by id.** Boards share sprints — GDE's two Scrum
  boards both carry sprint 38 — so without this every sprint appeared once per
  board that led to it. The `BOARD` column names a sprint's own board
  (`originBoardId`) rather than whichever one it was reached through.

A board that doesn't support sprints answers `400 The board does not support
sprints`; that's read as "this board has none" and the remaining boards are still
asked. `-v` prints both the skipped boards and that reason.

The **backlog** comes from `/rest/agile/1.0/board/<id>/backlog`, and not from
JQL, because the two don't agree: `project = "BUA" AND sprint IS EMPTY` answers
8 where the board's own backlog holds 5. The board leaves out what it doesn't
show, and the board is what "the backlog" means. It's per board and the boards
that have one share it — GDE's two report the same 83 issues — so it's one row
for the project and the issues are collected once by key. A board without one
says `400 Backlogs are not supported on this board`.

`list-tickets` searches with JQL through `/rest/api/3/search/jql`:

- **`/rest/api/2/search` is gone**, answering `410` on Cloud. Anything written
  against it no longer runs — including the older `jira-checklist-migrator`
  script, which used it in five places.
- **It pages by an opaque `nextPageToken`**, not by `startAt`, so it needs its
  own walk alongside the offset one.
- **It never objects.** An unknown project, an unknown sprint id and even
  syntactically broken JQL all come back as `200` with an empty list. Hence one
  call to `/rest/api/3/project/<KEY>` and, when a sprint is named, one to
  `/rest/agile/1.0/sprint/<id>` before searching: their answers are discarded
  and their 404s are the point.

The project key is quoted in the JQL, since a key like `AND` is legal and would
otherwise be read as an operator.

Tickets are sorted locally, by project and then by number, rather than left to
whatever produced the list. JQL's `ORDER BY key` already does that, but the
backlog arrives from the agile endpoint in board-rank order and was collected
into a map keyed by the whole key — which put `GDE-102` ahead of `GDE-11`.

`show` reads `/rest/api/3/issue/<KEY>` and pages
`/rest/api/3/issue/<KEY>/comment` separately — comments arrive under `comments`
rather than `values` and report a `total` instead of an `isLast`, which is why
the offset paginator takes the row name and accepts either signal. Walking it by
`isLast` alone stopped after the first page and silently lost the rest.

Rate limits are waited out rather than returned as an error: Atlassian says how
long to wait and the run honours it, up to five waits.

## Jira answers an unauthenticated caller with an anonymous view

Not with a `401`. `/project/search` returns `200` with no rows, and a project or
issue the caller may not see comes back as `404`. Left alone, a dead cookie
therefore reads as "no projects visible", "No project could be found with key
'BUA'" or "Issue does not exist or you do not have permission to see it" —
about things that are plainly there.

So wherever a result is empty or absent, the commands ask `/myself` who we are
and say the session expired when the answer is nobody. One extra call, only ever
on a path that already had nothing to show.

## The mirror has no sprint level

`--mirror` writes `<root>/<PROJECT>/<TICKET>.md` and nothing else, because a
ticket key carries both levels and needs no lookup. `show` and `list-tickets`
write the same file, from the same renderer, so the two can't drift apart.

A sprint level was tried and taken out again: it can't be had cheaply.

- `/rest/api/3/search/jql` **silently drops a requested `sprint` field**. Asking
  for `fields=summary,sprint` returns `summary` alone, with no error, so the
  tickets of a whole project arrive with no sprint at all.
- `/rest/agile/1.0/issue/<KEY>` does know, and reports a finished sprint under
  `closedSprints` where `sprint` is null. But its `description` comes back as a
  string rather than ADF, so it can't replace the `/rest/api/3/issue` call —
  only follow it, at one extra request per ticket.
- Nothing distinguishes "in the backlog" from "on no board at all": both report
  `sprint: null`.

Directory names still go through a slug, which matters more than it looks: it is
the one place where text becomes a path, and a key is the only thing that ever
reaches it by design.

## Rich text: ADF, not HTML

Jira Cloud stores descriptions and comments as **ADF** (Atlassian Document
Format) — a JSON tree, not HTML. `/rest/api/3` hands that tree over as-is, so
`adf.rs` walks it into Markdown. The alternative, asking for
`expand=renderedFields` and parsing the HTML back apart, means recovering
structure that was already there and losing it in the process.

What the renderer covers was decided by looking first: a census over ~120
descriptions in this instance turns up `paragraph`, `heading`, both list kinds,
`hardBreak`, `panel`, `table`, `mediaSingle`/`media`, `emoji`, `status` and
`inlineCard`, with `strong` and `textColor` as the only marks. The rest of the
documented set is handled alongside them.

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

Three decisions:

- **Text is not escaped.** The output is read in a terminal, where a backslash
  in front of every `*` and `_` costs more than the rare sentence it protects.
- **Underline, text colour and sub/superscript have no Markdown.** Their text
  passes through unmarked rather than being dropped.
- **An unknown node keeps its text.** ADF's node list is the most likely thing
  here to change, and a node this doesn't recognise must not swallow the
  sentence inside it.

### Making `show` valid Markdown

Checked against pandoc, and it took more than picking a character for the rules:

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

## fzf_example.sh

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
- **`status` is read-only in zsh**, being an alias for `$?`, so no local
  variable may be called that. The file is `#!/bin/bash` but gets sourced into
  zsh, so it's written for both.

## Tests

The HTTP layer is tested against a canned TCP server: that the Cookie header
goes out verbatim, the rate-limit retry and its budget, the three shapes a dead
session takes (`401`, a redirect to the login page, an HTML page where JSON was
expected), that a Jira error message is lifted out of its envelope, that a
status stays recognisable as a number so a caller can tell a 404 or 400 from a
real failure, and that both paginators walk to the end.

The rest covers the `.env` format, org resolution, the board-ownership filter,
the response parsing, and the ADF renderer node by node — nesting of marks,
nested lists, tables with ragged rows, the inline chips, and that an unknown
node keeps its text.
