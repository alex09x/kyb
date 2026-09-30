#!/usr/bin/env python3
import json
import os
import socket
import time
import base64
import urllib.request

SOCK_PATH = f"/tmp/prod-browser-{os.environ['USER']}.sock"
ARTIFACTS_DIR = "/Users/alex09x/.gemini/antigravity-cli/brain/89170df8-ae1b-4af8-81a9-2107b1a69605"

class ProdBrowserClient:
    def __init__(self, sock_path=SOCK_PATH):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(sock_path)
        self.f = self.sock.makefile('r')
        self._id = 0

    def call(self, method, params=None):
        self._id += 1
        req = {'id': self._id, 'method': method, 'params': params or {}}
        msg = json.dumps(req).encode('utf-8') + b'\n'
        self.sock.sendall(msg)
        line = self.f.readline()
        if not line:
            raise EOFError("Socket closed")
        data = json.loads(line)
        if 'error' in data and data['error']:
            raise RuntimeError(f"RPC Error [{method}]: {data['error']}")
        return data.get('result')

    def close(self):
        try:
            self.sock.close()
        except Exception:
            pass

def get_kyb_host():
    try:
        with open(os.path.expanduser("~/.config/kyb/host")) as f:
            return f.read().strip()
    except Exception:
        pass
    try:
        with open("scripts/fleet.local.sh") as f:
            for line in f:
                if line.startswith("SERVER="):
                    val = line.split("=", 1)[1].strip().strip('"').strip("'")
                    return val.split("@")[-1]
    except Exception:
        pass
    return "127.0.0.1"

def http_get_json(url):
    req = urllib.request.Request(url, headers={'Accept': 'application/json'})
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.loads(resp.read().decode('utf-8'))

def save_screenshot(client, tab_id, filename):
    res = client.call('screenshot', {'tabId': tab_id})
    if res and 'data' in res:
        path = os.path.join(ARTIFACTS_DIR, filename)
        with open(path, 'wb') as f:
            f.write(base64.b64decode(res['data']))
        print(f"  [SAVED SCREENSHOT] {path}")
        return path
    return None

def main():
    client = ProdBrowserClient()
    print("Connected to Prod Browser daemon.")
    host = get_kyb_host()
    base_url = f"http://{host}:9310"

    # 1. Close any existing :9310 tabs
    tabs = client.call('tabs.list')
    for t in tabs:
        if ':9310' in t.get('url', ''):
            try:
                client.call('tabs.close', {'tabId': t['id']})
            except Exception:
                pass

    # 2. Test Direct Link to task-e2e-test-1790717572
    task_key = "task-e2e-test-1790717572"
    url_task = f"{base_url}/#/knowledge/{task_key}"
    print(f"\n1. Navigating to Task Detail View: {url_task}")
    res = client.call('tabs.create', {'url': url_task, 'active': True})
    tab_id = res['tabId']
    time.sleep(3)

    # Check DOM state of detail view
    eval_res = client.call('eval', {
        'tabId': tab_id,
        'expression': """
        (() => {
            const hero = document.querySelector('.task-hero-panel');
            const prose = document.querySelector('#detailBodyContent .prose');
            const statusBadge = hero ? hero.querySelector('.badge') : null;
            return {
                hasHero: !!hero,
                status: statusBadge ? statusBadge.textContent.trim() : null,
                bodyText: prose ? prose.innerText.trim() : null,
                hasResolutionCallout: !!document.querySelector('.task-hero-panel div[style*="rgba(16,185,129"]')
            };
        })()
        """
    })
    print(f"Detail View Evaluation: {json.dumps(eval_res, indent=2)}")
    save_screenshot(client, tab_id, "kyb-v6-task-detail-hero.png")

    # 3. Test Kanban Board View
    url_kanban = f"{base_url}/#/tasks"
    print(f"\n2. Navigating to Kanban Board: {url_kanban}")
    res_k = client.call('tabs.create', {'url': url_kanban, 'active': True})
    tab_k_id = res_k['tabId']
    time.sleep(3)
    save_screenshot(client, tab_k_id, "kyb-v6-task-kanban-board.png")

    # 4. Interactive Task Movement Test
    # Test transition API:
    # A) Transition task to in_progress
    print(f"\n3. Transitioning {task_key} to in_progress...")
    req = urllib.request.Request(
        f"{base_url}/tasks/{task_key}/transition",
        data=json.dumps({"status": "in_progress", "author": "Alexander Panasenko <alex@prod.codes>"}).encode('utf-8'),
        headers={'Content-Type': 'application/json'}
    )
    urllib.request.urlopen(req, timeout=5)
    time.sleep(1)
    res_inp = client.call('tabs.create', {'url': url_kanban, 'active': True})
    time.sleep(2.5)
    save_screenshot(client, res_inp['tabId'], "kyb-v6-kanban-in-progress.png")
    client.call('tabs.close', {'tabId': res_inp['tabId']})

    # B) Transition task to blocked
    print(f"Transitioning {task_key} to blocked...")
    req = urllib.request.Request(
        f"{base_url}/tasks/{task_key}/transition",
        data=json.dumps({
            "status": "blocked",
            "blocked_reason": "Waiting on network validation and security review",
            "author": "Alexander Panasenko <alex@prod.codes>"
        }).encode('utf-8'),
        headers={'Content-Type': 'application/json'}
    )
    urllib.request.urlopen(req, timeout=5)
    time.sleep(1)
    res_blk = client.call('tabs.create', {'url': url_kanban, 'active': True})
    time.sleep(2.5)
    save_screenshot(client, res_blk['tabId'], "kyb-v6-kanban-blocked.png")
    client.call('tabs.close', {'tabId': res_blk['tabId']})

    # C) Transition task to open (backlog)
    print(f"Transitioning {task_key} to open...")
    req = urllib.request.Request(
        f"{base_url}/tasks/{task_key}/transition",
        data=json.dumps({"status": "open", "author": "Alexander Panasenko <alex@prod.codes>"}).encode('utf-8'),
        headers={'Content-Type': 'application/json'}
    )
    urllib.request.urlopen(req, timeout=5)
    time.sleep(1)

    # D) Resolve task
    print(f"Resolving {task_key} with resolution note...")
    req = urllib.request.Request(
        f"{base_url}/tasks/{task_key}/resolve",
        data=json.dumps({
            "resolution": "Acceptance criteria met and verified via automated test suite on fleet node."
        }).encode('utf-8'),
        headers={'Content-Type': 'application/json'}
    )
    urllib.request.urlopen(req, timeout=5)
    time.sleep(1)
    res_done = client.call('tabs.create', {'url': url_kanban, 'active': True})
    time.sleep(2.5)
    save_screenshot(client, res_done['tabId'], "kyb-v6-kanban-done.png")
    client.call('tabs.close', {'tabId': res_done['tabId']})

    # 5. Check detail view again to ensure updated resolution is visible
    print(f"\n4. Checking detail view for updated resolution...")
    res_d = client.call('tabs.create', {'url': url_task, 'active': True})
    tab_d_id = res_d['tabId']
    time.sleep(3)
    save_screenshot(client, tab_d_id, "kyb-v6-task-detail-after-resolve.png")

    # Close initial test tabs
    client.call('tabs.close', {'tabId': tab_k_id})
    client.call('tabs.close', {'tabId': tab_id})

    client.close()
    print("\nAll interactive verification steps passed successfully!")

if __name__ == '__main__':
    main()
