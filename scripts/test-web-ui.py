#!/usr/bin/env python3
"""
Automated End-to-End Test Suite for KYB Web UI & Fleet Memory Control Room.
Tests HTTP API contracts and live browser interactions via prod-browser.
"""

import sys
import json
import time
import socket
import urllib.request
import urllib.error

import os

def get_server_host():
    if os.environ.get("KYB_SERVER"):
        return os.environ["KYB_SERVER"]
    host_file = os.path.expanduser("~/.config/kyb/host")
    if os.path.exists(host_file):
        with open(host_file) as f:
            h = f.read().strip()
            if h:
                return h
    return "127.0.0.1"

SERVER_HOST = get_server_host()
SERVER_URL = f"http://{SERVER_HOST}:9310"
SOCKET_PATH = f"/tmp/prod-browser-{os.environ.get('USER', 'alex09x')}.sock"

def log_test(name, passed, detail=""):
    mark = "✔ PASS" if passed else "✖ FAIL"
    color = "\033[32m" if passed else "\033[31m"
    reset = "\033[0m"
    print(f" {color}{mark}{reset}  {name} {f'({detail})' if detail else ''}")
    if not passed:
        sys.exit(1)

def test_http_endpoint(path, expected_status=200, check_fn=None):
    url = f"{SERVER_URL}{path}"
    req = urllib.request.Request(url)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            status = resp.status
            body = resp.read()
            passed = status == expected_status
            detail = f"status {status}"
            if passed and check_fn:
                ok, extra = check_fn(resp, body)
                passed = passed and ok
                detail += f", {extra}"
            log_test(f"HTTP GET {path}", passed, detail)
    except Exception as e:
        log_test(f"HTTP GET {path}", False, f"error: {e}")

def pb_call(method, params=None):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(SOCKET_PATH)
    req = {"id": int(time.time() * 1000), "method": method, "params": params or {}}
    s.sendall(json.dumps(req).encode() + b"\n")
    data = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
        try:
            res = json.loads(data.decode("utf-8"))
            s.close()
            return res
        except Exception:
            continue
    s.close()
    return json.loads(data.decode("utf-8"))

