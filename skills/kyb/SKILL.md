---
name: kyb
description: >-
  Our internal infrastructure knowledge base — servers, services, architecture, who calls whom, deploys, ports, configs, decisions and house rules — plus incident reports and tasks.
  Search it before reasoning about our infra; write back verified facts, read entries and history, report and resolve incidents, and claim, block, relate, or close tasks as work progresses.
  Use for questions about our hosts, services, deploys, ports, configs, architecture, incidents, tasks, and operational decisions, including requests such as "remember this", "what do we know about X", "where does X run", "how do we deploy", "what is broken", "incident", "postmortem", "task", "idea", or "kyb".
---

# kyb — Know Your Business: our infrastructure knowledge base

**Why:** shared memory about our infrastructure and decisions — which servers exist and what
runs on them, how services are wired, how data flows, how we deploy, which rules we adopted
and why. Sessions end and context is lost; this base survives. Never invent answers about our
infra: **ask the base first**, and **write back everything you learned** while working.

**Where:** one shared HTTP service in docker (port 9310) — every agent on every machine sees
the same base. The CLI **`kyb`** is on PATH; it finds the server via `KYB_HOST`/`KYB_ADDR` env
or `~/.config/kyb/host` (written by the installer) and acts only as an HTTP client. If the
server is unavailable it reports the address and stops; it never opens SSH or attempts remote
repair. Every response is JSON. **If your client already lists `kyb_*` tools, use those** — they
are the same service over MCP, and this manual still describes what each one means. The canon is a git repo on the server: one md file per entry
(`knowledge/`, `incidents/`, `tasks/` by kind), one commit per change, **commit sha = version
id** — nothing is ever lost.

---

## 0. Keep a pointer in your own memory

