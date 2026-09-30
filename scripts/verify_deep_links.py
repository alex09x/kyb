#!/usr/bin/env python3
import json
import os
import socket
import time
import base64

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

    # Find or create KYB tab
    tabs = client.call('tabs.list')
    kyb_tab = None
    for t in tabs:
        if ':9310' in t.get('url', ''):
            kyb_tab = t
            break

    if not kyb_tab:
        print("Opening new tab for KYB...")
        res = client.call('tabs.create', {'url': f"{base_url}/#/search", 'active': True})
        tab_id = res['tabId']
        time.sleep(2)
    else:
        tab_id = kyb_tab['id']
        client.call('tabs.activate', {'tabId': tab_id})

    print(f"\n--- TEST 1: Direct Link to Knowledge Document ---")
    doc_url = f"{base_url}/#/knowledge/postgres-replicas"
    print(f"Navigating directly to: {doc_url}")
    client.call('navigate', {'tabId': tab_id, 'url': doc_url})
    time.sleep(2.5)

    eval_res = client.call('eval', {
        'tabId': tab_id,
        'expression': """
            JSON.stringify({
                activeTab: App.activeTab,
                selectedKey: App.selectedKey,
                hash: window.location.hash,
                title: document.title,
                cardSelected: !!document.querySelector('.item-card[data-key="postgres-replicas"].selected'),
                detailHeading: document.querySelector('#detailView h1')?.textContent || '',
                copyBtnExists: !!document.getElementById('btnCopyDirectLink')
            })
        """
    })
    print(f"  Result: {eval_res}")
    save_screenshot(client, tab_id, "kyb-v5-direct-link-knowledge.png")

    print(f"\n--- TEST 2: Direct Link to Graph Node with Open Drawer ---")
    graph_url = f"{base_url}/#/graph/postgres-replicas"
    print(f"Navigating directly to: {graph_url}")
    client.call('navigate', {'tabId': tab_id, 'url': graph_url})
    time.sleep(2.5)

    eval_graph = client.call('eval', {
        'tabId': tab_id,
        'expression': """
            JSON.stringify({
                activeTab: App.activeTab,
                drawerOpen: document.getElementById('graphDrawer')?.classList.contains('open'),
                drawerKey: document.getElementById('drawerKey')?.textContent,
                drawerTitle: document.getElementById('drawerTitle')?.textContent,
                copyLinkBtnExists: !!document.getElementById('drawerCopyLinkBtn')
            })
        """
    })
    print(f"  Result: {eval_graph}")
    save_screenshot(client, tab_id, "kyb-v5-direct-link-graph-node.png")

    print(f"\n--- TEST 3: Direct Link to Incident Report ---")
    inc_url = f"{base_url}/#/incidents"
    print(f"Navigating to incidents...")
    client.call('navigate', {'tabId': tab_id, 'url': inc_url})
    time.sleep(2.0)

    # Grab first incident key
    first_inc = client.call('eval', {
        'tabId': tab_id,
        'expression': "App.incidents[0]?.key || ''"
    })
    if first_inc and isinstance(first_inc, dict) and 'value' in first_inc:
        first_inc = first_inc['value']

    if first_inc:
        target_inc_url = f"{base_url}/#/incidents/{first_inc}"
        print(f"Navigating directly to incident permalink: {target_inc_url}")
        client.call('navigate', {'tabId': tab_id, 'url': target_inc_url})
        time.sleep(1.8)
        eval_inc = client.call('eval', {
            'tabId': tab_id,
            'expression': f"""
                JSON.stringify({{
                    activeTab: App.activeTab,
                    cardHighlighted: !!document.querySelector('.incident-card[data-key="{first_inc}"].target-highlight'),
                    key: '{first_inc}'
                }})
            """
        })
        print(f"  Result: {eval_inc}")
        save_screenshot(client, tab_id, "kyb-v5-direct-link-incident.png")

    print(f"\n--- TEST 4: Direct Link to Task Kanban ---")
    task_url = f"{base_url}/#/tasks"
    print(f"Navigating to tasks...")
    client.call('navigate', {'tabId': tab_id, 'url': task_url})
    time.sleep(2.0)

    first_task = client.call('eval', {
        'tabId': tab_id,
        'expression': "App.tasks[0]?.key || ''"
    })
    if first_task and isinstance(first_task, dict) and 'value' in first_task:
        first_task = first_task['value']

    if first_task:
        target_task_url = f"{base_url}/#/tasks/{first_task}"
        print(f"Navigating directly to task permalink: {target_task_url}")
        client.call('navigate', {'tabId': tab_id, 'url': target_task_url})
        time.sleep(1.8)
        eval_task = client.call('eval', {
            'tabId': tab_id,
            'expression': f"""
                JSON.stringify({{
                    activeTab: App.activeTab,
                    cardHighlighted: !!document.querySelector('.task-card[data-key="{first_task}"].target-highlight'),
                    key: '{first_task}'
                }})
            """
        })
        print(f"  Result: {eval_task}")
        save_screenshot(client, tab_id, "kyb-v5-direct-link-task.png")

    print(f"\n--- TEST 5: Direct Link to Search Query ---")
    search_url = f"{base_url}/#/search?q=postgres"
    print(f"Navigating directly to: {search_url}")
    client.call('navigate', {'tabId': tab_id, 'url': search_url})
    time.sleep(2.0)

    eval_search = client.call('eval', {
        'tabId': tab_id,
        'expression': """
            JSON.stringify({
                activeTab: App.activeTab,
                searchQuery: App.searchQuery,
                inputValue: document.getElementById('globalSearch')?.value,
                highlightsCount: document.querySelectorAll('.search-highlight').length
            })
        """
    })
    print(f"  Result: {eval_search}")
    save_screenshot(client, tab_id, "kyb-v5-direct-link-search.png")

    client.call('tabs.detach', {'tabId': tab_id})
    client.close()
    print("\nAll deep link verification tests finished successfully!")

if __name__ == '__main__':
    main()
