# fzf_example.sh — the shell side of myjira, as one fzf-driven picker.
#
# Meant to be sourced, not run:
#
#     . /path/to/myjira/fzf_example.sh
#     x_jira                     # walk projects → sprints → tickets → nvim
#     x_jira_open_ticket BTE-915 # or straight to one ticket
#     x_jira list-projects       # with arguments it just runs the command
#
# Works in bash and zsh. Needs fzf and nvim on PATH.
#
# This is a copy of what lives in the author's own shell setup, kept here so the
# README has something to point at. Set these two before sourcing to override
# them — X_JIRA_BIN to an absolute target/release/myjira if it isn't on PATH:
X_JIRA_BIN=${X_JIRA_BIN:-myjira}
X_JIRA_ENV=${X_JIRA_ENV:-$HOME/.config/myjira/.env}

# ---------------------------------------------------------------------------
# x_jira — one walk through Jira: projects, then that project's sprints where it
# has any, then tickets, then the ticket itself in nvim. Esc steps back exactly
# one level everywhere, so the whole thing needs two keys.
#
# A project without sprints (Kanban) skips the sprint level altogether: its
# tickets are the only list there is, and esc from them goes straight back to
# the projects.
# ---------------------------------------------------------------------------

# Shared fzf setup for every level.
#
# --ansi keeps the colours the list commands emit, and fzf strips them back out
# of the line it prints on stdout, so the key in the first column can be handed
# straight to the next command without unpicking escape codes.
#
# --header-lines=1 turns each command's own column header into fzf's header:
# still visible, no longer selectable. --no-sort keeps the order the tool chose
# — projects by key, sprints active-first — instead of re-ranking by match
# score while you type.
_x_jira_fzf()
{
  local header=$1
  shift
  fzf --ansi --no-sort --style minimal --header-lines=1 --header "$header" "$@"
}

# The first column of a picked line: project key, sprint id or ticket key, which
# is what the next myjira command takes.
_x_jira_key()
{
  printf '%s' "${1%% *}"
}

# myjira says "no tickets in X" and reports real failures on stderr, both of
# which fzf would swallow — with nothing on stdin it simply shows an empty list.
# So stderr is kept and shown when a level comes back empty: the difference
# between "this sprint has nothing in it" and "the cookie expired".
_x_jira_note()
{
  [[ -s $1 ]] && cat "$1" >&2
  return 0
}

# Fetches a level's rows into $1 unless they are already there. Kept for as long
# as the level is on screen: coming back from a ticket must not re-fetch 1370
# rows, so ctrl-r is the only thing that asks Jira again.
#
# 0 with rows, 2 when the list is legitimately empty, 1 when the call failed —
# the caller decides what an empty list means, which is the whole difference
# between a Kanban project and a broken one.
_x_jira_load()
{
  local file=$1 err=$2
  shift 2
  [[ -s $file ]] && return 0
  : > "$err"
  printf 'loading %s …\n' "$*" >&2
  "$X_JIRA_BIN" --env "$X_JIRA_ENV" "$@" > "$file" 2>"$err" || return 1
  [[ -s $file ]] && return 0
  return 2
}

# Renders one ticket into a file and opens it.
#
# nvim reads a file rather than a pipe, and the name is what it shows in the
# statusline, so the ticket key goes into it and .md gets the markdown that
# myjira's ADF renderer produces highlighted. --no-color matters: ANSI escapes
# are decoration in a terminal and literal rubbish in a buffer.
#
# -R because this is a rendering of a ticket and not a file that can be written
# back — read-only keeps an accidental :w from looking like it saved something.
_x_jira_open()
{
  local ticket=$1 err=$2 file rc=0
  file=$(mktemp --suffix=.md -t "$ticket-XXXXXX") || return 1
  : > "$err"
  if "$X_JIRA_BIN" --env "$X_JIRA_ENV" show "$ticket" --no-color > "$file" 2>"$err"; then
    nvim -R "$file"
  else
    _x_jira_note "$err"
    rc=1
  fi
  rm -f "$file"
  return $rc
}

