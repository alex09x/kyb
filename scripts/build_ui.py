#!/usr/bin/env python3
"""Assemble the single-file web UI from src/web/.

The server embeds one HTML document; people edit many files. The template
src/web/index.html carries two markers, `<!-- @css -->` and `<!-- @js -->`,
each replaced by the concatenation of src/web/css/*.css or src/web/js/*.js in
name order. The numeric prefixes are the order. build.rs does the same thing
for the Rust binary; keep the two in step.
"""
import pathlib
import sys

WEB = pathlib.Path(__file__).resolve().parent.parent / "src" / "web"


def assemble(web: pathlib.Path = WEB) -> str:
    html = (web / "index.html").read_text(encoding="utf-8")
    for marker, sub, ext in (("<!-- @css -->\n", "css", ".css"), ("<!-- @js -->\n", "js", ".js")):
        parts = sorted(p for p in (web / sub).iterdir() if p.suffix == ext)
        if marker not in html:
            raise SystemExit(f"template lacks {marker.strip()}")
        html = html.replace(marker, "".join(p.read_text(encoding="utf-8") for p in parts), 1)
    return html


if __name__ == "__main__":
    out = assemble()
    if len(sys.argv) > 1 and sys.argv[1] != "-":
        pathlib.Path(sys.argv[1]).write_text(out, encoding="utf-8")
    else:
        sys.stdout.write(out)
