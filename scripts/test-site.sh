#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
site_dir="$repo_root/docs"

python3 - "$site_dir" <<'PY'
import json
import struct
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit
from xml.etree import ElementTree

site = Path(sys.argv[1])
required = {
    "index.html",
    "favicon.svg",
    "og.png",
    "robots.txt",
    "sitemap.xml",
    "_headers",
}
missing = sorted(name for name in required if not (site / name).is_file())
if missing:
    raise SystemExit(f"missing site assets: {', '.join(missing)}")


class SiteParser(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.anchors = []
        self.local_assets = []
        self.runtime_scripts = 0

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if "id" in values:
            if values["id"] in self.ids:
                raise SystemExit(f"duplicate id: {values['id']}")
            self.ids.add(values["id"])
        if tag == "a" and values.get("href", "").startswith("#"):
            self.anchors.append(values["href"][1:])
        for attr in ("href", "src"):
            value = values.get(attr, "")
            if value.startswith("/"):
                self.local_assets.append(urlsplit(value).path)
        if tag == "script" and values.get("type") != "application/ld+json":
            self.runtime_scripts += 1


html = (site / "index.html").read_text(encoding="utf-8")
if "kyb-memory.com" in html:
    raise SystemExit("obsolete kyb-memory.com domain remains in index.html")
if "https://kybmemory.com/" not in html:
    raise SystemExit("canonical kybmemory.com URL is missing")

parser = SiteParser()
parser.feed(html)
missing_anchors = sorted(set(parser.anchors) - parser.ids)
if missing_anchors:
    raise SystemExit(f"missing anchor targets: {', '.join(missing_anchors)}")
missing_assets = sorted({asset for asset in parser.local_assets if not (site / asset.lstrip("/")).is_file()})
if missing_assets:
    raise SystemExit(f"missing local assets: {', '.join(missing_assets)}")
if parser.runtime_scripts:
    raise SystemExit("the static site must not contain runtime scripts")

marker = '<script type="application/ld+json">'
start = html.find(marker)
end = html.find("</script>", start)
if start < 0 or end < 0:
    raise SystemExit("JSON-LD metadata is missing")
json.loads(html[start + len(marker):end])

ElementTree.parse(site / "favicon.svg")
ElementTree.parse(site / "sitemap.xml")

with (site / "og.png").open("rb") as image:
    header = image.read(24)
if header[:8] != b"\x89PNG\r\n\x1a\n":
    raise SystemExit("og.png is not a PNG")
width, height = struct.unpack(">II", header[16:24])
if (width, height) != (1200, 630):
    raise SystemExit(f"og.png must be 1200x630, got {width}x{height}")

headers = (site / "_headers").read_text(encoding="utf-8")
for expected in ("Content-Security-Policy:", "Strict-Transport-Security:", "X-Content-Type-Options:"):
    if expected not in headers:
        raise SystemExit(f"missing security header: {expected}")

if (site / "_redirects").exists():
    raise SystemExit("www-to-apex redirects belong in Cloudflare Redirect Rules, not Pages _redirects")

print(
    f"Static site checks passed: {len(parser.ids)} ids, "
    f"{len(parser.anchors)} anchor links, {len(set(parser.local_assets))} local assets"
)
PY
