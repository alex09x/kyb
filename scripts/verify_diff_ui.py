#!/usr/bin/env python3
import os
import sys
import json
import socket
import time
import base64

SOCK_PATH = "/tmp/prod-browser-" + os.environ.get("USER", "alex09x") + ".sock"
ARTIFACTS_DIR = "/Users/alex09x/.gemini/antigravity-cli/brain/89170df8-ae1b-4af8-81a9-2107b1a69605"

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(SOCK_PATH)
f = sock.makefile("r")

_id = 0
def call(method, params=None):
    global _id
    _id += 1
    req = {"id": _id, "method": method, "params": params or {}}
    sock.sendall(json.dumps(req).encode("utf-8") + b"\n")
    line = f.readline()
    data = json.loads(line)
    if "error" in data and data["error"]:
        raise RuntimeError(f"RPC Error [{method}]: {data['error']}")
    return data.get("result")

# 1. Test Unified Diff View
tabs = call("tabs.list")
tab_id = None
for t in tabs:
    if "9311" in t.get("url", ""):
        tab_id = t["id"]
        break

if not tab_id:
    res = call("tabs.create", {"url": "http://127.0.0.1:9311/#/knowledge/task-e2e-test-1790717572", "active": True})
    tab_id = res["tabId"]
    time.sleep(3)

inspect = call("inspect", {"tabId": tab_id})
elements = inspect.get("elements", [])

unified_btn = None
for el in elements:
    if el.get("role") == "button" and el.get("name", "").strip() == "Unified":
        unified_btn = el
        break

if unified_btn:
    print("Clicking Unified button:", unified_btn["name"])
    call("click", {"tabId": tab_id, "elementId": unified_btn["id"]})
    time.sleep(1.5)

res = call("screenshot", {"tabId": tab_id})
if res and "data" in res:
    out = os.path.join(ARTIFACTS_DIR, "kyb-diff-unified-live.png")
    with open(out, "wb") as f_out:
        f_out.write(base64.b64decode(res["data"]))
    print("Saved Unified diff screenshot to:", out)

# 2. Test Activity Feed Diff Button
feed_btn = None
for el in elements:
    # Find navigation or feed
    if el.get("name", "").strip().lower() in ["feed", "activity"]:
        feed_btn = el
        break

sock.close()
