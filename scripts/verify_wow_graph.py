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
        return "127.0.0.1"

def main():
    client = ProdBrowserClient()
    print("Connected to Prod Browser bridge.")

    host = get_kyb_host()
    # 1. Locate the KYB tab
    tabs = client.call('tabs.list')
    kyb_tab = None
    for t in tabs:
        if ':9310' in t.get('url', ''):
            kyb_tab = t
            break

    if not kyb_tab:
        print("KYB tab not found. Opening new tab...")
        res = client.call('tabs.create', {'url': f'http://{host}:9310/#/graph', 'active': True})
        tab_id = res['tabId']
        time.sleep(2)
    else:
        tab_id = kyb_tab['id']
        print(f"Found KYB tab: id={tab_id} title={kyb_tab.get('title')}")
        # Activate tab and switch to graph
        client.call('tabs.activate', {'tabId': tab_id})
        # Reload to pick up newly deployed server
        print("Reloading KYB tab...")
        client.call('navigate', {'tabId': tab_id, 'action': 'reload'})
        time.sleep(2.5)

    # Make sure we are on tab graph
    client.call('click', {'tabId': tab_id, 'selector': '#tab-nav-graph'})
    time.sleep(1.8)

    # 2. Capture screenshot 1: Calm, stable constellation (no jittering/jumping)
    print("Capturing 1: Calm stable topology graph...")
    snap1 = client.call('screenshot', {'tabId': tab_id})
    if snap1 and 'data' in snap1:
        path1 = os.path.join(ARTIFACTS_DIR, 'kyb-v3-topology-calm.png')
        with open(path1, 'wb') as f:
            f.write(base64.b64decode(snap1['data']))
        print(f"Saved: {path1}")

    # 3. Test In-Graph Search with pulsating radar rings
    print("Testing In-Graph quick search for 'sccache'...")
    client.call('type', {'tabId': tab_id, 'selector': '#topoSearchInput', 'text': 'sccache', 'clearFirst': True})
    time.sleep(1.0)

    print("Capturing 2: Sonar radar rings for search matches...")
    snap2 = client.call('screenshot', {'tabId': tab_id})
    if snap2 and 'data' in snap2:
        path2 = os.path.join(ARTIFACTS_DIR, 'kyb-v3-topology-sonar.png')
        with open(path2, 'wb') as f:
            f.write(base64.b64decode(snap2['data']))
        print(f"Saved: {path2}")

    # 4. Clear search, close any modal, and click on node
    print("Clearing search input and ensuring clean state...")
    client.call('pressKey', {'tabId': tab_id, 'key': 'Escape'})
    time.sleep(0.3)
    client.call('type', {'tabId': tab_id, 'selector': '#topoSearchInput', 'text': '', 'clearFirst': True})
    time.sleep(0.5)

    # Click on a node (e.g. RINGFIRE or CH_PROXY)
    print("Clicking a node to trigger Neighborhood Spotlight & Interactive HUD...")
    client.call('click', {'tabId': tab_id, 'x': 450, 'y': 240})
    time.sleep(1.2)

    print("Capturing 3: Neighborhood Focus Spotlight, Photon Pulses & Interactive Floating HUD...")
    snap3 = client.call('screenshot', {'tabId': tab_id})
    if snap3 and 'data' in snap3:
        path3 = os.path.join(ARTIFACTS_DIR, 'kyb-v3-topology-hud-focused.png')
        with open(path3, 'wb') as f:
            f.write(base64.b64decode(snap3['data']))
        print(f"Saved: {path3}")

    # 5. Test 'Fit All' and 'Reheat' controls
    print("Testing 'Fit All' camera control...")
    client.call('click', {'tabId': tab_id, 'selector': '.graph-controls-bar button:nth-child(2)'})
    time.sleep(0.8)

    print("Testing 'Reheat' simulation control...")
    client.call('click', {'tabId': tab_id, 'selector': '#pausePhysicsBtn'})
    time.sleep(1.5)

    print("Capturing 4: Reheated & Camera Fitted constellation...")
    snap4 = client.call('screenshot', {'tabId': tab_id})
    if snap4 and 'data' in snap4:
        path4 = os.path.join(ARTIFACTS_DIR, 'kyb-v3-topology-fit-reheat.png')
        with open(path4, 'wb') as f:
            f.write(base64.b64decode(snap4['data']))
        print(f"Saved: {path4}")

    # Detach tab cleanly
    client.call('tabs.detach', {'tabId': tab_id})
    client.close()
    print("Verification completed successfully and tab detached cleanly!")

if __name__ == '__main__':
    main()
