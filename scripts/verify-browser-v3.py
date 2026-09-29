import sys
import json
import time
import socket
import base64
import os

SOCKET_PATH = f"/tmp/prod-browser-{os.environ.get('USER', 'alex09x')}.sock"
ARTIFACTS_DIR = "/Users/alex09x/.gemini/antigravity-cli/brain/89170df8-ae1b-4af8-81a9-2107b1a69605"

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

def capture_screenshot(tab_id, filename):
    res = pb_call("screenshot", {"tabId": tab_id})
    result = res.get("result") or {}
    data = result.get("data", "")
    if data:
        if "," in data:
            data = data.split(",", 1)[1]
        raw = base64.b64decode(data)
        out_path = os.path.join(ARTIFACTS_DIR, filename)
        with open(out_path, "wb") as f:
            f.write(raw)
        print(f"Captured screenshot: {out_path} ({len(raw)} bytes)")
        return out_path
    else:
        print(f"Failed to capture screenshot {filename}: {res}")
        return None

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

def eval_js(tab_id, code):
    res = pb_call("eval", {"tabId": tab_id, "expression": code})
    return res.get("result")

def main():
    tabs_res = pb_call("tabs.list")
    tabs = tabs_res.get("result", [])
    target_tab = next((t for t in tabs if ":9310" in t.get("url", "")), None)
    if not target_tab:
        print("No tab with :9310 found, opening...")
        open_res = pb_call("tabs.create", {"url": f"{SERVER_URL}/#/search"})
        tab_id = open_res.get("result", {}).get("tabId")
        time.sleep(2)
    else:
        tab_id = target_tab["id"]

    print(f"Using tab {tab_id}")
    pb_call("tabs.activate", {"tabId": tab_id})
    time.sleep(0.5)

    # 1. Reload page to get latest assets
    print("1. Reloading page...")
    pb_call("tabs.reload", {"tabId": tab_id})
    time.sleep(2.5)

    # 2. Select entry with 41 revisions and inspect history
    print("2. Selecting entry with 41 revisions: prod-code-refactor-verification-contract...")
    eval_js(tab_id, "selectEntry('prod-code-refactor-verification-contract'); switchTab('search');")
    time.sleep(1)

    eval_js(tab_id, "switchDetailTab('history');")
    time.sleep(1)
    capture_screenshot(tab_id, "kyb-v3-history-revisions.png")

    # 3. Test structured diff viewer
    print("3. Testing structured diff viewer...")
    eval_js(tab_id, "switchDetailTab('diff');")
    time.sleep(1)
    capture_screenshot(tab_id, "kyb-v3-diff-view.png")

    # 4. Live Feed Activity Stream Cards
    print("4. Switching to Tab 5 (Live Feed)...")
    eval_js(tab_id, "switchTab('feed');")
    time.sleep(1.5)
    capture_screenshot(tab_id, "kyb-v3-live-feed-cards.png")

    # 5. Interconnected Topology Hubs
    print("5. Switching to Tab 4 (Topology Hubs)...")
    eval_js(tab_id, "switchTab('graph');")
    time.sleep(2)
    capture_screenshot(tab_id, "kyb-v3-topology-hubs.png")

    # 6. Search Highlighting & Instant Filtering
    print("6. Searching 'sccache' with search term highlighting...")
    eval_js(tab_id, "switchTab('search'); const inp = document.getElementById('globalSearch'); if(inp){ inp.value = 'sccache'; App.searchQuery = 'sccache'; renderKnowledgeList(); }")
    time.sleep(1)
    capture_screenshot(tab_id, "kyb-v3-search-highlighting.png")

    # Return cleanly to search view
    eval_js(tab_id, "const inp = document.getElementById('globalSearch'); if(inp){ inp.value = ''; App.searchQuery = ''; renderKnowledgeList(); }")
    time.sleep(0.5)

    # Cleanly detach
    pb_call("tabs.detach", {"tabId": tab_id})
    print("Live browser verification completed cleanly and detached.")

if __name__ == "__main__":
    main()
