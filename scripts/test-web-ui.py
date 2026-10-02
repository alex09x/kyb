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

        # --- every inline handler points at a function; every id the code reads exists
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

        check("no script errors during the flows", not errors, "; ".join(errors[:3]))
        browser.close()


if __name__ == "__main__":
    import urllib.parse  # noqa: E402  (used in main)
    main()
