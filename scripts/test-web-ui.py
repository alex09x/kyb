#!/usr/bin/env python3
"""End-to-end test of the web UI in a real browser.

What it proves: the page the server serves loads without script errors, every
screen renders, and every write a person can make from the UI - create, edit,
resolve, move a task through its states - lands on the server as intended.
The edit checks are the ones that bit: an upsert replaces the whole record,
so saving a title must not reopen a blocked task or drop an incident's
detection.

How it runs:

    python3 scripts/test-web-ui.py                 # own sandbox server
    python3 scripts/test-web-ui.py --base URL      # against a running server

Without --base it starts kyb-server (KYB_SERVER_BIN, else target/release or
target/debug) on an empty data directory with no model, lexical-only, so it is
up in a second, and serves the UI assembled from src/web/ (KYB_UI_PATH), so the
sources under test are the current ones even when the binary is older. With
--base it refuses to run against anything but a loopback address unless
--allow-writes is given: the flows create and mutate entries.

Needs Playwright for Python with Chromium (`pip install playwright &&
playwright install chromium`).
"""

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

REPO = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))
from build_ui import assemble  # noqa: E402

try:
    from playwright.sync_api import sync_playwright
except ImportError:  # pragma: no cover - environment, not the UI
    sys.exit("playwright for python is not installed: pip install playwright && playwright install chromium")

FAILED = []


def check(name, ok, detail=""):
    mark = "ok  " if ok else "FAIL"
    print(f"  {mark} {name}{(' - ' + str(detail)) if detail and not ok else ''}")
    if not ok:
        FAILED.append(name)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_http(url, seconds=30):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.2)
    return False


def start_sandbox(tmp):
    binary = os.environ.get("KYB_SERVER_BIN")
    if not binary:
        for cand in (REPO / "target/release/kyb-server", REPO / "target/debug/kyb-server"):
            if cand.exists():
                binary = str(cand)
                break
    if not binary:
        sys.exit("no kyb-server binary: set KYB_SERVER_BIN or cargo build")
    ui = tmp / "index.html"
    ui.write_text(assemble(), encoding="utf-8")
    port = free_port()
    env = dict(os.environ,
               KYB_DATA=str(tmp / "data"), KYB_INDEX=str(tmp / "index"), KYB_AUDIT=str(tmp / "audit.jsonl"),
               KYB_MODEL=str(tmp / "no-model"), KYB_ADDR=f"127.0.0.1:{port}", KYB_UI_PATH=str(ui))
    log = open(tmp / "server.log", "w")
    proc = subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT)
    base = f"http://127.0.0.1:{port}"
    if not wait_http(base + "/healthz"):
        proc.kill()
        sys.exit("sandbox server did not come up:\n" + (tmp / "server.log").read_text())
    return proc, base


def api(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method,
                                 headers={"content-type": "application/json"} if data else {})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


