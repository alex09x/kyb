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
    "404.html",
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


def resolve(link):
    """A root-relative link as served: "/" and "/blog/" are directories with an
    index.html, everything else is the file itself."""
    rel = link.lstrip("/")
    if link.endswith("/") or not rel:
        return site / rel / "index.html"
    return site / rel


html = (site / "index.html").read_text(encoding="utf-8")
if "kyb-memory.com" in html:
    raise SystemExit("obsolete kyb-memory.com domain remains in index.html")
if "https://kybmemory.com/" not in html:
    raise SystemExit("canonical kybmemory.com URL is missing")

# Every published page is checked, not only the landing page: a blog post with a
# dead link or a runtime script ships just as publicly as index.html does.
pages = sorted(site.rglob("*.html"))
if not pages:
    raise SystemExit("no HTML pages found")

ids = anchors = assets = 0
for page in pages:
    where = page.relative_to(site)
    page_html = page.read_text(encoding="utf-8")
    parser = SiteParser()
    parser.feed(page_html)

    missing_anchors = sorted(set(parser.anchors) - parser.ids)
    if missing_anchors:
        raise SystemExit(f"{where}: missing anchor targets: {', '.join(missing_anchors)}")
    missing_assets = sorted({a for a in parser.local_assets if not resolve(a).is_file()})
    if missing_assets:
        raise SystemExit(f"{where}: missing local assets: {', '.join(missing_assets)}")
    if parser.runtime_scripts:
        raise SystemExit(f"{where}: the static site must not contain runtime scripts")

    if page.name != "404.html":
        marker = '<script type="application/ld+json">'
        start = page_html.find(marker)
        end = page_html.find("</script>", start)
        if start < 0 or end < 0:
            raise SystemExit(f"{where}: JSON-LD metadata is missing")
        json.loads(page_html[start + len(marker):end])

    if page.name == "404.html":
        # The error page must never be indexed, and a self-canonical on a 404 is
        # meaningless - so it is the one page exempt from the canonical rule.
        if 'name="robots" content="noindex' not in page_html:
            raise SystemExit(f"{where}: the 404 page must be noindex")
    elif f'rel="canonical" href="https://kybmemory.com/' not in page_html:
        raise SystemExit(f"{where}: canonical URL must point at kybmemory.com")

    ids += len(parser.ids)
    anchors += len(parser.anchors)
    assets += len(set(parser.local_assets))

ElementTree.parse(site / "favicon.svg")
sitemap = ElementTree.parse(site / "sitemap.xml")
locs = [el.text or "" for el in sitemap.iter("{http://www.sitemaps.org/schemas/sitemap/0.9}loc")]
if any("404" in loc for loc in locs):
    raise SystemExit("the 404 page must not be listed in sitemap.xml")
for page in pages:
    if page.name == "404.html":
        continue
    served = "https://kybmemory.com/" + str(page.relative_to(site)).replace("index.html", "")
    if served not in locs:
        raise SystemExit(f"sitemap.xml does not list {served}")

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
    f"Static site checks passed: {len(pages)} pages, {ids} ids, "
    f"{anchors} anchor links, {assets} local assets"
)
PY
