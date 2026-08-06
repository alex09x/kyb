---
name: kyb
description: Our internal infrastructure knowledge base — servers, services, architecture, who calls whom, deploys, ports, configs, decisions and house rules — plus incident reports. Search it ("what do we know about X" — kyb query), write and update facts (kyb add, overwrite by key), read an entry in full (kyb get), pull the change history from git (kyb history, search old versions with --history). File incident reports when something breaks (kyb incident), check what is broken right now (kyb incidents --status open), close them with an outcome (kyb resolve). Use it ALWAYS before guessing about our infra, and ALWAYS write back what you learned about a service, its architecture or its deploy while working. Triggers: «запомни», «что мы знаем про», «где у нас», «как мы делаем», «что сломано», «инцидент», "remember this", "incident", "postmortem", "kyb".
---

# kyb — Know Your Business: our infrastructure knowledge base

**Why it exists:** shared memory about our infrastructure and decisions — which servers exist
and what runs on them, how services are wired together and who calls whom, how data flows,
how we deploy, ports/paths/stack, which rules we adopted and why. Sessions end and context is
lost; this base is what survives. Never invent answers about our infra: **ask the base first**,
and **write back everything you learned** while working.

**Where it lives:** a docker service on the **booster** server (`$KYB_HOST:9310`, over
Tailscale). The base is **shared** — every agent (Claude Code, Codex, Antigravity) on every
machine sees it, so anything written here is instantly available everywhere. The canon is a
git repo on booster (`~/kyb/data/kyb-data`): one md file per entry (in `knowledge/`,
`incidents/` or `tasks/` by kind), one commit per change. **A commit sha is a version id**,
so nothing is ever lost — old versions stay searchable and retrievable.

CLI: **`kyb`** (on PATH, `~/.local/bin/kyb`), every response is JSON. The container runs
permanently; if it is ever down the CLI brings it up over ssh. Another instance:
`KYB_ADDR=host:port kyb ...`.

---

## 0. Put this base in your own memory

If your runtime has persistent memory (a memory directory, `CLAUDE.md`, `AGENTS.md`, project
notes), **record a pointer to this base there on first use** — one line is enough:

> There is a shared infrastructure knowledge base, CLI `kyb`. Query it before reasoning about
> our servers/services/deploys, and write back what you learn. Manual: `~/.claude/skills/kyb/SKILL.md`.

Why: your memory is loaded in every session, this skill is not — the pointer is what makes you
reach for the base at all. Store the pointer, never a copy of the knowledge: the base is the
single source of truth and it changes; a stale copy in memory is worse than no copy.
`skills/install.sh` already writes this pointer into the global instructions of every agent on
this machine, so if it is already there, leave it alone.

---

## 1. The default loop: ask → assess → enrich

Whenever you touch one of our services, servers or deploys, run this loop. It is the point of
the skill — a base nobody feeds is worthless.

**Step 1 — ask first.** Before reasoning about our infra, search. Try a couple of phrasings and
the tag, because one query is not a search:
```bash
kyb query "orders_api clickhouse"
kyb query "" --tag orders-api
```

**Step 2 — assess what came back.**

| What you found | What to do |
|---|---|
| Nothing | You are the first to learn this → **write a new entry** once you have verified the facts. |
| Thin entry (a line or two) while you now know the architecture, the flow, the gotchas | **Enrich it**: `kyb get <key>`, extend the body, write it back under the **same key**. |
| Entry contradicts what you just saw in the code/on the host | **Reality wins.** Update the entry, and say in the body what changed and when. |
| Complete and correct | Use it, write nothing. Do not re-save an unchanged entry. |

**Step 3 — enrich before you finish.** At the end of work that taught you something durable
about our systems, write it down. Not "what I did today" — that belongs in the chat — but
**what is true about the system afterwards**.

Enrich only with **verified** facts: things you read in the code, saw in a config, or confirmed
by running a command. Never record a guess. If something is genuinely uncertain, say so
explicitly in the body ("not verified: probably X") rather than stating it flatly.

---

## 2. What is worth writing down

Capture the knowledge that a new agent would otherwise have to rediscover by reading the whole
repo and ssh-ing around:

- **Architecture and topology** — which services exist, what each one is responsible for, which
  host it runs on, which ports it listens on.
- **Who calls whom and how data flows** — service A → NATS subject → service B → ClickHouse
  table. Protocols (gRPC/HTTP/NATS), directions, what breaks downstream when a link dies.