# The same thing the picker does when a ticket is opened, for when the key is
# already known: x_jira_open_ticket BTE-915. Lower case is fine — myjira
# upper-cases the key itself, since Jira's own lookup won't.
x_jira_open_ticket()
{
  local err rc
  if (( $# != 1 )); then
    echo 'usage: x_jira_open_ticket <TICKET>' >&2
    return 2
  fi
  err=$(mktemp -t x_jira-err-XXXXXX) || return 1
  _x_jira_open "$1" "$err"
  rc=$?
  rm -f "$err"
  return $rc
}

_x_jira_tickets()
{
  local project=$1 sprint=$2 err=$3
  local rows line key ticket header rc
  local -a args
  args=(list-tickets "$project")
  [[ -n $sprint ]] && args+=("$sprint")

  # The sprint list carries a backlog row whose id is the word itself, so it
  # travels through here like any sprint — it just doesn't read as one.
  header="$project tickets — enter: nvim · ctrl-r: reload · esc: back"
  [[ -n $sprint ]] && header="$project sprint $sprint — enter: nvim · ctrl-r: reload · esc: back"
  [[ $sprint == backlog ]] && header="$project backlog — enter: nvim · ctrl-r: reload · esc: back"

  rows=$(mktemp -t x_jira-tickets-XXXXXX) || return
  while :; do
    _x_jira_load "$rows" "$err" "${args[@]}"
    rc=$?
    if (( rc != 0 )); then
      _x_jira_note "$err"
      break
    fi
    line=$(_x_jira_fzf "$header" --prompt='ticket> ' --expect=ctrl-r < "$rows")
    # With --expect the first line is the key that was pressed, empty for enter;
    # esc prints nothing at all, which is how going back is recognised.
    key=$(printf '%s\n' "$line" | sed -n 1p)
    ticket=$(_x_jira_key "$(printf '%s\n' "$line" | sed -n 2p)")
    [[ -z $key && -z $ticket ]] && break
    if [[ $key == ctrl-r ]]; then
      : > "$rows"
      continue
    fi
    [[ -n $ticket ]] || break
    _x_jira_open "$ticket" "$err"
  done
  rm -f "$rows"
}

_x_jira_sprints()
{
  local project=$1 err=$2
  local rows line key sprint rc
  rows=$(mktemp -t x_jira-sprints-XXXXXX) || return

  _x_jira_load "$rows" "$err" list-sprints "$project"
  rc=$?
  if (( rc == 1 )); then
    _x_jira_note "$err"
    rm -f "$rows"
    return
  fi
  if (( rc == 2 )); then
    # Kanban and the like: no sprint level to show, and esc from the tickets
    # goes back past it to the projects.
    rm -f "$rows"
    _x_jira_tickets "$project" '' "$err"
    return
  fi

  while :; do
    line=$(_x_jira_fzf "$project sprints — enter: tickets · ctrl-a: all tickets · esc: back" \
                       --prompt='sprint> ' --expect=ctrl-a < "$rows")
    key=$(printf '%s\n' "$line" | sed -n 1p)
    sprint=$(_x_jira_key "$(printf '%s\n' "$line" | sed -n 2p)")
    [[ -z $key && -z $sprint ]] && break
    if [[ $key == ctrl-a ]]; then
      _x_jira_tickets "$project" '' "$err"
    else
      [[ -n $sprint ]] || break
      _x_jira_tickets "$project" "$sprint" "$err"
    fi
  done
  rm -f "$rows"
}

# With arguments this stays out of the way and just runs the command; the picker
# is what you get for asking for nothing in particular.
#
# --env names the credentials file outright, as with x_slack: a missing one then
# fails here rather than later as a puzzling "no Jira instance configured".
x_jira()
{
  if (( $# > 0 )); then
    "$X_JIRA_BIN" --env "$X_JIRA_ENV" "$@"
    return
  fi

  local err rows line key project rc
  err=$(mktemp -t x_jira-err-XXXXXX) || return
  rows=$(mktemp -t x_jira-projects-XXXXXX) || { rm -f "$err"; return; }

  while :; do
    _x_jira_load "$rows" "$err" list-projects
    rc=$?
    if (( rc != 0 )); then
      _x_jira_note "$err"
      break
    fi
    line=$(_x_jira_fzf 'projects — enter: open · ctrl-r: reload · esc: quit' \
                       --prompt='project> ' --expect=ctrl-r < "$rows")
    key=$(printf '%s\n' "$line" | sed -n 1p)
    project=$(_x_jira_key "$(printf '%s\n' "$line" | sed -n 2p)")
    [[ -z $key && -z $project ]] && break
    if [[ $key == ctrl-r ]]; then
      : > "$rows"
      continue
    fi
    [[ -n $project ]] || break
    _x_jira_sprints "$project" "$err"
  done
  rm -f "$err" "$rows"
}
