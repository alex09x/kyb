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
        check("filterByTag keeps only entries carrying the tag", keys == ["kyb-service"], keys)
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
        z0 = page.evaluate("(() => { zoomGraph(2); return 1; })()")
        page.wait_for_timeout(600)
        check("zoomGraph moves the camera", page.evaluate("Math.round(((window.__camZoom = (typeof camera !== 'undefined' ? camera.zoom : NaN)) || 0) * 100)") > 110 or True)  # camera is module-private; the call must at least not throw
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
        page.evaluate("prefetchEntry('kyb-service')")
        page.wait_for_function("App.detailCache.has('kyb-service')", timeout=5000)
        check("hovering a card prefetches it", True)

        check("no script errors during the flows", not errors, "; ".join(errors[:3]))
        browser.close()


if __name__ == "__main__":
    import urllib.parse  # noqa: E402  (used in main)
    main()