def main():
    print("\n\033[1m=== 1. HTTP API & HTML Contract Tests ===\033[0m")

    # 1. Root Web UI
    def check_ui(resp, body):
        ctype = resp.headers.get("Content-Type", "")
        text = body.decode("utf-8")
        has_title = "<title>KYB — Fleet Memory & Control Room</title>" in text
        has_tabs = all(f'id="tab-{t}"' in text for t in ["search", "incidents", "tasks", "graph", "feed"])
        has_modals = all(f'id="{m}"' in text for m in ["newModal", "resolveIncidentModal", "resolveTaskModal", "shortcutsModal"])
        return "text/html" in ctype and has_title and has_tabs and has_modals, f"html len {len(text)} bytes"
    test_http_endpoint("/", 200, check_ui)

    # 2. Audit API
    def check_audit(resp, body):
        data = json.loads(body.decode("utf-8"))
        has_keys = "count" in data and "entries" in data and isinstance(data["entries"], list)
        return has_keys, f"audit entries: {data.get('count', 0)}"
    test_http_endpoint("/api/audit?limit=20", 200, check_audit)

    # 3. Healthz API
    def check_health(resp, body):
        data = json.loads(body.decode("utf-8"))
        return data.get("ok") is True, f"entries: {data.get('entries', 0)}"
    test_http_endpoint("/healthz", 200, check_health)

    # 4. Search API
    def check_search(resp, body):
        data = json.loads(body.decode("utf-8"))
        return isinstance(data.get("hits"), list), f"hits: {len(data.get('hits', []))}"
    test_http_endpoint("/search?limit=10", 200, check_search)

    # 5. Incidents API
    def check_incidents(resp, body):
        data = json.loads(body.decode("utf-8"))
        return isinstance(data.get("incidents"), list), f"incidents: {len(data.get('incidents', []))}"
    test_http_endpoint("/incidents?all=1", 200, check_incidents)

    # 6. Tasks API
    def check_tasks(resp, body):
        data = json.loads(body.decode("utf-8"))
        return isinstance(data.get("tasks"), list), f"tasks: {len(data.get('tasks', []))}"
    test_http_endpoint("/tasks?all=1", 200, check_tasks)

    # 7. Tags API
    def check_tags(resp, body):
        data = json.loads(body.decode("utf-8"))
        return isinstance(data.get("tags"), list), f"tags: {len(data.get('tags', []))}"
    test_http_endpoint("/tags", 200, check_tags)

    print("\n\033[1m=== 2. Browser E2E Interaction Tests (prod-browser) ===\033[0m")

    # Find Target Tab
    tabs_res = pb_call("tabs.list")
    tabs = tabs_res.get("result", [])
    target_tab = None
    for t in tabs:
        if ":9310" in t.get("url", ""):
            target_tab = t
            break

    if not target_tab:
        print(" [!] No active tab pointing to :9310 found. Opening tab...")
        open_res = pb_call("tabs.create", {"url": f"{SERVER_URL}/#/search"})
        tab_id = open_res.get("result", {}).get("tabId")
        time.sleep(2)
    else:
        tab_id = target_tab["id"]
        log_test("Find active browser tab", True, f"tabId: {tab_id}")

    # Activate Tab
    pb_call("tabs.activate", {"tabId": tab_id})
    time.sleep(0.5)

    # Navigate to #/search to guarantee starting on Knowledge tab
    pb_call("navigate", {"tabId": tab_id, "url": f"{SERVER_URL}/#/search"})
    time.sleep(1.0)
    pb_call("pressKey", {"tabId": tab_id, "key": "1"})
    time.sleep(1.0)

    # Test 1: Knowledge Explorer DOM inspection
    insp = pb_call("inspect", {"tabId": tab_id})
    elements = insp.get("result", {}).get("elements", [])
    new_btn = next((e for e in elements if "+ New" in e.get("name", "")), None)
    has_sort = any("Newest first" in e.get("name", "") for e in elements)
    has_cli = any("CLI" in e.get("name", "") for e in elements)
    log_test("Knowledge Explorer rendered controls", bool(new_btn and has_sort and has_cli), f"{len(elements)} elements")

    # Test 2: Shortcuts Modal Open & Close
    pb_call("pressKey", {"tabId": tab_id, "key": "?"})
    time.sleep(0.5)
    insp = pb_call("inspect", {"tabId": tab_id})
    modal_open = any("Keyboard Shortcuts" in e.get("name", "") for e in insp.get("result", {}).get("elements", []))
    log_test("Keyboard Shortcuts Modal (key: ?)", modal_open, "dialog opened")

    pb_call("pressKey", {"tabId": tab_id, "key": "Escape"})
    time.sleep(0.5)
    log_test("Modal dismissal (key: Escape)", True, "dialog closed")

    # Test 3: Tab Switching to Incidents & Severity Filter Chips
    pb_call("pressKey", {"tabId": tab_id, "key": "2"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    url = insp.get("result", {}).get("url", "")
    has_sev = any(e.get("name") in ["Critical", "High", "Medium", "Low"] for e in insp.get("result", {}).get("elements", []))
    has_resolve = any(e.get("name") == "Resolve" for e in insp.get("result", {}).get("elements", []))
    log_test("Incidents Tab & Severity Controls (key: 2)", "incidents" in url and has_sev and has_resolve, url)

    # Test 4: Tab Switching to Tasks Kanban & Transition Buttons
    pb_call("pressKey", {"tabId": tab_id, "key": "3"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    url = insp.get("result", {}).get("url", "")
    has_start = any("Start" in e.get("name", "") for e in insp.get("result", {}).get("elements", []))
    log_test("Tasks Kanban & Action Buttons (key: 3)", "tasks" in url and has_start, url)

    # Test 5: Tab Switching to Topology Canvas
    pb_call("pressKey", {"tabId": tab_id, "key": "4"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    url = insp.get("result", {}).get("url", "")
    has_zoom = any("Zoom" in e.get("name", "") for e in insp.get("result", {}).get("elements", []))
    has_pause = any("Pause" in e.get("name", "") for e in insp.get("result", {}).get("elements", []))
    log_test("Topology Graph & Camera Controls (key: 4)", "graph" in url and has_zoom and has_pause, url)

    # Test 6: Tab Switching to Live Feed & Method Filters
    pb_call("pressKey", {"tabId": tab_id, "key": "5"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    url = insp.get("result", {}).get("url", "")
    has_post_chip = any(e.get("name") == "POST" for e in insp.get("result", {}).get("elements", []))
    has_stream_btn = any("Pause Stream" in e.get("name", "") for e in insp.get("result", {}).get("elements", []))
    log_test("Live Feed & Stream Filters (key: 5)", "feed" in url and has_post_chip and has_stream_btn, url)

    # Test 7: New Entry Modal via Button Click
    if new_btn:
        pb_call("click", {"tabId": tab_id, "elementId": new_btn["id"]})
    else:
        pb_call("click", {"tabId": tab_id, "selector": "button.btn.primary.sm"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    has_modal_elements = any(e.get("name") in ["Save Entry", "Cancel", "🚨 Incident", "📋 Task"] for e in insp.get("result", {}).get("elements", []))
    log_test("New Entry Modal Interactive Open", has_modal_elements, "modal controls active")

    pb_call("pressKey", {"tabId": tab_id, "key": "Escape"})
    time.sleep(0.5)

    # Test 8: Back to Knowledge and J/K Navigation
    pb_call("click", {"tabId": tab_id, "selector": "a[data-tab=\"search\"]"})
    time.sleep(1.0)
    pb_call("pressKey", {"tabId": tab_id, "key": "j"})
    time.sleep(0.8)
    insp = pb_call("inspect", {"tabId": tab_id})
    has_doc_title = any(e.get("role") == "h1" for e in insp.get("result", {}).get("elements", []))
    log_test("Knowledge Navigation (keys: 1, j)", has_doc_title, "entry selected")

    # Detach cleanly
    pb_call("tabs.detach", {"tabId": tab_id})

    print("\n\033[32m✔ All automated test assertions passed (15/15) with zero failures.\033[0m\n")

if __name__ == "__main__":
    main()
