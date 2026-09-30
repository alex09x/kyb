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
        print(f"  [SAVED] {path}")
        return path
    return None

def main():
    client = ProdBrowserClient()
    print("Connected to Prod Browser daemon.")
    host = get_kyb_host()
    base_url = f"http://{host}:9310"

    # Query sample keys directly from HTTP API
    hits_data = http_get_json(f"{base_url}/search?limit=5")
    sample_doc_key = hits_data['hits'][0]['key'] if hits_data.get('hits') else 'postgres-replicas'

    inc_data = http_get_json(f"{base_url}/incidents?all=1")
    sample_inc_key = inc_data['incidents'][0]['key'] if inc_data.get('incidents') else 'inc-test'

    task_data = http_get_json(f"{base_url}/tasks?all=1")
    sample_task_key = task_data['tasks'][0]['key'] if task_data.get('tasks') else 'task-test'

    print(f"Discovered sample keys:")
    print(f"  Doc:      {sample_doc_key}")
    print(f"  Incident: {sample_inc_key}")
    print(f"  Task:     {sample_task_key}")

    # Close any existing test tabs on :9310 first to ensure a clean state
    tabs = client.call('tabs.list')
    for t in tabs:
        if ':9310' in t.get('url', ''):
            try:
                client.call('tabs.close', {'tabId': t['id']})
            except Exception:
                pass

    test_routes = [
        ("knowledge", f"/#/knowledge/{sample_doc_key}", "Direct Link to Knowledge Document"),
        ("graph-node", f"/#/graph/{sample_doc_key}", "Direct Link to Graph Node with Open Drawer"),
        ("incident", f"/#/incidents/{sample_inc_key}", "Direct Link to Incident Report"),
        ("task", f"/#/tasks/{sample_task_key}", "Direct Link to Task Kanban"),
        ("search", "/#/search?q=postgres", "Direct Link to Search Query"),
        ("feed", "/#/feed", "Direct Link to Operations Activity Stream")
    ]

    for name, route, description in test_routes:
        url = f"{base_url}{route}"
        print(f"\n--- TEST: {description} ---")
        print(f"Opening cold-boot tab: {url}")
        res = client.call('tabs.create', {'url': url, 'active': True})
        tab_id = res['tabId']
        time.sleep(3.2)
        save_screenshot(client, tab_id, f"kyb-v5-direct-link-{name}.png")
        if name != "feed": # keep last one open for interactive browsing
            client.call('tabs.close', {'tabId': tab_id})

    client.close()
    print("\nAll cold-boot deep link tests completed successfully!")

if __name__ == '__main__':
    main()