If your runtime has persistent memory (a memory dir, `CLAUDE.md`, `AGENTS.md`), record one
line on first use: *shared infra knowledge base, CLI `kyb` — query it before reasoning about
our servers/services/deploys, write back what you learn; manual in the skill.* Store the
pointer, never a copy of the knowledge — the base changes, and a stale copy is worse than
none. (`skills/install.sh` already injects this into every agent's global instructions.)

---

## 1. The default loop: ask → assess → enrich

Whenever you touch one of our services, servers or deploys — a base nobody feeds is worthless.

**Ask first.** Try a couple of phrasings and the tag; one query is not a search:
```bash
kyb query "orders_api clickhouse"
kyb query "" --tag orders-api
```

**Assess what came back.**

| Found | Do |
|---|---|
| Nothing | You are the first → **write a new entry** once the facts are verified. |
| Thin entry, you now know more | **Enrich**: `kyb get <key>`, extend, write back under the **same key**. |
| Contradicts the code/host | **Reality wins.** Update it; note in the body what changed and when. |
| Complete and correct | Use it, write nothing. Never re-save an unchanged entry. |

**Enrich before you finish.** Not "what I did today" — **what is true about the system
afterwards**. Only verified facts (read the code, saw the config, ran the command); mark real
uncertainty explicitly ("not verified: probably X") instead of stating it flatly.

---

## 2. What is worth writing down

What a new agent would otherwise rediscover by reading the whole repo and inspecting hosts:

- **Architecture and topology** — what each service is, which host, which ports.
- **Who calls whom, how data flows** — protocols, directions, what breaks downstream.
- **Deploy and operations** — how it ships, where data lives, restart, logs, volumes.
- **Configuration** — meaningful env vars, config paths, credential **pointers** (never values).
- **Decisions and rationale** — the *why* stops the next agent from redoing a settled argument.
- **Gotchas** — non-obvious behaviour that cost time.

Skip: what the code says plainly, one-off task logs, anything stale next week.

### Language: one canonical language

**Keys, titles, bodies and tags are English** — the base keeps one canonical language no
matter which languages people ask in. A base written in two languages fragments: the same
concept lands under two keys with no token overlap, and half the knowledge goes silently
missing. Semantic search bridges *queries* in any language; it cannot repair a split base.
Keep the literal terms people search by verbatim in the body — hostnames, service names,
ports, table names, env vars — they anchor exact lookups.

### Key naming

Keys are the deduplication mechanism — predictable keys land the next agent on the same key
instead of a synonym:

| Pattern | Example | For |
|---|---|---|
| `<service>-architecture` | `orders-api-architecture` | what a service is and how it is built |
| `<service>-deploy` | `web-app-deploy` | how it ships and runs |
| `<host>-services` | `host-a-services` | what lives on a host |
| `<area>-<topic>` | `nats-streams` | cross-cutting rules and gotchas |
| `<decision>` | `why-tantivy-not-mongo` | a settled decision and its rationale |

Tags carry the scope (`infra`, `nats`, `host-a`) — no projects, no namespaces. Link related
entries with `[[other-key]]` so the graph stays walkable.

### Shape of a good entry

```bash
kyb add --key orders-api-architecture --title "orders_api: gRPC gateway to ClickHouse and Mongo" \
        --tags infra,orders-api,clickhouse --refs "see .env on host-a" <<'EOF'
What it is: a Go service, the single write path into ClickHouse.
Runs on host-a, listens on gRPC :9000.

Flow: clients -> gRPC orders_api -> write buffer -> ClickHouse (host-a).
Deploy: git push -> CI -> docker pull -> docker compose on host-a.

Gotcha: batches flush on size AND on a timer; a stuck flush shows up as growing
memory long before ClickHouse complains.

Related: [[nats-streams]], [[host-a-services]].
EOF
```

Self-contained, states the flow, records the trap, points at credentials without containing
them, links its neighbours.

---

## 3. Data model

One entry = one fact under a stable key.

| Field | Meaning |
|---|---|
| `key` | slug `[a-z0-9-]` ≤100 — the identity; writing the same key again **overwrites** (new version, never a duplicate) |
| `title` | one-line headline; weighs double in search |
| `body` | markdown; link related entries with `[[other-key]]` |
| `tags` | scope and facets, case-insensitive |
| `refs` | pointers to secrets and sources ("see .env on host-a") — **never the secrets** |
| `updated_at` | set by the server, only on real change |

Rejected with `400`: non-slug key · empty title · body/refs that look like a secret
(`ghp_…`, `sk-…`, `AKIA…`, `-----BEGIN … PRIVATE KEY`, `password: …`).

`kind`: `knowledge` (default) · `incident` (§7) · `task` (§8).

**Lifecycle rule (all kinds): the canon holds only what is live.** Closing an incident
(`resolved`) or a task (`done`/`dropped`) *archives* it — the file leaves the tree, but the
final version stays in the **default search** and in `kyb get` (marked `"archived": true`).
The intermediate states (`mitigated`, `in_progress`, `blocked`) are not closings: those
entries stay in the canon. `kyb rm` on knowledge is a *retraction*
("this was wrong"): it drops out of the default search; history keeps it, as always.

---

## 4. Write and update

```bash
kyb add --key nats-streams --title "NATS: always CreateOrUpdateStream" --tags nats,infra <<'EOF'
Always use CreateOrUpdateStream, never assume a stream is already configured.
Related: [[orders-api-architecture]].
EOF
# -> {"key":"nats-streams","sha":"<version id>","changed":true,"action":"created"}
```

Body on stdin. Re-running with identical content is a safe no-op (`changed:false`, no commit).

**Updating is the same `add` with the same key** — a wholesale replace; the old version stays
in history. Send the whole body and pass `title`/`tags`/`refs` again or they are wiped: so
`kyb get` first, keep what is still true, add your part. A fact that went stale but is useful
as history → update it; `kyb rm` only for entries that were simply wrong.

---

## 5. Search

```bash
kyb tags                          # the topic map — start here when unsure how it is labelled
kyb query "nats streams"          # current knowledge + archived reports, ranked
kyb query "config" --tag infra    # tag filter (several tags = AND)
kyb query "deploy" --recent       # order by commit time instead of relevance
kyb query "" --limit 50           # empty query = list everything, newest first
kyb query "old rule" --history    # search EVERY version, current and deleted included
```

Hits come back with the **full body** plus `sha` (→ `kyb get --at`), `committed_at`,
`is_head`, `score`; incident/task hits carry their extra fields (`status`, `resolution`,
`priority`, `blocked_reason`, …).
Free text matches keys and tags too: `kubernetes` finds an entry that is only *tagged*
kubernetes, `orders api` reaches the key `orders-api-deploy`.

- **Hybrid**: BM25 (title ×2, tags/keys ×1.5) fused with multilingual vectors — **ask in any
  language, or in words the entry never uses**, and still land on it; exact technical terms
  win on the lexical side. `"semantic":true` in the response means vectors took part
  (`--recent`, `--history`, `--as-of`, `--changed-between`, empty queries and a model-less
  server are lexical-only: vectors exist for the current version of each key only).
- **Default scope = the latest version of every key**: live entries plus archived (closed)
  incidents/tasks — "how did we fix this last time" needs no flag. Retracted knowledge is the
  one thing excluded. **`--history` = every version of every key, the current one included**,
  one hit per version and no time scoping; the default scope never returns a superseded one.
- Broken query syntax never fails the request — the parser is lenient.

---

## 6. Read an entry and its history

```bash
kyb query "..." --as-of 2026-08-01  # what the base said then: the version current at that
                                    # instant per key (never a superseded one that still had
                                    # the word), knowledge deleted by then absent
kyb query "" --changed-between 2026-08-01,2026-08-07   # what moved while you were away
kyb diff <key> [--from <rev>] [--to <rev>]             # what changed in it, field by field
# these three check /healthz first: an older server would ignore the parameter
# and answer with today's data, so the CLI refuses instead of asking
kyb get nats-streams              # current version (archived entries carry "archived":true)
kyb get nats-streams --at <sha>   # the version at a specific commit
kyb history nats-streams          # every version, newest first: sha · time · message · added|modified|deleted
kyb export --out kyb.jsonl        # the whole base as of one commit, active + archived + retracted,
                                  # one JSON line per key; exits 1 unless the export is complete
```

What moved where: `kyb query "old term" --history` → take the hit's `sha` → `kyb get <key> --at <sha>`.

---

## 7. Incident reports

An incident is an operational event, not a fact: *X broke, here is the effect, how to live
with it, how it ended.* Same mechanics, own kind, tied to the knowledge entries it concerns.

**File one** when something broke or degraded and it cost you (or will cost the next agent)
time. **Before infra work**, triage: `kyb incidents` + `kyb tasks` — an open incident on the
service you are about to touch changes what you do.

A report is a **control panel, not a story**: everything the next agent should be able to DO,
in executable form. `kyb incident --template` prints the skeleton; a bare report is accepted
but the reply lists `hints` for the missing parts — empty `hints` = structurally complete.

```bash
kyb incident --key inc-2026-07-22-orders-api-oom --title "orders_api OOM-killed every ~2h on host-a" \
        --service orders_api --hosts host-a --severity high \
        --detection "docker inspect orders_api --format '{{.RestartCount}}' growing; healthy = stable" \
        --affected '[{"scope":"orders-writes","from":"2026-07-22T06:00:00Z","to":"2026-07-22T09:30:00Z"}]' \
        --knowledge orders-api-architecture,host-a-services <<'EOF'
Symptom: client writes stall every ~2h, container restarts on its own.
Impact: write buffer grows and flushes late during each restart window.
Root cause (suspected, not verified): leak in the batch flush path.
Cure / runbook:
  1. docker restart orders_api          # SAFE — agent may do this alone
  2. verify: detection returns healthy
  3. raise memory limit in compose      # APPROVAL — only with a human
Follow-ups:
- [ ] find the leak in the write buffer flush path
EOF
```

| Field | Meaning |
|---|---|
| `--key` | `inc-<yyyy-mm-dd>-<slug>`; the `inc-` prefix is enforced in both directions |
| `--service` | required; what broke — exact filter + searchable |
| `--severity` | required: `low` `medium` `high` `critical` |
| `--status` | `open` (default) → `mitigated` → `resolved` |
| `--hosts` | where it runs/broke |
| `--knowledge` | related knowledge keys; dangling links allowed, reported back as `unknown_knowledge` |
| `--detection` | **the most actionable field**: command/query + expected healthy result — "is it still happening?" in 30 s |
| `--affected` | machine-readable poisoned windows `[{scope,from,to}]` UTC — consumers exclude them programmatically |
| `--started`/`--detected` | RFC3339; `detected_at` defaults to filing time, `mitigated_at`/`resolved_at` stamped by the server |

Body sections — each answers "what can the next agent DO": **Symptom** (verbatim, the search
anchor) · **Impact** · **Root cause** with confidence `verified|suspected|unknown` ·
**Cure/runbook** as numbered steps marked `SAFE` (agent alone) or `APPROVAL` (human required) ·
**Actions log** (action + its verification) · **Follow-ups** as `- [ ]` checkboxes (the server
counts open ones). Same rules as knowledge: canonical language, verified facts, no secrets.

**Updating** = the same `kyb incident` with the same key (wholesale replace; server-stamped
timeline fields are inherited). The whole timeline stays in `kyb history <key>`.

### Lifecycle: open → mitigated → resolved

```bash
kyb incidents                     # LIVE reports (open + mitigated), freshest on top
kyb incidents --all               # + archived (resolved), at the bottom
kyb incidents --status resolved   # an explicit status looks into the archive by itself
kyb incidents --open-followups    # loose ends worth picking up, archived included
kyb resolve inc-2026-07-22-orders-api-oom <<< "Raised the limit to 2G; fixed the flush leak."
kyb resolve inc-2026-07-22-orders-api-oom --status mitigated --resolution "Hourly restart cron while the fix bakes."
```

`kyb resolve` flips the status and records **how it ended**; everything else stays as stored.
**Closing requires a resolution** — "it just went away" teaches nobody anything — and
**archives the report** (§3). Closing over unfinished `- [ ]` succeeds with a warning — finish
or reassign them. Amend a closed report with another `kyb resolve`; re-filing the same key or
parking it with `--status mitigated` reopens it.

**After resolving, fold the durable lesson back into knowledge** (`kyb get` → extend →
`kyb add`): knowledge is *what is true now*, an incident is *what happened*.

Incidents show up in plain `kyb query` (hits carry `kind`/`status`/`resolution`); narrow with
`--kind incident [--status open] [--service X]`. **Resolutions are searchable** — "how did we
fix this last time" is one query away. `kyb health` reports `open_incidents`.

---

## 8. Tasks and ideas

The third kind: a short actionable note — "do X", "look into Y", an idea worth keeping — with
the same close-with-an-outcome discipline and none of the ceremony (no service, severity or
detection). Keys start with `task-`, no date; an idea is a task tagged `idea`.

```bash
kyb task --key task-raise-log-retention --title "Raise container log retention to 72h" \
         --tags observability --priority high [--knowledge web-app-architecture] <<'EOF'
Short retention loses evidence during incidents.
- [ ] measure current log volume first
EOF

kyb task-status task-raise-log-retention --status in_progress --assignee agent-a   # picked it up
kyb task-status task-swap-disk --status blocked --blocked-reason "waiting on the replacement disk"

kyb tasks                          # LIVE tasks (open + in_progress + blocked), freshest on top
kyb tasks --priority critical      # exact rank filter
kyb tasks --status blocked         # what is stuck, and on what
kyb tasks --assignee agent-a       # what one owner is holding
kyb tasks --parent task-migrate-logs   # the children of one task
kyb tasks --all                    # + archived (done/dropped), at the bottom
kyb tasks --open-followups         # loose ends inside tasks, archived included
kyb done task-raise-log-retention <<< "Raised to 72h with a 2G disk budget."
kyb done task-try-foo --status dropped <<< "Obsolete after the rewrite."
```

### Agent workflow — mandatory

A shared task list only works if it says who is doing what **right now**. Whenever you touch a
task from this base:

| Moment | Do exactly this |
|---|---|
| **You pick a task up** | `kyb task-status <key> --status in_progress --assignee <your label>` — *before* you start, so no second agent picks up the same work |
| **You get stuck** | `kyb task-status <key> --status blocked --blocked-reason "<what it waits on>"` |
| **You get unstuck** | `kyb task-status <key> --status in_progress` — the stale reason clears itself |
| **You hand it back** | `kyb task-status <key> --status open --assignee ""` |
| **You finish it** | `kyb done <key> <<< "what came of it"` — **never** `task-status`: closing demands an outcome |
| **You split work off** | file the child with `--parent <parent-key>`, so the tree shows what belongs to what |

`kyb task-status` is a **partial** update: it sends the status and nothing else. Title, body,
tags, priority, `--knowledge` links and refs stay exactly as stored — you never have to fetch,
re-paraphrase or resend the task you are claiming (which is how bodies get silently truncated).
Rewriting the task itself is still `kyb task` with the same key.

**The assignee label**: short, stable and reused across sessions (`agent-a`, `codex-cli`,
`ops-rotation`, or your organization's ordinary responsible-person/team label). It must answer
"who owns this work?" without embedding an email, host name, ephemeral session id, filesystem
path or other sensitive identifier. The base is shared and version-controlled forever;
secret-looking values are rejected (400).

**Every update is git-versioned.** A re-assignment overwrites nothing: `kyb history <key>` lists
every version and `kyb get <key> --at <sha>` replays the entry as it stood at that commit — who
held it, in which status, with which reason. "Who had this before me, and where did they stop"
is always answerable.

**Lifecycle `open → in_progress → blocked → done | dropped`.** Only `done` and `dropped` are
terminal: they **require a resolution** — what came of it, or why it was dropped ("dropped:
obsolete" is knowledge too) — and archive the task (§3); the server stamps `resolved_at`.
`in_progress` and `blocked` are work in flight: no resolution, no archive, still listed and
still counted by `kyb health` (`open_tasks` = all three live statuses).

| Field | Meaning |
|---|---|
| `--priority` | optional rank: `low` `medium` `high` `critical`. Empty = unranked, and it stays unranked — nothing infers one. Exact filter: `kyb tasks --priority high` (the HTTP API takes the same `?priority=` on `/search`). |
| `--blocked-reason` | optional: what the task waits on. **Only with `--status blocked`** — setting it on any other status is rejected (400), and moving off `blocked` clears it, so a task never advertises a block it is no longer in. |
| `--assignee` | optional: who holds it now — a short public label (≤80 chars, single line, no secrets). Empty = unclaimed, and it stays unclaimed; nothing infers an owner. Exact filter: `kyb tasks --assignee agent-a` (`?assignee=` on `/tasks` and `/search`). |
| `--parent` | optional: the `task-` key this one hangs under; empty = top-level. Must be a valid `task-` key and cannot create a parent cycle (400). A parent that does not exist **yet** is allowed — a child may be filed first — and comes back as `unknown_parent` in the reply. Exact filter: `kyb tasks --parent <key>`. |

Updating is the same `kyb task` with the same key (wholesale replace — resend `--priority`
and `--tags` or they are wiped; the deadline fields below are the exception and are kept when
left out); moving a task between live statuses is `kyb task-status` (partial, resends nothing).
Follow-ups from a resolved incident that deserve their own life → file them as tasks linked
via `--knowledge`.

### Deadlines

A date written into a title as prose ("renew before 03/14") can be neither sorted nor noticed.
Give the task a due date and the server surfaces it on its own — in **every** read answer.

```bash
kyb task --key task-renew-api-cert --title "Renew the api.example.com certificate" \
         --due 2026-03-14 --hard [--remind-days 30] <<< "Expires on the 14th."
kyb task-status task-renew-api-cert --due 2026-03-21      # move it: a new version, `kyb diff` shows it
kyb task --key task-answer-quote --title "Answer the vendor's quote" \
         --waits-on "the vendor's quote arrives" --expected 2026-03-20 <<< "Within 10 business days."
kyb task-status task-answer-quote --due 2026-03-24 --expected ""   # the quote came: the real date

kyb deadlines                      # open tasks with a date: OVERDUE / THIS WEEK / LATER
kyb deadlines --within 14d         # due within 14 days (late ones always count)
kyb deadlines --overdue | --hard | --tag tls | --assignee agent-a | --json
```

| Field | Meaning |
|---|---|
| `--due` | `YYYY-MM-DD`, or a date-time with its zone (`2026-03-14T17:00:00-07:00`). A bare date is due on that day **on the caller's calendar**; a date-time is late once that instant has passed. A time without a zone is rejected. `""` clears it. |
| `--hard` / `--no-hard` | a miss has consequences outside the team: an expiry, a contractual or regulatory date, a third party's cut-off. Hard deadlines come first and are surfaced earlier. |
| `--remind-days N` | how many days ahead it starts being surfaced. Default 14, or 30 for a hard deadline; `""` returns it to the default. |
| `--waits-on` + `--expected` | the real date depends on an event. `expected` is a provisional date, reminded on (and marked provisional) until you set `--due`. |

Deadline fields belong to tasks only. **A flag left out keeps what the task already says** —
on `kyb task` too — so rewriting a task to fix its body never drops its due date; pass the flag
with `""` to clear it. `kyb task-status --due ...` needs no `--status`: the task keeps its own.

**The block in every answer.** `query`, `get`, `tasks` and `incidents` — over the CLI, HTTP and
MCP alike — carry a `deadlines` object when an open task is late or inside its reminder window:
`today`, `count`, `overdue`, `more`, and up to 5 `items` (key, title, due, `days_left` — negative
is late — `overdue`, `hard`, `provisional`, `waits_on`), hard ones first, then by date; a query
with `--tag` puts that area's deadlines first. Nothing late or near = no block at all. The CLI
prints it as a short banner on **stderr**, so stdout stays the server's JSON:

```
kyb deadlines, today 2026-03-16: 1 overdue, 1 due soon
  2d late    HARD task-renew-api-cert  Renew the api.example.com certificate  (due 2026-03-14)
  in 4d      task-answer-quote  Answer the vendor's quote  (expected 2026-03-20, waits on the vendor's quote arrives)
```

When you see it: act on it, or tell the person you are working for. `--no-deadlines` on any
read leaves it off (scripts, bulk reads); over HTTP that is `?deadlines=false`, over MCP
`"deadlines": false`. Closing the task removes it from every deadline answer at once.

**Whose "today".** The CLI sends the date of the machine it runs on (`X-KYB-Today`; `KYB_TODAY`
overrides it). A caller that sends none — an MCP client, a script — gets the server's date in
its own time zone (`TZ` in the server's environment).

**Deadlines at session start (optional).** So that they are seen even in a session that never
queries the base, have the agent's session-start hook print them. Claude Code,
`~/.claude/settings.json`:

```json
{"hooks": {"SessionStart": [{"hooks": [{"type": "command",
  "command": "kyb deadlines --within 14d 2>/dev/null || true"}]}]}}
```

What a `SessionStart` hook prints is added to the session's context. Other agents: run the same
command from whatever their start hook or startup script is.

---

## 9. The rest

```bash
kyb rm <key>       # knowledge: retract a wrong entry. incident/task: archive it
                   # (resolve/done is the normal path, rm is the manual override)
kyb reindex        # full index rebuild from git (the service also does it on every start)
kyb health         # {"ok","entries","open_incidents","open_tasks","index_docs","last_commit"}
                   # open_tasks = open + in_progress + blocked (every task still in flight)
```

For a client that speaks MCP, the server serves it directly — nothing to install:

    claude mcp add --transport http kyb http://<host>:9310/mcp

The tool list comes from the server, so it always matches the routes that actually exist, and
upgrading the server is how agents get new tools. `?readonly=1` drops the writing tools.
`kyb rm` and `kyb reindex` are not on the MCP surface; they stay CLI-only.

### Exit codes

The CLI reports outcomes through deterministic exit codes:
- `0` — Success.
- `1` — The server rejected the request (`4xx`), or the command line was malformed.
- `2` — Server error (`5xx`).
- `3` — The server could not be reached (connection refused, timeout, DNS failure).

---

## 10. Write rules (governance — mandatory)

1. **ALWAYS `kyb query` before `add`.** Found something close → overwrite the **same key**;
   a new key only for genuinely new knowledge. Duplicates kill the base.
2. **A key is a stable entity slug** (§2), not a date and not a paraphrased sentence.
3. **One fact = one key.** Growing too big → split and link with `[[key]]`.
4. **Only verified facts** — read it, saw it, ran it — or mark it unverified in the body.
5. **Secrets only as `refs` pointers.** The service rejects secret-looking bodies (400).
6. **Bodies self-contained**: what, why, how to apply — readable in six months without this chat.
7. Something vanished or moved → don't guess, dig history (`--history`, `--at`).
8. **Broke something / found it broken → file an incident** (§7), link `--knowledge`, close
   with a real resolution, fold the lesson back into knowledge.
9. **Working a task → claim it first** (`--status in_progress --assignee <label>`), block it
   with a reason when it stalls, close it with `kyb done`, and hang child work off
   `--parent` (§8). An unclaimed in-flight task is two agents doing the same work twice.

---

## 11. When something breaks

- Not responding → the CLI is a pure HTTP client: it reports the configured address and stops.
  Diagnose or restart the server through that deployment's own operator runbook; the public
  client never opens SSH or attempts remote repair.
- Index drifted from hand-edited canon files → `kyb reindex`.
- Inspect canon history through `kyb history <key>` and `kyb get <key> --at <sha>`; do not
  bypass the service by guessing a deployment's filesystem layout.
- Upgrade or restart only through that deployment's documented operator workflow. The public
  skill deliberately contains no host-specific remote commands or credential plumbing.
- The skill is installed from one source — `skills/install.sh` in the kyb repo. Edit there and
  reinstall; never patch the agents' copies by hand.

## Access tokens

A server may ask for a token (`kyb whoami` shows `"auth": "required"` or `"compat"`). The CLI
sends the one in `KYB_TOKEN`, else the one `kyb login` stored in `~/.config/kyb/token`:

```bash
echo "$TOKEN" | kyb login     # checked against the server before it is kept
kyb whoami                    # your id, name and role: read | write | admin
```

Your role decides what you may do: `read` searches and reads, `write` also adds, edits, files
and closes, `admin` also retracts knowledge and reindexes. Every write is committed under the
token's name, so `kyb history <key>` says who changed what. A `401` means the server does not
know the token (ask the owner for one, never invent or borrow one); a `403` names the role the
action needs. Never put a token into an entry, a command line, a log or a message. A server
behind TLS is addressed whole with `KYB_URL=https://...`.

## Who wrote it: attribution

Every write is committed with trailers naming the agent, its session and the workspace it
ran in, as far as the client could tell. `kyb history <key>` shows them as `by`:

```json
{"sha": "…", "change": "modified", "by": {"agent": "claude", "session": "18970ebf-…", "workspace": "knowyourbusiness/scripts", "via": "cli"}}
```

- `via: cli` — the `kyb` command read its environment: the agent from `PROD_PROVIDER`,
  `CLAUDECODE`, `CODEX_*` or `ANTIGRAVITY_*`; the session from `CLAUDE_CODE_SESSION_ID`,
  `CODEX_THREAD_ID`, `ANTIGRAVITY_CONVERSATION_ID` or `PROD_PRIMARY_SESSION_KEY`; the workspace
  from the git repository the command ran in (`<repo>/<path inside it>`), never an absolute path.
- `via: hook` — Claude Code calls the kyb MCP tools over HTTP, so a PreToolUse hook
  (`~/.local/bin/kyb-hook`, registered by the installer) adds the same three to each call. It
  changes only the call's input; whether the call may run stays with your permission policy.
- `via: meta` — the MCP client's own call metadata named the conversation (Antigravity, Codex).
- `via: headers` — a client that sent `X-Kyb-Agent` / `X-Kyb-Session` / `X-Kyb-Workspace` with its
  MCP requests and had neither a hook nor metadata. A field one source left empty is filled from
  the next (hook, then metadata, then headers); `via` names where the agent came from.
- `kyb whoami` shows what a write from here would be attributed to. Override with `KYB_AGENT`,
  `KYB_SESSION`, `KYB_WORKSPACE` when the environment is not yours (a script acting for an agent,
  or an agent started from inside another agent's shell, which inherits the outer one's markers).

This is forensics, not identity: it grants nothing and is only what the client claimed. The
signer of the commit (`author`) is the token holder, when a token was presented.