# JS helpers injected once per page: fill a field the way a person would, and
# wait for the 4-second refresh loop to settle so the next step sees the write.
HELPERS = """
window.__t = {
  set(id, v) { const el = document.getElementById(id); if (!el) throw new Error('missing #' + id); el.value = v; el.dispatchEvent(new Event('input', {bubbles: true})); },
  wait(ms) { return new Promise(r => setTimeout(r, ms)); },
  open() { return [...document.querySelectorAll('.modal-backdrop.open')].map(m => m.id); },
};
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", help="URL of a running server (UI and API on one origin)")
    ap.add_argument("--allow-writes", action="store_true", help="allow the write flows against a non-loopback --base")
    ap.add_argument("--headed", action="store_true")
    args = ap.parse_args()

    tmp = pathlib.Path(tempfile.mkdtemp(prefix="kyb-ui-test-"))
    proc = None
    try:
        if args.base:
            base = args.base.rstrip("/")
            host = urllib.parse.urlsplit(base).hostname or ""
            if host not in ("127.0.0.1", "localhost", "::1") and not args.allow_writes:
                sys.exit(f"{base} is not loopback; the flows write. Pass --allow-writes if that is a sandbox.")
        else:
            proc, base = start_sandbox(tmp)
        print(f"== web UI against {base}")
        run(base, args.headed)
    finally:
        if proc:
            proc.kill()
        shutil.rmtree(tmp, ignore_errors=True)
    if FAILED:
        print(f"\n{len(FAILED)} failed: {', '.join(FAILED)}")
        sys.exit(1)
    print("\nall web UI checks passed")


def run(base, headed):
    stamp = format(int(time.time()), "x")
    K, I, N = f"task-ui-test-{stamp}", f"inc-ui-test-{stamp}", f"ui-test-{stamp}"

    # Seed what the read-side screens need and what the edit checks protect.
    s, _ = api(base, "POST", "/knowledge", {"key": "kyb-service", "title": "KYB service", "body": "The registry.", "tags": ["kyb"], "refs": ["vault://kyb"]})
    check("seed knowledge", s == 200, s)
    s, _ = api(base, "POST", "/knowledge", {"key": "mentions-kyb", "title": "Mentions the word", "body": "Talks about kyb without carrying the tag.", "tags": ["other"]})
    check("seed decoy without the tag", s == 200, s)
    s, _ = api(base, "POST", "/incidents", {"key": I + "-seed", "title": "Seeded incident", "body": "Symptom.", "service": "svc", "severity": "high",
                                            "status": "resolved", "resolution": "fixed it", "detection": "curl -s localhost/healthz", "hosts": ["h1"], "refs": ["vault://y"]})
    check("seed resolved incident", s == 200, s)
    for n in range(2):  # the incident screens hide resolved reports, and the selection check needs two open rows
        s, _ = api(base, "POST", "/incidents", {"key": f"{I}-open{n}", "title": f"Open incident {n}", "body": "Symptom.", "service": "svc", "severity": "high", "detection": "curl -sf localhost/healthz"})
        check(f"seed open incident {n}", s == 200, s)
    s, _ = api(base, "POST", "/tasks", {"key": K + "-seed", "title": "Seeded task", "body": "b", "status": "blocked", "blocked_reason": "waiting on vendor",
                                        "priority": "high", "assignee": "alex", "knowledge": ["kyb-service"], "refs": ["vault://x"]})
    check("seed blocked task", s == 200, s)

    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=not headed)
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        errors = []
        page.on("pageerror", lambda e: errors.append(f"pageerror {e}"))
        page.on("console", lambda m: errors.append(f"console.error {m.text}") if m.type == "error" else None)
        page.goto(base + "/#/search", wait_until="networkidle")
        page.evaluate(HELPERS)
        page.wait_for_function("document.querySelectorAll('#knowledgeList .item-card').length > 0", timeout=10000)

        # --- every screen renders, no script errors
        for tab, probe in (("search", "#tab-search"), ("incidents", "#incidentRows .inc-r"), ("tasks", "#tab-tasks .inc-r"), ("feed", "#ledBody"), ("graph", "#tab-graph canvas, #tab-graph svg")):
            page.evaluate(f"switchTab('{tab}')")
            page.wait_for_timeout(600)
            check(f"screen {tab} renders", page.locator(probe).count() > 0 or tab == "feed", probe)
        check("no script errors on load and tab switches", not errors, "; ".join(errors[:3]))

        # --- every inline handler points at a function (the ids the code reads are checked by use, in the flows below)
        missing = page.evaluate("""() => {
          const out = [];
          for (const el of document.querySelectorAll('[onclick],[onchange],[ondblclick],[onmouseenter],[onkeydown]')) {
            for (const a of ['onclick','onchange','ondblclick','onmouseenter','onkeydown']) {
              const h = el.getAttribute(a); if (!h) continue;
              for (const m of h.matchAll(/(?<![\\w.$])([A-Za-z_$][\\w$]*)\\s*\\(/g)) {
                const f = m[1]; if (['event','setTimeout','encodeURIComponent','String','Number','JSON','Date','Math'].includes(f)) continue;
                if (typeof window[f] !== 'function') out.push(f);
              }
            }
          }
          return [...new Set(out)];
        }""")
        check("every inline handler calls an existing function", not missing, missing)

        # --- inline handlers take values as JS literals: a hostile title cannot run
        pwn = page.evaluate("""() => { window.__pwn = 0; const d = document.createElement('div');
          d.innerHTML = `<button onclick="window.__got=${jsArg("x');window.__pwn=1//")}">b</button>`; d.firstChild.click();
          return [window.__pwn, window.__got]; }""")
        check("jsArg keeps a hostile value a string", pwn[0] == 0 and pwn[1] == "x');window.__pwn=1//", pwn)

        # --- create knowledge / edit it
        page.evaluate("switchTab('search')")
        page.evaluate(f"""async () => {{ openNewModal('knowledge'); await __t.wait(200);
          __t.set('newTitle', 'UI test entry'); __t.set('newKey', '{N}'); __t.set('newBody', 'Body written by the UI test.'); __t.set('newTags', 'ui-test');
          await submitNewModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{N}")
        check("create knowledge from the form", s == 200 and e.get("title") == "UI test entry" and e.get("tags") == ["ui-test"], (s, e))
        page.evaluate(f"""async () => {{ await openEditModal('{N}'); await __t.wait(300); __t.set('editTitle', 'UI test entry edited'); await submitEditModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{N}")
        check("edit knowledge keeps body and tags", e.get("title") == "UI test entry edited" and e.get("tags") == ["ui-test"] and "UI test" in e.get("body", ""), e)

        # --- create incident / resolve it
        page.evaluate(f"""async () => {{ openNewModal('incident'); await __t.wait(200);
          __t.set('newTitle', 'UI test incident'); __t.set('newKey', '{I}'); __t.set('newBody', 'Symptom: test.'); __t.set('newService', 'ui-test');
          await submitNewModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{I}")
        check("create incident from the form", s == 200 and e.get("kind") == "incident" and e.get("service") == "ui-test", (s, e))
        page.evaluate(f"""async () => {{ openResolveIncidentModal('{I}', 't'); await __t.wait(200); __t.set('resolveIncidentOutcome', 'Resolved by the UI test.'); await submitResolveIncident(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{I}")
        check("resolve incident from the dialog", e.get("status") == "resolved" and "UI test" in e.get("resolution", ""), e)

        # --- task through its states
        page.evaluate(f"""async () => {{ openNewModal('task'); await __t.wait(200);
          __t.set('newTitle', 'UI test task'); __t.set('newKey', '{K}'); __t.set('newBody', 'Do the thing.'); document.getElementById('newPriority').value = 'high';
          await submitNewModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("create task from the form (priority accepted)", s == 200 and e.get("kind") == "task" and e.get("priority") == "high", (s, e))
        check("creating closes the dialog", page.evaluate("__t.open()") == [], page.evaluate("__t.open()"))
        page.evaluate(f"async () => {{ await transitionTask('{K}', 'in_progress'); await __t.wait(300); }}")
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("task -> in_progress", e.get("status") == "in_progress", e.get("status"))
        page.evaluate(f"""async () => {{ moveTaskTo('blocked', '{K}', 't'); await __t.wait(200); __t.set('blockTaskReason', 'Waiting on the UI test.'); await submitBlockTask(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("task -> blocked with a reason", e.get("status") == "blocked" and "UI test" in e.get("blocked_reason", ""), (e.get("status"), e.get("blocked_reason")))
        page.evaluate(f"""async () => {{ await openEditModal('{K}'); await __t.wait(300); __t.set('editTitle', 'UI test task edited'); await submitEditModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("editing a blocked task keeps status, reason and priority",
              e.get("title") == "UI test task edited" and e.get("status") == "blocked" and "UI test" in e.get("blocked_reason", "") and e.get("priority") == "high", e)
        page.evaluate(f"""async () => {{ openResolveTaskModal('{K}', 't'); await __t.wait(200); __t.set('resolveTaskOutcome', 'Done by the UI test.'); await submitResolveTask(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("task -> done with a resolution", e.get("status") == "done" and "UI test" in e.get("resolution", ""), (e.get("status"), e.get("resolution")))
        page.evaluate(f"() => moveTaskTo('open', '{K}', 't')")  # the Reopen button's handler; returns the transition promise
        s, e = api(base, "GET", f"/knowledge/{K}")
        check("reopen a done task", e.get("status") == "open" and e.get("title") == "UI test task edited" and e.get("priority") == "high", (e.get("status"), e.get("title"), e.get("priority")))

        # --- the edits that used to destroy records
        page.evaluate(f"""async () => {{ await openEditModal('{I}-seed'); await __t.wait(300); __t.set('editTitle', 'Seeded incident edited'); await submitEditModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{I}-seed")
        check("editing a resolved incident keeps status, resolution, detection, hosts, refs",
              e.get("status") == "resolved" and e.get("resolution") == "fixed it" and "healthz" in e.get("detection", "") and e.get("hosts") == ["h1"] and e.get("refs") == ["vault://y"], e)
        page.evaluate(f"""async () => {{ await openEditModal('{K}-seed'); await __t.wait(300); __t.set('editTitle', 'Seeded task edited'); await submitEditModal(); await __t.wait(400); }}""")
        s, e = api(base, "GET", f"/knowledge/{K}-seed")
        check("editing a blocked task keeps knowledge links and refs", e.get("knowledge") == ["kyb-service"] and e.get("refs") == ["vault://x"] and e.get("status") == "blocked", e)
        page.evaluate("async () => { await openEditModal('kyb-service'); await __t.wait(300); __t.set('editTitle', 'KYB service edited'); await submitEditModal(); await __t.wait(400); }")
        s, e = api(base, "GET", "/knowledge/kyb-service")
        check("editing knowledge keeps refs", e.get("refs") == ["vault://kyb"], e.get("refs"))

        # --- the selection survives a background refresh that reorders the list
        kept = page.evaluate("""async () => { switchTab('incidents'); await refreshAll(); App.incidentCursor = 1; renderIncidents();
          const k = App.incidentCursorKey; const save = App.incidents;
          App.incidents = [{key: 'zz-new', title: 'new', status: 'open', severity: 'critical', detected_at: new Date().toISOString(), updated_at: new Date().toISOString()}, ...save];
          renderIncidents(true); const r = [k, App.incidentCursorKey, App.incidentCursor]; App.incidents = save; renderIncidents(true); return r; }""")
        check("incident selection follows the entry across a refresh", kept[0] == kept[1] and kept[2] == 2, kept)

        # --- the ledger shows a change in full
        page.evaluate("switchTab('feed')")
        page.wait_for_timeout(1500)
        check("activity ledger shows diffs", page.locator("#ledBody .led-ev").count() > 0 and page.locator("#ledBody .led-d").count() > 0,
              (page.locator('#ledBody .led-ev').count(), page.locator('#ledBody .led-d').count()))


        # ------------------------------------------------------------------
        # Reading, filtering, navigating: each control changes what is shown,
        # and what is shown is what the control promised.
        # ------------------------------------------------------------------
        rows = lambda: page.evaluate("[...document.querySelectorAll('#incidentRows .inc-r')].map(r => r.querySelector('.t span').textContent)")
        page.evaluate("switchTab('incidents'); App.incidentStatusFilter='all'; App.incidentSeverityFilter='all'; App.incidentQueue=null; renderIncidents();")
        open_rows = rows()
        check("incident list hides resolved by default", open_rows and all(not r.endswith("-seed") and r != I for r in open_rows), open_rows)
        page.evaluate("filterIncidents('resolved')")
        res_rows = rows()
        check("filterIncidents('resolved') shows only resolved", set(res_rows) == {I, I + "-seed"}, res_rows)
        page.evaluate("filterIncidents('all'); filterIncidentsSeverity('high')")
        check("filterIncidentsSeverity('high') keeps only high", rows() and page.evaluate("incFiltered().every(i => i.severity === 'high')"), rows())
        page.evaluate("filterIncidentsSeverity('low')")
        check("a severity with no incidents shows the empty state", page.locator("#incidentRows .inc-empty").count() == 1, rows())
        page.evaluate("filterIncidentsSeverity('all'); incSelectQueue('unverifiable')")
        check("queue 'unverifiable' = open incidents without detection", page.evaluate("incFiltered().every(i => i.status !== 'resolved' && !(i.detection||'').trim())") and page.evaluate("App.incidentQueue") == "unverifiable", rows())
        n_unver = len(rows())
        page.evaluate("incSelectQueue('unverifiable')")
        check("selecting the same queue again clears it", page.evaluate("App.incidentQueue") is None and len(rows()) >= n_unver, rows())
        page.evaluate("incSelectQueue('abandoned'); incClearCell()")
        check("incClearCell clears the queue filter", page.evaluate("App.incidentQueue") is None)
        cmd = page.evaluate("filterIncidents('open'); filterIncidentsSeverity('high'); document.getElementById('incCmd').textContent")
        check("the command line spells the active filters", "--status open" in cmd and "--severity high" in cmd, cmd)
        page.evaluate("filterIncidents('all'); filterIncidentsSeverity('all')")

        trows = lambda: page.evaluate("[...document.querySelectorAll('#tab-tasks .inc-r')].map(r => r.querySelector('.t span').textContent.split(' ')[0])")
        page.evaluate("switchTab('tasks'); taskSetStatus('live'); App.taskQueue=null; renderTasks()")
        check("task filter 'live' shows open and blocked tasks", set(trows()) == {K, K + "-seed"}, trows())  # K was reopened above
        page.evaluate("taskSetStatus('done')")
        check("task filter 'done' is empty after the reopen", trows() == [], trows())
        page.evaluate("taskSetStatus('blocked')")
        check("task filter 'blocked' keeps only blocked", trows() and page.evaluate("taskFiltered().every(t => t.status === 'blocked')"), trows())
        page.evaluate("taskSetStatus('live'); taskSelectQueue('blocked')")
        check("task queue 'blocked' selects and filters", page.evaluate("App.taskQueue") == "blocked" and page.evaluate("taskFiltered().every(t => t.status === 'blocked')"), trows())
        page.evaluate("taskSelectQueue('blocked')")
        check("task queue toggles off", page.evaluate("App.taskQueue") is None)

        # knowledge list: search, tag, card
        page.evaluate("setGlobalSearchQuery('entry edited')")
        keys = page.evaluate("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key)")
        check("global search narrows the list to the match", keys == [N], keys)
        check("search writes the url", page.evaluate("location.hash") == "#/search?q=entry%20edited", page.evaluate("location.hash"))
        page.evaluate("filterByTag('kyb')")
        keys = page.evaluate("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key)")
        check("filterByTag keeps only entries carrying the tag, not ones that merely mention it", keys == ["kyb-service"], keys)
        check("the search box shows the tag as a tag", page.evaluate("document.getElementById('globalSearch').value") == "#kyb")
        page.evaluate("setGlobalSearchQuery('')")
        page.evaluate(f"handleCardClick(new MouseEvent('click'), '{N}', 'knowledge')")
        page.wait_for_function(f"App.selectedKey === '{N}' && document.getElementById('detailBodyContent') && document.getElementById('detailBodyContent').textContent.includes('Body')", timeout=5000)
        check("clicking a card opens that entry", page.evaluate("App.selectedKey") == N and page.evaluate("location.hash") == f"#/knowledge/{N}", page.evaluate("[App.selectedKey, location.hash]"))
        page.evaluate(f"handleCardClick(new MouseEvent('click'), '{I}', 'incidents')")
        check("an incident card deep-links into the incidents tab", page.evaluate("location.hash") == f"#/incidents/{I}", page.evaluate("location.hash"))

        # detail pane: tabs, versions, snapshot
        page.evaluate(f"switchTab('search'); selectEntry('{N}')")
        page.wait_for_function("document.getElementById('verList') !== null", timeout=5000)
        vis = lambda id_: page.evaluate(f"(() => {{ const el = document.getElementById('{id_}'); return !!el && el.style.display !== 'none' && el.offsetParent !== null; }})()")
        page.evaluate("switchDetailTab('history')")
        check("Versions tab shows the version ledger", vis("detailBodyHistory") and not vis("detailBodyContent") and page.evaluate("App.detailTab") == "history")
        page.wait_for_function("document.querySelectorAll('#verList .ver-ev').length >= 2", timeout=8000)  # the edit above must show up: no stale cache
        stale = page.evaluate(f"""async () => {{ App.detailCache.clear(); const p = prefetchEntry('{N}'); App.cacheGen++; await p; return App.detailCache.has('{N}'); }}""")
        check("a prefetch that started before a write does not repopulate the cache", stale is False, stale)
        drawn = page.evaluate(f"""async () => {{ App.detailCache.clear(); const p = selectEntry('kyb-service', false); App.selectedKey = '{N}'; await p;
          return [[...document.querySelectorAll('.detail-tab')].map(b => b.textContent).join('|'), App.detailCache.has('kyb-service')]; }}""")
        # the optimistic skeleton (history: 0 versions) may draw before the key changes; the fetched history must not follow it
        check("a stale selectEntry response neither draws nor caches", drawn[1] is False and "Versions (0)" in drawn[0], drawn)
        refetched = page.evaluate(f"""async () => {{ App.detailCache.clear(); const p = selectEntry('{N}', false); App.cacheGen++; await p;
          return [[...document.querySelectorAll('.detail-tab')].map(b => b.textContent).join('|'), App.detailCache.has('{N}')]; }}""")
        check("a write during the load of the selected entry triggers a fresh load, not a skeleton", "Versions (2)" in refetched[0] and refetched[1] is True, refetched)
        check("the edited entry has two versions, each with its change", page.locator("#verList .ver-ev").count() == 2 and page.locator("#verList .ver-fld, #verList .ver-dif, #verList .ver-note").count() >= 2)
        sha = page.evaluate("App.currentHistoryList[App.currentHistoryList.length - 1].sha")
        page.evaluate(f"verToggleText('{sha}')")
        page.wait_for_function("document.querySelectorAll('#verList .ver-text').length === 1", timeout=5000)
        check("'text at v1' shows the document as first written", "Body written" in page.locator("#verList .ver-text").inner_text())
        page.evaluate(f"verToggleText('{sha}')")
        check("toggling again hides it", page.locator("#verList .ver-text").count() == 0)
        page.evaluate("switchDetailTab('raw')")
        check("Raw tab", vis("detailBodyRaw") and not vis("detailBodyHistory"))
        page.evaluate("switchDetailTab('content')")
        check("Document tab", vis("detailBodyContent"))
        page.evaluate(f"inspectRevision('{N}', '{sha}')")
        page.wait_for_function("document.getElementById('detailBodyContent').textContent.includes('Historical Snapshot')", timeout=5000)
        check("inspectRevision renders the old body", "Body written" in page.locator("#detailBodyContent").inner_text())
        page.evaluate(f"openHistoryForEntry('{K}')")
        page.wait_for_function(f"App.selectedKey === '{K}' && App.detailTab === 'history'", timeout=5000)
        check("h/History from a task row opens its Versions", page.evaluate("App.activeTab") == "search")

        # copy buttons write the clipboard
        page.context.grant_permissions(["clipboard-read", "clipboard-write"], origin=base)
        page.evaluate(f"copyDirectLink('#/knowledge/{N}')")
        page.wait_for_timeout(200)
        check("Link copies the full URL", page.evaluate("navigator.clipboard.readText()") == f"{base}/#/knowledge/{N}", page.evaluate("navigator.clipboard.readText()"))
        page.evaluate(f"copyToClipboard('kyb get {N}', 'x')")
        page.wait_for_timeout(200)
        check("CLI copies the command", page.evaluate("navigator.clipboard.readText()") == f"kyb get {N}")
        page.evaluate(f"selectEntry('{N}')")
        page.wait_for_function(f"App.currentDetailBody && App.currentDetailBody.includes('UI test')", timeout=5000)
        page.evaluate("copyEntryBody()")
        page.wait_for_timeout(200)
        check("Markdown copies the body", "UI test" in page.evaluate("navigator.clipboard.readText()"))

        # ledger filter and search folds
        for q in ("alpha probe", "beta probe"):
            api(base, "GET", f"/search?q={q.replace(' ', '%20')}")
        page.evaluate("switchTab('feed'); manualRefresh()")
        page.wait_for_function("document.querySelectorAll('#ledBody .led-fold').length > 0", timeout=8000)
        page.evaluate("setActivityFilter('task')")
        subs = page.evaluate("[...document.querySelectorAll('#ledBody .led-ev .sub')].map(e => e.textContent)")
        check("ledger filter 'task' keeps only task events", subs and all("task" in x for x in subs), subs[:3])
        page.evaluate("setActivityFilter('all')")
        fold = page.locator("#ledBody .led-fold").first
        check("searches are folded by default", "show" in fold.inner_text())
        page.evaluate("ledToggleFold(0)")
        check("ledToggleFold unfolds the searches", page.locator("#ledBody .led-q").count() >= 2 and "hide" in page.locator("#ledBody .led-fold").first.inner_text(), page.locator('#ledBody .led-q').count())
        page.evaluate("handleSearchFeedClick(new MouseEvent('click'), 'alpha probe')")
        check("clicking a logged search runs it", page.evaluate("App.activeTab") == "search" and page.evaluate("document.getElementById('globalSearch').value") == "alpha probe")

        # topology: state, not pixels
        page.evaluate("switchTab('graph')")
        page.wait_for_function("(App.graphNodes || []).length > 0", timeout=8000)
        n_all = page.evaluate("App.graphNodes.length")
        page.evaluate("setGraphFilter('incident')")
        check("graph filter records its state", page.evaluate("App.graphFilter") == "incident")
        page.evaluate("setGraphFilter('all'); setGraphLayout('grid')")
        check("graph layout switches and marks its button", page.evaluate("App.graphLayout") == "grid" and page.evaluate("document.getElementById('layoutGridBtn').classList.contains('active')"))
        page.evaluate("setGraphLayout('orbit')")
        node = page.evaluate(f"(App.graphNodes.find(n => n.id === '{N}') || {{}}).id")
        check("the knowledge entry is a graph node", node == N, node)
        page.evaluate(f"focusNodeById('{N}')")
        check("focusNodeById opens the drawer on that node", page.evaluate("document.getElementById('graphDrawer').classList.contains('open')") and page.evaluate("document.getElementById('drawerKey').textContent") == N and page.evaluate("location.hash") == f"#/graph/{N}")
        page.evaluate("closeGraphDrawer()")
        check("closeGraphDrawer closes it", not page.evaluate("document.getElementById('graphDrawer').classList.contains('open')"))
        page.evaluate("setTopoSearch('ui-test')")
        cnt = page.locator("#topoSearchCount").inner_text()
        check("topology search counts matching nodes", cnt.startswith(str(page.evaluate("App.graphNodes.filter(n => n.id.toLowerCase().includes('ui-test') || n.label.toLowerCase().includes('ui-test')).length"))), cnt)
        page.evaluate("setTopoSearch('')")
        z0, tz = page.evaluate("(() => { const z = graphCamera().zoom; zoomGraph(2); return [z, graphCamera().targetZoom]; })()")
        check("zoomGraph aims the camera at twice the zoom", abs(tz - min(3.2, z0 * 2)) < 1e-6, (z0, tz))
        page.wait_for_function(f"graphCamera().zoom > {z0} * 1.5", timeout=5000)
        check("and the camera gets there", True)
        page.evaluate("resetGraphCamera(); fitGraphCamera(); toggleGraphPhysics(); unpinAllGraphNodes(); reheatSimulation(0.5)")
        page.evaluate(f"navigateToGraphNode(new MouseEvent('click'), '{N}')")
        check("Topology link from a row deep-links to the node", page.evaluate("location.hash") == f"#/graph/{N}")
        page.evaluate("filterFleetByHub('svc:ui-test')")
        check("a hub chip searches by its service", page.evaluate("document.getElementById('globalSearch').value") == "ui-test" and page.evaluate("App.activeTab") == "search")

        # the thin click wrappers behind chips and drawer links
        page.evaluate("switchTab('graph')")
        page.evaluate(f"handleGraphChipClick(new MouseEvent('click'), '{N}')")
        check("a connected-node chip focuses that node", page.evaluate("document.getElementById('drawerKey').textContent") == N and page.evaluate("document.getElementById('graphDrawer').classList.contains('open')"))
        page.evaluate(f"handleDrawerOpenFullClick(new MouseEvent('click'), '{N}')")
        page.wait_for_function(f"App.selectedKey === '{N}' && App.activeTab === 'search'", timeout=5000)
        check("'Open full' from the drawer lands on the entry", not page.evaluate("document.getElementById('graphDrawer').classList.contains('open')"))
        page.evaluate("handleTagClick(new MouseEvent('click'), 'kyb')")
        check("a tag badge filters by its tag", page.evaluate("location.hash") == "#/search?tag=kyb" and page.evaluate("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key)") == ["kyb-service"])
        page.evaluate("setGlobalSearchQuery('')")
        page.evaluate(f"openDiffForEntry('{K}')")
        page.wait_for_function(f"App.selectedKey === '{K}' && App.detailTab === 'history'", timeout=5000)
        check("d/Diff from a row opens the same Versions ledger", True)
        copied = page.evaluate("""() => { const real = navigator.clipboard; Object.defineProperty(navigator, 'clipboard', {value: undefined, configurable: true});
          let got = null; const ex = document.execCommand; document.execCommand = (c) => { if (c === 'copy') got = document.activeElement && document.activeElement.value; return true; };
          copyToClipboard('fallback text', 'x'); document.execCommand = ex; Object.defineProperty(navigator, 'clipboard', {value: real, configurable: true}); return got; }""")
        check("without the clipboard API the copy falls back to execCommand", copied == "fallback text", copied)

        # modals and refresh
        page.evaluate("openShortcutsModal()")
        check("? opens the shortcuts dialog", page.evaluate("__t.open()") == ["shortcutsModal"], page.evaluate("__t.open()"))
        page.evaluate("handleBackdropClick({target: document.getElementById('shortcutsModal')}, 'shortcutsModal')")
        check("clicking the backdrop closes it", page.evaluate("__t.open()") == [])
        t0 = page.evaluate("App.lastSyncTime")
        page.evaluate("manualRefresh()")
        page.wait_for_function(f"App.lastSyncTime > {t0}", timeout=8000)
        check("manual refresh re-syncs", True)
        page.evaluate("App.entryPrefetch.clear(); prefetchEntry('kyb-service')")
        page.wait_for_function("App.entryPrefetch.has('kyb-service')", timeout=5000)
        check("hovering a card prefetches its document", True)


        # ------------------------------------------------------------------
        # Every function has a contract; here each one is held to it directly.
        # ------------------------------------------------------------------
        ev = page.evaluate

        # pure helpers
        md = ev("""renderMarkdown('# T\\n\\n**b** *i* `c` [x](other-key) [y](https://e.com) [z](#/incidents)\\n\\n- a\\n- [x] done\\n- [ ] todo\\n\\n> q\\n\\n```sh\\nls\\n```\\n\\n<script>1</script>')""")
        for frag, why in (("<h1>T</h1>", "heading"), ("<strong>b</strong>", "bold"), ("<em>i</em>", "italic"), ("<code>c</code>", "inline code"),
                          ('href="#/knowledge/other-key"', "bare link becomes an entry link"), ('href="https://e.com" target="_blank" rel="noopener noreferrer"', "external link opens in a new tab"),
                          ('href="#/incidents"', "hash link stays"), ("<li>a</li>", "list item"), ('type="checkbox" checked disabled', "done checkbox"), ('type="checkbox" disabled>', "open checkbox"),
                          ("<blockquote>q</blockquote>", "quote"), ('<pre><code class="lang-sh">ls</code></pre>', "fenced code"), ("&lt;script&gt;", "html is escaped")):
            check(f"renderMarkdown: {why}", frag in md, md[:400])
        check("renderMarkdown: empty in, empty out", ev("renderMarkdown('')") == "")
        check("escapeHtml escapes & < > \" '", ev("escapeHtml('&<>\"\\'')") == "&amp;&lt;&gt;&quot;&#39;")
        check("escapeHtml of nothing is ''", ev("[escapeHtml(''), escapeHtml(null), escapeHtml(undefined)]") == ["", "", ""])
        check("jsArg quotes and escapes for an attribute", ev("""jsArg('a"b<c')""") == "&quot;a\\&quot;b&lt;c&quot;" and ev("jsArg(null)") == "&quot;&quot;", ev("""jsArg('a"b<c')"""))
        now = ev("Date.now()")
        for delta, want in ((5e3, "5s ago"), (90e3, "1m ago"), (2 * 3600e3, "2h ago"), (3 * 86400e3, "3d ago")):
            check(f"timeAgo {want}", ev(f"timeAgo(new Date({now} - {delta}).toISOString())") == want)
        check("timeAgo of nothing is 'recently'; garbage passes through", ev("[timeAgo(''), timeAgo('not a date')]") == ["recently", "not a date"])
        check("incAgeLabel counts d/h/m from detected_at", ev(f"[incAgeLabel({{detected_at: new Date({now} - 2*86400e3).toISOString()}}), incAgeLabel({{started_at: new Date({now} - 3*3600e3).toISOString()}}), incAgeLabel({{updated_at: new Date({now} - 5*60e3).toISOString()}})]") == ["2d", "3h", "5m"])
        check("incWhen prefers detected_at over started_at over updated_at", ev("incWhen({detected_at: '2026-01-02T00:00:00Z', started_at: '2026-01-01T00:00:00Z'}) > incWhen({started_at: '2026-01-01T00:00:00Z'})"))
        check("ledVerb maps actions to verbs and tones", ev("[ledVerb({kind:'incident', action:'created'}), ledVerb({action:'resolved'}), ledVerb({action:'done'}), ledVerb({action:'archived'}), ledVerb({action:'updated'}), ledVerb({action:'transitioned'}), ledVerb({action:'weird'})]")
              == [["opened", "open"], ["resolved", "res"], ["done", "res"], ["archived", "retract"], ["edited", ""], ["moved", ""], ["weird", ""]])
        check("ledWho/verWho strip the e-mail", ev("[ledWho('Alexander Panasenko <a@b.c>'), ledWho(''), verWho({author: 'X <y>'}), verWho({})]") == ["Alexander Panasenko", "unknown", "X", ""])
        check("ledDay labels today and yesterday", ev(f"[ledDay(new Date({now}).toISOString()).startsWith('today'), ledDay(new Date({now} - 86400e3).toISOString()).startsWith('yesterday'), ledDay('2020-01-05T00:00:00Z').startsWith('today')]") == [True, True, False])
        check("ledTime is HH:MM", ev("/^\\d{2}:\\d{2}$/.test(ledTime(new Date().toISOString()))"))
        vw = ev("(() => { const d = new Date(2026, 2, 4, 5, 6); return [verWhen({committed_at: d.toISOString()}), d.toLocaleString(undefined, {day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit', hour12: false})]; })()")
        check("verWhen is the locale's short day-month-time rendering", vw[0] == vw[1] and "05:06" in vw[0] and "4" in vw[0], vw)
        check("highlightText marks matches, escapes html, and leaves text alone without a query", ev("[highlightText('a <b> c', 'b'), highlightText('x', ''), highlightText('a.b', 'a.b')]")
              == ['a &lt;<mark class="search-highlight">b</mark>&gt; c', "x", '<mark class="search-highlight">a.b</mark>'])
        check("verPrevSha walks the list it is given", ev("[verPrevSha([{sha:'c'},{sha:'b'},{sha:'a'}], 'c'), verPrevSha([{sha:'c'},{sha:'b'},{sha:'a'}], 'a'), verPrevSha([], 'x')]") == ["b", None, None])

        # header stats mirror the data
        ev("renderHeaderStats()")
        stats = ev("({indexed: document.getElementById('statIndexed').textContent, inc: document.getElementById('statIncidents').textContent, tasks: document.getElementById('statTasks').textContent, bK: document.getElementById('badgeKnowledgeCount').textContent, bI: document.getElementById('badgeIncidentsCount').textContent, bT: document.getElementById('badgeTasksCount').textContent, red: document.getElementById('statIncidents').className, pills: document.querySelectorAll('#tagPills .badge-tag').length, n: App.entries.length, oi: App.incidents.filter(i => i.status !== 'resolved').length, at: App.tasks.filter(t => t.status !== 'done').length, tags: Math.min(8, App.tags.length)})")
        check("header stats: entries, open incidents, active tasks, tab badges, tag pills", stats["indexed"] == f"{stats['n']} entries" and stats["inc"] == f"{stats['oi']} open" and stats["tasks"] == f"{stats['at']} active"
              and stats["bK"] == str(stats["n"]) and stats["bI"] == str(stats["oi"]) and stats["bT"] == str(stats["at"]) and stats["pills"] == stats["tags"] and ("red" in stats["red"]) == (stats["oi"] > 0), stats)
        ev("App.lastSyncTime = Date.now() - 65e3; updateLastSyncTicker()")
        check("last-sync ticker shows the age", ev("document.getElementById('statLastSync').textContent") == "1m ago")

        # knowledge list: count label, filter chips, sort, highlight
        ev("setGlobalSearchQuery(''); App.currentFilter = 'all'; renderKnowledgeList()")
        check("list count label matches the list", ev("document.getElementById('listCountLabel').textContent") == f"{ev('getFilteredEntries().length')} items")
        for kind in ("incident", "task", "knowledge"):
            ev(f"document.querySelector('.chip[data-filter=\"{kind}\"]').click()")
            kinds = ev("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => App.entries.find(e => e.key === c.dataset.key).kind)")
            check(f"filter chip '{kind}' keeps only that kind", kinds and all(k == kind for k in kinds) and ev(f"document.querySelector('.chip[data-filter=\"{kind}\"]').classList.contains('active')"), kinds)
        ev("document.querySelector('.chip[data-filter=\"all\"]').click()")
        ev("document.getElementById('sortSelect').value = 'az'; renderKnowledgeList()")
        titles = ev("[...document.querySelectorAll('#knowledgeList .item-title')].map(t => t.textContent)")
        check("sort A-Z orders by title", titles == sorted(titles, key=lambda t: t.lower()) or titles == sorted(titles), titles[:4])
        ev("document.getElementById('sortSelect').value = 'oldest'; renderKnowledgeList()")
        ages = ev("getFilteredEntries().map(e => new Date(e.updated_at || e.committed_at || 0).getTime())")
        check("sort oldest first", ages == sorted(ages))
        ev("document.getElementById('sortSelect').value = 'newest'; renderKnowledgeList()")
        ages = ev("getFilteredEntries().map(e => new Date(e.updated_at || e.committed_at || 0).getTime())")
        check("sort newest first", ages == sorted(ages, reverse=True))
        ev("setGlobalSearchQuery('seeded')")
        check("search highlights the match in the card", ev("document.querySelectorAll('#knowledgeList mark.search-highlight').length") > 0)
        check("getFilteredEntries matches title, key, tag and body", ev("App.searchQuery = 'vault'; const a = getFilteredEntries().length; App.searchQuery = 'kyb'; const b = getFilteredEntries().length; App.searchQuery = ''; [a, b]") == [0, ev("App.entries.filter(e => (e.title+e.key+(e.tags||[]).join()+(e.body||'')).toLowerCase().includes('kyb')).length")])
        ev("setGlobalSearchQuery('')")
        check("an empty search shows the empty state when nothing matches", ev("setGlobalSearchQuery('zzz-nothing-zzz'); document.getElementById('knowledgeList').textContent.includes('No entries matching')"))
        ev("setGlobalSearchQuery('')")

        # backend search on Enter
        ev("document.getElementById('globalSearch').value = 'Seeded task'; document.getElementById('globalSearch').dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}))")
        page.wait_for_function("App.isBackendSearchResults === true", timeout=8000)
        check("Enter runs a server search and selects the top hit", ev("App.entries.length") >= 1 and ev("App.selectedKey") == ev("App.entries[0].key") and ev("location.hash") == "#/search?q=Seeded%20task")
        ev("document.getElementById('globalSearch').value = ''; document.getElementById('globalSearch').dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}))")
        page.wait_for_function("App.isBackendSearchResults === false", timeout=8000)

        # detail view: hero banner actions follow the task's state
        ev(f"selectEntry('{K}-seed')")  # blocked
        page.wait_for_function(f"App.selectedKey === '{K}-seed' && document.querySelector('.task-hero-panel')", timeout=5000)
        acts = ev("[...document.querySelectorAll('.task-hero-panel .btn')].map(b => b.textContent.trim())")
        check("blocked task offers Unblock, Resolve, Backlog", "Unblock & Resume" in acts and "Resolve Task" in acts and "Move to Backlog" in acts and "Start Progress" not in acts, acts)
        ev(f"selectEntry('{K}')")  # open after reopen
        page.wait_for_function(f"App.selectedKey === '{K}' && [...document.querySelectorAll('.task-hero-panel .btn')].some(b => b.textContent.includes('Start Progress'))", timeout=5000)
        acts = ev("[...document.querySelectorAll('.task-hero-panel .btn')].map(b => b.textContent.trim())")
        check("open task offers Start, Block, Resolve", "Start Progress" in acts and "Block Task" in acts and "Resolve Task" in acts, acts)
        check("detail header shows key, Link, CLI, Edit", ev("!!document.getElementById('btnCopyDirectLink') && !!document.getElementById('btnEditEntry')"))
        ev(f"selectEntry('{I}-seed')")
        page.wait_for_function(f"App.selectedKey === '{I}-seed'", timeout=5000)
        check("resolved incident shows its resolution and detection", ev("document.getElementById('detailView').textContent.includes('fixed it') && document.getElementById('detailView').textContent.includes('healthz')"))

        # incidents / tasks rows mirror the filtered data
        ev("switchTab('incidents'); filterIncidents('all'); filterIncidentsSeverity('all'); App.incidentQueue = null; renderIncidents()")
        check("incident rows == incFiltered()", rows() == ev("incFiltered().map(i => i.key)"))
        check("incident rows carry service and age", ev("[...document.querySelectorAll('#incidentRows .inc-r')].every(r => r.querySelector('.sv').textContent && r.querySelector('.age').textContent)"))
        check("the cursor row shows the peek with its detection", ev("(() => { const p = document.querySelector('#incidentRows .inc-peek'); const k = App.incidentCursorKey; const i = App.incidents.find(x => x.key === k); return !!p && (!i.detection || p.textContent.includes(i.detection)); })()"))
        check("rows without detection say so", ev("[...document.querySelectorAll('#incidentRows .inc-r:not(.cur)')].every(r => { const i = App.incidents.find(x => x.key === r.querySelector('.t span').textContent); return i.status === 'resolved' || (!!(i.detection||'').trim()) === !!r.querySelector('.inc-note.det') && (!(i.detection||'').trim()) === !!r.querySelector('.inc-note:not(.det):not(.res)'); })"))
        check("summary line counts open/critical/high", ev("(() => { const t = document.getElementById('incTop').textContent; const o = App.incidents.filter(i => i.status !== 'resolved'); return t.includes(o.length + 'open') && t.includes(o.filter(i => i.severity === 'critical').length + ' critical') && t.includes(o.filter(i => i.severity === 'high').length + ' high'); })()"), ev("document.getElementById('incTop').textContent").strip()[:80])
        check("queue counters equal their filters", ev("INC_QUEUES.every(q => document.querySelector('#incQueue .b:nth-child(' + (INC_QUEUES.indexOf(q) + 1) + ') b').textContent == App.incidents.filter(q.test).length)"))
        ev("switchTab('tasks'); taskSetStatus('live'); App.taskQueue = null; renderTasks()")
        check("task rows == taskFiltered()", trows() == ev("taskFiltered().map(t => t.key)"))
        check("blocked rows show their reason inline", ev("[...document.querySelectorAll('#tab-tasks .inc-r')].every(r => { const t = App.tasks.find(x => x.key === r.querySelector('.t span').textContent.split(' ')[0]); return t.status !== 'blocked' || r.querySelector('.inc-note').textContent.includes(t.blocked_reason); })"))
        check("task queue counters equal their filters", ev("TASK_QUEUES.every((q, i) => document.querySelector('#taskQueue .b:nth-child(' + (i + 1) + ') b').textContent == App.tasks.filter(q.test).length)"))
        check("blocked tasks sort before in-progress before open", ev("(() => { const o = {blocked: 0, in_progress: 1, open: 2}; const s = taskFiltered().map(t => o[t.status] ?? 2); return s.every((v, i) => i === 0 || s[i-1] <= v); })()"))

        # ledger mirrors the activity
        ev("switchTab('feed'); setActivityFilter('all'); renderFeed()")
        check("ledger rows == activity events (capped at 120)", ev("document.querySelectorAll('#ledBody .led-ev').length") == ev("Math.min(120, App.activity.length)"))
        check("ledger summary counts today's changes", ev("document.getElementById('ledTop').textContent.includes(App.activity.filter(a => new Date(a.ts).toDateString() === new Date().toDateString()).length + 'changes today')"))
        check("each ledger row names verb, title, key and author", ev("[...document.querySelectorAll('#ledBody .led-ev')].every(r => r.querySelector('.led-verb').textContent && r.querySelector('.h b').textContent && r.querySelector('.who').textContent)"))
        check("ledDiffHtml renders fields and body lines, nothing for no change", ev("[ledDiffHtml({changed: true, fields: [{field: 'title', from: 'a', to: 'b'}], body: {added: ['x'], removed: ['y']}}).includes('title') && ledDiffHtml({changed: true, fields: [{field: 'title', from: 'a', to: 'b'}], body: {added: ['x'], removed: ['y']}}).includes('+ x'), ledDiffHtml({changed: false}), ledDiffHtml(null)]") == [True, "", ""])
        check("ledLoadVisible fetches only on-screen rows", ev("App.ledEvents.filter(e => e.key && e.action !== 'created').length") >= ev("[...App.ledDiffs.keys()].length"))

        # versions: diff per version, created marker on the first
        ev(f"switchTab('search'); selectEntry('{N}'); switchDetailTab('history')")
        page.wait_for_function("document.querySelectorAll('#verList .ver-ev').length === 2 && document.querySelector('#verList .ver-note') && document.querySelector('#verList .ver-fld, #verList .ver-dif')", timeout=8000)
        check("v1 is marked created, v2 shows what changed", ev("document.querySelectorAll('#verList .ver-ev')[1].textContent.includes('created') && document.querySelectorAll('#verList .ver-ev')[0].querySelector('.ver-fld, .ver-dif') !== null"))
        check("version header counts them", ev("document.querySelector('#verList .ver-h').textContent.includes('2 versions')"))
        ev("App.currentHistoryList = []; renderVersions()")
        check("no history, no ledger", ev("document.querySelector('#verList .ver-empty') !== null"))
        ev(f"selectEntry('{N}')")

        # modals: type switch, slugs, block prompt, toasts, Escape
        ev("openNewModal('knowledge'); switchNewModalType('incident')")
        check("switchNewModalType builds the incident form", ev("App.newModalKind") == "incident" and ev("!!document.getElementById('newService') && !!document.getElementById('newSeverity') && document.getElementById('modalTypeIncident').classList.contains('active')"))
        ev("autoSlugIncident('Disk Full on cache-01!')")
        check("incident key is dated and slugged", ev("document.getElementById('newKey').value").endswith("-disk-full-on-cache-01") and ev("document.getElementById('newKey').value").startswith("inc-20"))
        ev("switchNewModalType('task'); autoSlugTask('Rotate the Keys')")
        check("task key is prefixed and slugged", ev("document.getElementById('newKey').value") == "task-rotate-the-keys")
        ev("switchNewModalType('knowledge'); autoSlugKey('Postgres  Replicas & Failover')")
        check("knowledge key is slugged", ev("document.getElementById('newKey').value") == "postgres-replicas-failover")
        ev("document.getElementById('newKey').dataset.touched = 'true'; autoSlugKey('Something Else')")
        check("a hand-edited key is not overwritten", ev("document.getElementById('newKey').value") == "postgres-replicas-failover")
        ev("submitNewModal()")
        check("submitting an incomplete form shows an error toast and keeps the dialog", ev("[...document.querySelectorAll('#toastContainer .toast.error')].some(t => t.textContent.includes('required'))") and ev("__t.open()") == ["newModal"])
        ev("showToast('hello', 'success')")
        check("showToast renders with its tone", ev("[...document.querySelectorAll('#toastContainer .toast.success')].some(t => t.textContent.includes('hello'))"))
        page.keyboard.press("Escape")
        check("Escape closes the dialog", ev("__t.open()") == [])
        ev(f"promptBlockTask('{K}')")
        check("promptBlockTask names the task and opens the dialog", ev("__t.open()") == ["blockTaskModal"] and K in ev("document.getElementById('blockTaskTargetText').textContent"))
        ev("closeModal('blockTaskModal')")
        check("closeModal closes", ev("__t.open()") == [])

        # keyboard: global, incidents, tasks
        page.mouse.click(5, 400)  # drop focus from any input
        for key, tab in (("2", "incidents"), ("3", "tasks"), ("4", "graph"), ("5", "feed"), ("1", "search")):
            page.keyboard.press(key)
            check(f"key {key} opens {tab}", ev("App.activeTab") == tab)
        page.keyboard.press("/")
        check("/ focuses the search box", ev("document.activeElement.id") == "globalSearch")
        page.keyboard.press("Escape")
        page.mouse.click(5, 400)
        page.keyboard.press("n")
        check("n opens the new-entry dialog", ev("__t.open()") == ["newModal"])
        page.keyboard.press("Escape")
        page.keyboard.press("Shift+?")
        check("? opens the shortcuts dialog", ev("__t.open()") == ["shortcutsModal"])
        page.keyboard.press("Escape")
        ev(f"selectEntry('{N}')")
        page.wait_for_function(f"App.selectedKey === '{N}'", timeout=5000)
        page.mouse.click(5, 400)
        page.keyboard.press("e")
        page.wait_for_function("document.querySelector('#editModal.open')", timeout=5000)
        check("e edits the selected entry", ev("document.getElementById('editKey').value") == N)
        page.keyboard.press("Escape")
        ev("setGlobalSearchQuery(''); App.currentFilter = 'all'; App.isBackendSearchResults = false; renderKnowledgeList()")
        check("the full list is back for keyboard navigation", ev("getFilteredEntries().length") >= 2, ev("getFilteredEntries().length"))
        first = ev("getFilteredEntries()[0].key")
        ev(f"selectEntry('{first}')")
        page.keyboard.press("j")
        page.wait_for_function(f"App.selectedKey === getFilteredEntries()[1].key", timeout=5000)
        check("j moves to the next entry", True)
        page.keyboard.press("k")
        page.wait_for_function(f"App.selectedKey === '{first}'", timeout=5000)
        check("k moves back", True)
        ev("switchTab('incidents'); filterIncidents('all'); filterIncidentsSeverity('all'); App.incidentQueue = null; App.incidentCursor = 0; renderIncidents()")
        page.keyboard.press("j")
        check("incidents: j moves the cursor", ev("App.incidentCursor") == 1 and ev("document.querySelector('#incidentRows .inc-r.cur').dataset.idx") == "1")
        page.keyboard.press("k")
        check("incidents: k moves it back", ev("App.incidentCursor") == 0)
        page.keyboard.press("r")
        check("incidents: r opens Resolve for the cursor row", ev("__t.open()") == ["resolveIncidentModal"] and ev("App.activeResolveKey") == ev("incFiltered()[0].key"))
        page.keyboard.press("Escape")
        page.keyboard.press("e")
        page.wait_for_function("document.querySelector('#editModal.open')", timeout=5000)
        check("incidents: e edits the cursor row", ev("document.getElementById('editKey').value") == ev("incFiltered()[0].key"))
        page.keyboard.press("Escape")
        page.keyboard.press("Enter")
        check("incidents: Enter opens the full report", ev("location.hash") == "#/knowledge/" + ev("incFiltered()[0].key"))
        ev("switchTab('incidents'); incSelectQueue('abandoned')")
        page.keyboard.press("Escape")
        check("incidents: Escape clears the queue", ev("App.incidentQueue") is None)
        ev("switchTab('tasks'); taskSetStatus('live'); App.taskQueue = null; App.taskCursor = 0; renderTasks()")
        page.keyboard.press("j")
        check("tasks: j moves the cursor", ev("App.taskCursor") == 1)
        page.keyboard.press("k")
        page.keyboard.press("x")
        check("tasks: x opens Done for the cursor row", ev("__t.open()") == ["resolveTaskModal"] and ev("App.activeResolveKey") == ev("taskFiltered()[0].key"))
        page.keyboard.press("Escape")
        page.keyboard.press("m")
        check("tasks: m focuses the move menu", ev("document.activeElement.tagName") == "SELECT")
        page.mouse.click(5, 400)
        ev("switchTab('tasks')")
        page.keyboard.press("Enter")
        check("tasks: Enter opens the doc", ev("location.hash") == "#/knowledge/" + ev("taskFiltered()[0].key"))

        # router: every hash form lands where it says
        for h, want in ((f"#/knowledge/{N}", ("search", N)), ("#/incidents", ("incidents", None)), (f"#/tasks/{K}", ("tasks", None)), ("#/feed", ("feed", None)), ("#/graph", ("graph", None)), ("#/search?tag=kyb", ("search", None))):
            ev(f"location.hash = '{h}'")
            page.wait_for_function(f"App.activeTab === '{want[0]}'", timeout=5000)
            if want[1]:
                page.wait_for_function(f"App.selectedKey === '{want[1]}'", timeout=5000)
            check(f"route {h} -> {want[0]}" + (f" + {want[1]}" if want[1] else ""), True)
        page.wait_for_timeout(500)
        check("route ?tag= filters the list", ev("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key)") == ["kyb-service"], ev("[[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key), App.searchQuery, App.isBackendSearchResults, document.getElementById('globalSearch').value]"))
        ev("location.hash = '#/search'")
        page.wait_for_function("App.tagFilter === null && document.querySelectorAll('#knowledgeList .item-card').length > 1", timeout=5000)
        check("route tag -> plain search drops the tag filter", ev("document.getElementById('globalSearch').value") == "")
        ev("location.hash = '#/search?tag=kyb'")
        page.wait_for_function("App.tagFilter === 'kyb'", timeout=5000)
        ev("location.hash = '#/search?q=Mentions'")
        page.wait_for_function("App.tagFilter === null && App.isBackendSearchResults === true", timeout=8000)
        check("route tag -> query search drops the tag filter and finds the mention", "mentions-kyb" in ev("App.entries.map(e => e.key)"))
        ev("document.getElementById('globalSearch').value = ''; document.getElementById('globalSearch').dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}))")
        page.wait_for_function("App.isBackendSearchResults === false", timeout=8000)
        ev(f"location.hash = '#/graph/{N}'")
        page.wait_for_function(f"App.activeTab === 'graph' && document.getElementById('drawerKey').textContent === '{N}'", timeout=8000)
        check("route #/graph/<node> opens the drawer on it", True)
        ev("location.hash = '#/search'")

        # topology graph: built from the data, drawn on the canvas
        ev("App.isBackendSearchResults = false; App.searchQuery = ''; document.getElementById('globalSearch').value = ''")
        ev("refreshAll()")
        page.wait_for_function("App.entries.length >= 7", timeout=8000)
        ev("switchTab('graph'); buildTopology()")
        g = ev("({nodes: App.graphNodes.length, hubs: App.graphNodes.filter(n => n.isHub).length, entries: App.graphNodes.filter(n => !n.isHub).length, links: App.graphLinks.length, hosts: App.graphNodes.filter(n => n.type === 'host').map(n => n.label), svcs: App.graphNodes.filter(n => n.type === 'service').map(n => n.label), n: App.entries.length})")
        check("every entry is a node and hubs come from hosts/services", g["entries"] == g["n"] and "h1" in g["hosts"] and "SVC" in g["svcs"] and g["links"] >= g["entries"], g)
        check("each entry links to its service or host hub", ev(f"App.graphLinks.some(l => (l.source.id === '{I}-seed' && l.target.id === 'svc:svc') || (l.target.id === '{I}-seed' && l.source.id === 'svc:svc') || (l.source.id === '{I}-seed' && l.target.id === 'host:h1') || (l.target.id === '{I}-seed' && l.source.id === 'host:h1'))"))
        page.wait_for_timeout(800)
        check("the canvas has something drawn on it", ev("(() => { const c = document.getElementById('topoCanvas'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data; let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n++; return n > 1000; })()"))
        ev("setGraphLayout('grid'); calculateLayoutTargets('grid')")
        check("layout targets are set for every node", ev("App.graphNodes.every(n => Number.isFinite(n.targetX) && Number.isFinite(n.targetY))"))
        ev("setGraphLayout('orbit')")
        ev(f"focusNode(App.graphNodes.find(n => n.id === '{N}'))")
        check("focusNode aims the camera at the node with zoom >= 1.3", ev("graphCamera().targetZoom") >= 1.3)
        ev(f"openGraphDrawer(App.graphNodes.find(n => n.id === '{N}'))")
        check("openGraphDrawer fills key, title, badge, links and body", ev(f"document.getElementById('drawerKey').textContent === '{N}' && document.getElementById('drawerTitle').textContent.length > 0 && document.getElementById('drawerBadge').textContent === 'KNOWLEDGE' && document.getElementById('drawerLinks').children.length > 0") and ev("document.getElementById('drawerOpenFullBtn').style.display") == "inline-block")
        ev("openGraphDrawer(App.graphNodes.find(n => n.id === 'svc:svc'))")
        check("a hub drawer hides entry-only actions", ev("document.getElementById('drawerOpenFullBtn').style.display") == "none" and ev("document.getElementById('drawerBadge').textContent") == "SERVICE HUB")
        ev("closeGraphDrawer()")
        check("highlightCard reports whether it found a card", ev("highlightCard('tasks', 'no-such-key')") is False)


        # ------------------------------------------------------------------
        # Usability fixes the owner asked for, held in place.
        # ------------------------------------------------------------------
        ev("switchTab('search'); setGlobalSearchQuery('')")
        ev("const el = document.getElementById('globalSearch'); el.value = '#kyb'; el.dispatchEvent(new Event('input', {bubbles: true}))")
        check("typing #tag in the search box filters by that tag", ev("App.tagFilter") == "kyb" and ev("[...document.querySelectorAll('#knowledgeList .item-card')].map(c => c.dataset.key)") == ["kyb-service"])
        ev("const el = document.getElementById('globalSearch'); el.value = 'kyb'; el.dispatchEvent(new Event('input', {bubbles: true}))")
        check("dropping the # makes it a text search again", ev("App.tagFilter") is None and ev("App.searchQuery") == "kyb")
        ev("setGlobalSearchQuery('')")
        ev(f"selectEntry('{I}-seed')")
        page.wait_for_function(f"App.selectedKey === '{I}-seed' && document.querySelector('.incident-hero-panel')", timeout=5000)
        check("the incident card no longer offers a pointless 'Incidents View' button", "Incidents View" not in ev("document.getElementById('detailView').textContent") and "Kanban View" not in ev("document.getElementById('detailView').textContent"))
        n_hist = ev("performance.getEntriesByType('resource').filter(r => r.name.includes('/history')).length")
        ev("App.entryPrefetch.clear(); prefetchEntry('mentions-kyb')")
        page.wait_for_function("App.entryPrefetch.has('mentions-kyb')", timeout=5000)
        check("hovering a card fetches the entry but not its history", ev("performance.getEntriesByType('resource').filter(r => r.name.includes('/history')).length") == n_hist)
        before = api(base, "GET", f"/knowledge/kyb-service/history")[1]["versions"]
        api(base, "POST", "/knowledge", {"key": "kyb-service", "title": "KYB service edited twice", "body": "The registry.", "tags": ["kyb"], "refs": ["vault://kyb"]})
        after = api(base, "GET", f"/knowledge/kyb-service/history")[1]["versions"]
        check("the server's history cache is dropped by a write", len(after) == len(before) + 1, (len(before), len(after)))
        for n in range(45):
            api(base, "POST", "/incidents", {"key": f"inc-page-{stamp}-{n:02d}", "title": f"Paged incident {n}", "body": "b", "service": "pager", "severity": "low", "detection": "true"})
        ev("refreshAll()")
        page.wait_for_function("App.incidents.length >= 45", timeout=8000)
        ev("switchTab('incidents'); filterIncidents('all'); filterIncidentsSeverity('all'); App.incidentQueue = null; App.incidentCursor = 0; App.incidentShown = 40; renderIncidents()")
        total = ev("incFiltered().length")
        check("a long incident list shows 40 rows and says how many more", len(rows()) == 40 and ev("document.querySelector('#incidentRows .inc-more').textContent").startswith(f"{total - 40} more"), (len(rows()), total))
        ev("document.querySelector('#incidentRows .inc-more').click()")
        check("'show more' extends the page", len(rows()) == min(80, total))
        ev("App.incidentShown = 40; App.incidentCursor = 39; renderIncidents()")
        page.keyboard.press("j")
        check("the cursor walking past the page extends it", ev("App.incidentCursor") == 40 and len(rows()) >= 41)
        ev("filterIncidentsSeverity('low')")
        check("changing a filter resets the page", ev("App.incidentShown") == 40)
        ev("filterIncidentsSeverity('all'); incSelectQueue('unverifiable'); App.incidentShown = 80; incClearCell()")
        check("Escape/clearing the queue resets the page too", ev("App.incidentShown") == 40)
        ev("const el = document.getElementById('globalSearch'); el.value = '#prod.codes'; el.dispatchEvent(new Event('input', {bubbles: true}))")
        check("a tag with a dot is still a tag when typed", ev("App.tagFilter") == "prod.codes")
        ev("setGlobalSearchQuery('')")
        for n in range(45):
            api(base, "POST", "/tasks", {"key": f"task-page-{stamp}-{n:02d}", "title": f"Paged task {n}", "body": "b", "status": "open"})
        ev("refreshAll()")
        page.wait_for_function("App.tasks.length >= 45", timeout=8000)
        ev("switchTab('tasks'); taskSetStatus('live'); App.taskQueue = null; App.taskCursor = 0; renderTasks()")
        ttotal = ev("taskFiltered().length")
        check("a long task list pages at 40", len(trows()) == 40 and ev("document.querySelector('#taskRows .inc-more').textContent").startswith(f"{ttotal - 40} more"), (len(trows()), ttotal))
        ev("document.querySelector('#taskRows .inc-more').click()")
        check("'show more' extends the task page", len(trows()) == min(80, ttotal))
        ev("App.taskShown = 40; App.taskCursor = 39; renderTasks()")
        page.keyboard.press("j")
        check("the task cursor walking past the page extends it", ev("App.taskCursor") == 40 and len(trows()) >= 41)
        ev("taskSetStatus('live')")
        check("changing the task filter resets its page", ev("App.taskShown") == 40)
        # archiving (DELETE of an open incident) is a history change the cache must see
        # (a resolved incident is archived by the server on its own, so use an open one)
        hist0 = len(api(base, "GET", f"/knowledge/{I}-open1/history")[1]["versions"])
        d = api(base, "DELETE", f"/knowledge/{I}-open1")
        check("DELETE archives an open incident", d[0] == 200, d)
        hist1 = api(base, "GET", f"/knowledge/{I}-open1/history")[1]
        check("archiving an incident shows up in its history (cache dropped)", len(hist1.get("versions", [])) == hist0 + 1 and hist1["versions"][0]["change"] == "deleted", (hist0, len(hist1.get("versions", [])), [v["change"] for v in hist1.get("versions", [])]))

        check("no script errors in the per-function checks", not errors, "; ".join(errors[:3]))
        browser.close()


if __name__ == "__main__":
    import urllib.parse  # noqa: E402  (used in main)
    main()