- **Deploy and operations** — how it ships (CI, image, compose), where the data lives, how to
  restart it, how to see the logs, which volumes matter.
- **Configuration** — meaningful env vars, config paths, where credentials live (**as pointers
  only**, never the values).
- **Decisions and their rationale** — why this stack, why this design was rejected. The *why*
  is what stops the next agent from redoing a settled argument.
- **Gotchas and traps** — non-obvious behaviour that cost time. "Tantivy needs delete+add for
  an upsert", "the container must run as the host uid or the git canon ends up root-owned".

Skip: things the code says plainly and repeats (a function's signature), one-off task logs,
anything that will be stale next week.

### Language: write entries in English

**Keys, titles, bodies and tags are English.** A bilingual base fragments: lexical search
matches words, not meanings, and «реестр знаний» shares no token with "knowledge base". Semantic
search now bridges that gap for *queries* — asking in Russian works — but it cannot repair a
base where the same concept is written two ways under two keys.

Consequences for you:
- Write English; ask in whatever language you like.
- Keep the literal terms people search by in the body verbatim — hostnames (`host-a`, `booster`),
  service names, ports, table names, env var names. They are the anchors that make an exact
  lookup land on the first result.

### Key naming

Keys are the deduplication mechanism, so keep them predictable — that way the next agent lands
on the same key instead of inventing a synonym:

| Pattern | Example | For |
|---|---|---|
| `<service>-architecture` | `orders-api-architecture` | what a service is and how it is built |
| `<service>-deploy` | `web-app-deploy` | how it ships and runs |
| `<host>-services` | `host-a-services` | what lives on a host |
| `<area>-<topic>` | `nats-streams`, `clickhouse-mutations` | cross-cutting rules and gotchas |
| `<decision>` | `why-tantivy-not-mongo` | a settled decision and its rationale |

Tags carry the scope (`infra`, `acme`, `nats`, `host-a`) — there are no projects or namespaces.
Link related entries with `[[other-key]]` so the graph stays walkable.

### Shape of a good architecture entry

```bash
kyb add --key orders-api-architecture --title "orders_api: gRPC gateway to ClickHouse and Mongo" \
        --tags acme,infra,orders-api,clickhouse --refs "see .env on host-a" <<'EOF'
What it is: a Go service, the single write path into ClickHouse for the ACME cluster.
Runs on host-a, listens on gRPC :9000.

Flow: clients -> gRPC orders_api -> write buffer -> ClickHouse (host-a).
Table metadata lives in MongoDB; CreateTable2 touches both, which is why it can be slow.

Deploy: git push -> CI -> docker pull -> docker compose on host-a. Never import into
MongoDB by hand — go through NATS.

Gotcha: batches flush on size AND on a timer; a stuck flush shows up as growing memory
long before ClickHouse complains.

Related: [[nats-streams]], [[host-a-services]].
EOF
```

Self-contained, states the flow explicitly, records the trap, points at credentials without
containing them, and links its neighbours.

---

## 3. Data model: what one entry is

One entry = one fact under a stable key.

| Field | Type | Set by | Meaning |
|---|---|---|---|
| `key` | slug `[a-z0-9-]`, ≤100 | you | Identity of the fact. Stable: writing the same key again **overwrites** instead of creating a duplicate. |
| `title` | non-empty string | you | One-line headline. Weighs twice as much as the body in search. |
| `body` | markdown | you | The knowledge itself. Link related entries with `[[other-key]]`. |
| `tags` | list of strings | you | Scope and facets (`nats`, `infra`, `acme`). Case-insensitive. |
| `refs` | list of strings | you | Pointers to secrets and external sources ("see .env on host-a"). **Never the secrets themselves.** |
| `updated_at` | `YYYY-MM-DD` | **server** | Date of the last real change. Unchanged when the content is identical. |

Write-time limits (otherwise `400`): key is not a slug · empty title · body or refs contain
something that looks like a secret (`ghp_…`, `sk-…`, `AKIA…`, `-----BEGIN … PRIVATE KEY`,
`password: …`).

Every entry also has a `kind`: `knowledge` (default, everything above), `incident` —
an operational event report with extra fields (`service`, `hosts`, `severity`, `status`,
`knowledge` links, `resolution`, see §7) — or `task`, a lightweight note/idea with a
resolution loop (see §8).

**Lifecycle rule (all kinds): the canon holds only what is live.** Closing an incident or
a task *archives* it — the file leaves the working tree, but the final version stays in
the **default search** and stays readable via `kyb get` (marked `"archived": true`).
`kyb rm` on plain knowledge is different: that is a *retraction* ("this was wrong"), and a
retracted entry drops out of the default search (history keeps it, as always).

---

## 4. Write and update

```bash
kyb add --key nats-streams --title "NATS: always CreateOrUpdateStream" \
        --tags nats,infra [--refs "see .env on host-a"] <<'EOF'
Always use CreateOrUpdateStream, never assume a stream is already configured.
Related: [[orders-api-architecture]].
EOF
```
The body comes from stdin (heredoc). Response:

```json
{"key":"nats-streams","sha":"1163de8f…","changed":true,"action":"created"}
```

| Field | Meaning |
|---|---|
| `key` | the entry's key |
| `sha` | commit sha of the version just written — use it later with `--at` |
| `changed` | `true` — a new version was committed; `false` — content was identical, no commit |
| `action` | `created` (key was new) or `updated` (overwritten). Absent when `changed:false` |

**Re-running `add` with identical content is safe** — it is a no-op: no commit, no date bump.

**Updating is the same `add` with the same `key`** — there is no update command. The entry is
replaced wholesale and the previous version stays in history:

```bash
kyb get host-a-services                     # 1. read what is stored now
kyb add --key host-a-services --title "host-a: what runs there" --tags infra,host-a <<'EOF'
orders_api (gRPC :9000) + web_app. ClickHouse on the same host.
Update 2026-07-20: the HTTP port moved 8080 -> 8090.
EOF
# -> {"action":"updated","changed":true,"sha":"<new sha>"}
kyb history host-a-services                 # 2. confirm: one more version
```

Update rules: **send the whole body** (there is no partial patch — what you send becomes the
entry) and pass `title`/`tags`/`refs` again or they get wiped. So when enriching, `kyb get`
first, keep what is still true, and add your part. If a fact went stale but is still useful as
history, update it rather than deleting; `kyb rm` is for entries that were simply wrong.

---

## 5. Search

```bash
kyb tags                                 # which topics the base covers, most used first
kyb query "nats streams"                 # across current knowledge
kyb query "config" --tag infra           # + tag filter (several tags = AND)
kyb query "" --limit 50                  # empty query = list everything, newest first
kyb query "" --recent --limit 10         # what changed lately (same order, explicit flag)
kyb query "old rule" --history           # search ALL versions, deleted ones included
```

**Start with `kyb tags`** when you do not know how a topic is labelled here — it is the map of
the base. Free text already matches keys and tags, not just prose: a query for `kubernetes`
finds an entry tagged `kubernetes` even if the body never says the word, and `ch proxy` reaches
the key `orders-api-deploy`. `--tag` remains the exact filter when you want only that topic.

Response is `{"count": N, "hits": [...]}`, hits sorted by relevance:

```json
{"count":1,"hits":[{
  "key":"booster-deploy", "title":"KYB on booster: docker compose",
  "body":"Image comes from ghcr, ./data:/data holds the git canon and the index.",
  "tags":["kyb","infra"],
  "sha":"7aa977d1…", "committed_at":"2026-07-20T07:12:25+00:00",
  "is_head":true, "updated_at":"2026-07-20", "score":2.78
}]}
```

| Hit field | Meaning |
|---|---|
| `key` / `title` / `body` / `tags` | contents of the matched **version** (the body is returned in full, not as a snippet) |
| `sha` | commit sha of that version → `kyb get <key> --at <sha>` |
| `committed_at` | when the version was committed (ISO-8601, UTC) |
| `is_head` | `true` — this is the current version; `false` — a version from history |
| `updated_at` | the date stored in the entry's frontmatter |
| `score` | BM25 relevance, higher is better. A hit in `title` counts double |

Search behaviour:
- **Hybrid, lexical + semantic.** BM25 over key (tokenized: `nats-streams` → `nats streams`),
  title (×2), body and tags (×1.5), fused by reciprocal rank with vector search over every
  entry (multilingual-e5-small, running locally). So **you can ask in Russian, or in words the
  entry never uses, and still land on it**: «какие сигналы про крипту» finds an architecture
  entry that never says the word, «куда складываются данные» finds the storage topology. Exact technical
  terms still win on the lexical side — fusion means a hit sinks only when both signals are weak.
- `"semantic":true` in the response means vectors took part. `--recent`, `--history` and empty
  queries are lexical-only, and so is the service when no model is installed.
- **Russian stemming**: «стримах» finds «стримы». Latin words (NATS, CreateOrUpdateStream) pass
  through as-is, case-insensitive. Exception: loanwords ending in «-й» («деплой») are not stemmed.
- **Without `--history`** the search covers the *latest version of every key*: current
  knowledge AND archived (closed) incidents/tasks — "how did we fix this last time" lands
  without any special flag. Retracted knowledge (`kyb rm`) is the one thing excluded.
  **With `--history`** only historical versions are searched (`is_head:false`), including
  every superseded version. The two modes never return duplicates of each other.
- Broken query syntax never fails the request — the parser is lenient.

---

## 6. Read an entry and its history

```bash
kyb get nats-streams                  # current version
kyb get nats-streams --at 1163de8f…   # the version at a specific commit
kyb history nats-streams              # every version of the key
```

`kyb get` returns the whole entry (`key`, `title`, `body`, `tags`, `refs`, `updated_at`).
Missing key or unknown sha → `404`.

`kyb history` lists versions, **newest first**:

```json
{"key":"booster-deploy","versions":[
  {"sha":"7aa977d1…","committed_at":"2026-07-20T07:12:25+00:00","message":"kyb: upsert booster-deploy","change":"modified"},
  {"sha":"5c73d6c3…","committed_at":"2026-07-20T07:12:11+00:00","message":"kyb: upsert booster-deploy","change":"added"}
]}
```

| Version field | Meaning |
|---|---|
| `sha` | version id; feed it to `kyb get --at` |
| `committed_at` | commit time (ISO-8601, UTC) |
| `message` | commit message (`kyb: upsert <key>` / `kyb: delete <key>`) |
| `change` | `added` (created), `modified` (changed), `deleted` (removed) |

**To find out what moved where:** `kyb query "old term" --history` → take the `sha` from the
hit → `kyb get <key> --at <sha>` shows how the knowledge looked back then; `kyb history <key>`
shows the whole chain of changes.

---

## 7. Incident reports

An incident report is an operational event, not a fact: *service X broke, here is the effect,
here is how to live with it, here is how it ended.* It lives in the same base under the same
mechanics (git canon, versions, hybrid search) but as its own kind, tied to the knowledge
entries it concerns.

**When to file one:** something broke or degraded and it cost you (or will cost the next agent)
time — a service down, data gaps, a stuck deploy, an OOM loop, a poisoned config. If you found
yourself ssh-ing around to understand "why is X dead", the next agent should not have to.

**Check what is broken before you work:** `kyb incidents --status open` at the start of infra
work — an open incident on the service you are about to touch changes what you do.

### File a report

A report is a **control panel, not a story**: everything the next agent should be able to DO
about the incident lives in it in executable form.

**Start from the skeleton**: `kyb incident --template` prints the canonical body structure to
fill in. The server accepts a bare report but answers with `hints` naming what a complete one
carries (detection, affected windows, root cause with confidence, follow-ups) — an empty
`hints` field means the report is structurally complete.

```bash
kyb incident --key inc-2026-07-22-orders-api-oom --title "orders_api OOM-killed every ~2h on host-a" \
        --service orders_api --hosts host-a --severity high \
        --detection "docker inspect orders_api --format '{{.RestartCount}}' growing + RSS near 1G; healthy = restarts stable" \
        --affected '[{"scope":"orders-writes","from":"2026-07-22T06:00:00Z","to":"2026-07-22T09:30:00Z"}]' \
        --started 2026-07-22T06:00:00Z \
        --knowledge orders-api-architecture,host-a-services --tags acme <<'EOF'
Symptom: client writes stall every ~2h, container restarts on its own.
Impact: write buffer grows and flushes late during each restart window.
Root cause (suspected, not verified): leak in the batch flush path.
Cure / runbook:
  1. docker restart orders_api            # SAFE - agent may do this alone
  2. verify: detection above returns to healthy
  3. raise memory limit in compose      # APPROVAL - only with a human
Follow-ups:
- [ ] find the leak in the write buffer flush path
- [x] alert on RestartCount growth
EOF
```

| Field | Values | Meaning |
|---|---|---|
| `--key` | `inc-<yyyy-mm-dd>-<slug>` | **Must start with `inc-`** (the server enforces it; plain knowledge cannot take `inc-` keys). Date = when it started. |
| `--service` | required | What broke (`orders_api`, `web_app`, `nats`…). Exact filter + searchable. |
| `--hosts` | optional list | Where it runs/broke (`host-a`, `booster`). |
| `--severity` | `low` `medium` `high` `critical` | required |
| `--status` | `open` (default) `mitigated` `resolved` | lifecycle, see below |
| `--knowledge` | optional list of keys | **Ties the report to the knowledge entries it concerns** (the service's architecture entry, the host entry). Dangling links are allowed but reported back as `unknown_knowledge` — write that entry soon after. |
| `--detection` | command/SQL + expected healthy result | **The most actionable field**: "is it still happening?" answered in 30 seconds instead of guessed. Shown in `kyb incidents`. |
| `--affected` | JSON `[{scope, from, to}]`, UTC | Machine-readable poisoned windows (an exchange, a table, a host). A backtester excludes them **programmatically**, without parsing prose. |
| `--started` / `--detected` | RFC3339 UTC | when it began / when it was noticed. `detected_at` defaults to filing time; `mitigated_at`/`resolved_at` are **stamped by the server** on the status transition. |
| `--resolution` | text | how it ended; required to set `resolved` |

Body skeleton — each section answers "what can the next agent DO": **Symptom** (what it looks
like, verbatim — the search anchor) · **Impact** (prose; exact windows go to `--affected`) ·
**Root cause** with confidence marked `verified | suspected | unknown` · **Cure / runbook** as
numbered steps, each marked `SAFE` (agent may do alone) or `APPROVAL` (human required) ·
**Actions log** (action + verification after it; an action without verification = not done) ·
**Follow-ups** as `- [ ]` / `- [x]` checkboxes — the server counts open ones (`open_followups`
in listings, `kyb incidents --open-followups` finds loose ends to pick up).
Same rules as knowledge: English, verified facts, no secrets (pointers in `--refs`).

**Updating** an incident is the same `kyb incident` with the same key (send everything again —
it is a wholesale replace, like `kyb add`). Every update is a new git version; the whole
timeline stays in `kyb history <key>`.

### Lifecycle: open → mitigated → resolved

```bash
kyb incidents                              # LIVE reports only (open + mitigated), freshest on top
kyb incidents --all                        # + archived (resolved) reports, at the bottom
kyb incidents --status resolved            # an explicit status looks into the archive by itself
kyb incidents --service orders_api           # incident history of one service (live)
kyb resolve inc-2026-07-22-orders-api-oom <<< "Raised the limit to 2G; fixed the flush leak in the write buffer."
kyb resolve inc-2026-07-22-orders-api-oom --status mitigated --resolution "Cron restarts it hourly while the fix bakes."
```

`kyb resolve` flips the status and records **how it ended** — everything else in the report
stays as stored. **Closing requires a resolution** (the server refuses `resolved` without one):
an incident that ends with "it just went away" teaches nobody anything. `--status mitigated`
parks it with a comment instead of closing. The server stamps `mitigated_at`/`resolved_at` on
the transition. Closing with unfinished `- [ ]` follow-ups succeeds but returns a **warning**
with the count — reassign or finish them; `kyb incidents --open-followups` lists reports
(resolved included) that still have loose ends.

**Closing archives the report**: the file leaves the canon, so the tree holds only open and
mitigated incidents. Nothing is lost — the resolved report stays in the default `kyb query`,
in `kyb get`, and in `kyb incidents --all` / `--status resolved` / `--open-followups`
(marked `"archived": true`); the plain `kyb incidents` shows only what is live. To amend a
closed report, run `kyb resolve` again with a new resolution; re-filing the same key
(`kyb incident`) or parking it (`--status mitigated`) reopens it.

At the start of a session, the triage pass is:
```bash
kyb incidents --status open        # anything burning? run its detection to see if it still is
kyb incidents --open-followups     # loose ends worth picking up
```

**After resolving, fold the durable lesson back into knowledge**: the gotcha goes into the
service's architecture entry (`kyb get` → extend → `kyb add`), the incident stays as the event
record. That is the difference between the two kinds — knowledge is *what is true now*,
an incident is *what happened*.

### Finding incidents

Incidents are searched like everything else and show up in plain `kyb query` results
(hits carry `kind`, `service`, `status`, `severity`, `resolution`):

```bash
kyb query "oom restart"  --kind incident              # only incidents
kyb query "clickhouse"   --kind incident --status open
kyb query "flush leak"   --kind incident              # resolutions are searchable:
                                                      # "how did we fix this last time"
kyb query "ch proxy" --service orders_api               # exact service filter on any search
```

`kyb health` reports `open_incidents` — non-zero means something is officially broken.

---

## 8. Tasks and ideas

A task is the third kind: a short actionable note — "do X", "look into Y", an idea worth
keeping — with the same close-with-an-outcome discipline as incidents, but none of the
ceremony. No service, no severity, no detection. Keys start with `task-` (no date — a task
may live long; its birth date is in git).

```bash
kyb task --key task-raise-log-retention --title "Raise container log retention to 72h" \
         --tags observability [--knowledge web-app-architecture] <<'EOF'
Short retention loses evidence during incidents.
- [ ] measure current log volume first
EOF

kyb tasks                                  # open tasks only, freshest on top
kyb tasks --all                            # + archived (done/dropped), at the bottom
kyb tasks --open-followups                 # loose ends inside tasks (archived included)
kyb done task-raise-log-retention <<< "Raised to 72h with a 2G disk budget."
kyb done task-try-foo --status dropped <<< "Obsolete after the bar rewrite."
```

- `status`: `open → done | dropped`. **Both closings require a resolution** — what came of
  it, or why it was dropped; "dropped because obsolete" is knowledge too.
- Closing archives the task (same rule as incidents): the file leaves the canon, the task
  stays in the default search, in `kyb get` and in `kyb tasks --all` with `"archived": true`;
  plain `kyb tasks` shows only open ones. The server stamps `resolved_at` on close.
- An idea is just a task tagged `idea`. Follow-ups discovered while resolving an incident
  that deserve their own life → file them as tasks and link the incident via `--knowledge`.
- Session triage: `kyb incidents --status open` + `kyb tasks --status open`.
- `kyb health` reports `open_tasks` next to `open_incidents`.

---

## 9. The rest

```bash
kyb rm <key>       # knowledge: retract a wrong entry (drops out of the default search).
                   # incident/task: archive it (stays searchable) — closing via
                   # resolve/done is the normal path, rm is the manual override
kyb reindex        # full index rebuild from git (rarely needed: the service does it on start)
kyb health         # {"ok":true,"entries":N,"open_incidents":K,"open_tasks":T,"index_docs":M,"last_commit":{…}}
```

`entries` — how many live facts in the canon, `open_incidents`/`open_tasks` — what is
currently broken / pending, `index_docs` — documents in the index (live + every historical
version), `last_commit` — the canon's latest commit.

---

## 10. Write rules (governance — mandatory)

1. **ALWAYS run `kyb query` before `add`.** Found something close → overwrite it under **the
   same key**. A new key only when the knowledge genuinely does not exist. Duplicates kill the base.
2. **A key is a stable entity slug** (`nats-streams`, `orders-api-architecture`), not a date and
   not a paraphrased sentence. Follow the naming patterns in §2.
3. **One fact = one key.** Growing too big → split into several keys and link them with `[[key]]`.
4. **Only verified facts.** Read it, saw it, ran it — or mark it as unverified in the body.
5. **Secrets only as `refs` pointers.** The service rejects tokens/passwords in the body (400).
6. **Bodies are self-contained**: what, why, how to apply — so it still reads in six months
   without this chat.
7. Something vanished or moved — don't guess, dig in history (`--history`, `--at`).
8. **Broke something / found something broken → file an incident report** (§7), linked via
   `--knowledge` to the entries it concerns. Close it with a real resolution, then fold the
   durable lesson back into the knowledge entry.

---

## 11. When something breaks

- Not responding → check tailscale (`tailscale status`), then
  `ssh $KYB_HOST 'cd ~/kyb && docker compose up -d; docker logs kyb | tail'`.
- Index drifted from the canon (files were hand-edited on booster) → `kyb reindex`.
- The canon is a plain git repo on the server:
  `ssh $KYB_HOST 'git -C ~/kyb/data/kyb-data log --oneline'` — the full history of knowledge,
  `git show <sha>` — one specific change.
- Upgrade the service to a new image:
  `gh auth token | ssh $KYB_HOST 'docker login ghcr.io -u <your-github-username> --password-stdin'`
  then `ssh $KYB_HOST 'cd ~/kyb && docker compose pull && docker compose up -d'`.
- The skill is installed for Claude Code, Codex and Antigravity from one source
  (`skills/install.sh` in the knowyourbase repo) — edit there and reinstall, never patch the
  agents' copies by hand.
