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
repair. Every response is JSON. The canon is a git repo on the server: one md file per entry
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
kyb query "old rule" --history    # search ALL superseded versions, deleted included
```

Hits come back with the **full body** plus `sha` (→ `kyb get --at`), `committed_at`,
`is_head`, `score`; incident/task hits carry their extra fields (`status`, `resolution`,
`priority`, `blocked_reason`, …).
Free text matches keys and tags too: `kubernetes` finds an entry that is only *tagged*
kubernetes, `orders api` reaches the key `orders-api-deploy`.

- **Hybrid**: BM25 (title ×2, tags/keys ×1.5) fused with multilingual vectors — **ask in any
  language, or in words the entry never uses**, and still land on it; exact technical terms
  win on the lexical side. `"semantic":true` in the response means vectors took part
  (`--recent`, `--history`, empty queries and a model-less server are lexical-only).
- **Default scope = the latest version of every key**: live entries plus archived (closed)
  incidents/tasks — "how did we fix this last time" needs no flag. Retracted knowledge is the
  one thing excluded. **`--history` = superseded versions only**; the two modes never overlap.
- Broken query syntax never fails the request — the parser is lenient.

---

## 6. Read an entry and its history

```bash
kyb get nats-streams              # current version (archived entries carry "archived":true)
kyb get nats-streams --at <sha>   # the version at a specific commit
kyb history nats-streams          # every version, newest first: sha · time · message · added|modified|deleted
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
and `--tags` or they are wiped); moving a task between live statuses is `kyb task-status`
(partial, resends nothing). Follow-ups from a resolved incident that deserve their own life →
file them as tasks linked via `--knowledge`.

---

## 9. The rest

```bash
kyb rm <key>       # knowledge: retract a wrong entry. incident/task: archive it
                   # (resolve/done is the normal path, rm is the manual override)
kyb reindex        # full index rebuild from git (the service also does it on every start)
kyb health         # {"ok","entries","open_incidents","open_tasks","index_docs","last_commit"}
                   # open_tasks = open + in_progress + blocked (every task still in flight)
```

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
