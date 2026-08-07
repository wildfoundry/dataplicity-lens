#!/usr/bin/env python3
from __future__ import annotations

import html.parser
import sys
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else "site").resolve()
HTML_FILES = sorted(ROOT.glob("*.html"))
REQUIRED = {
    "index.html", "lens-suite.html", "lens.html", "lens-top.html", "lens-services.html",
    "lens-logs.html", "lens-disk.html", "lens-net.html", "lens-hardware.html", "lens-system.html", "lens-health.html",
    "architecture.html", "404.html",
}
REQUIRED_SEARCH_FILES = {
    "robots.txt",
    "sitemap.xml",
    "llms.txt",
}
SITE_ORIGIN = "https://lens.dataplicity.com"
SITEMAP_EXCLUDE = {"404.html"}

class Links(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.links: list[tuple[str, str]] = []
        self.ids: set[str] = set()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(values["id"] or "")
        for key in ("href", "src"):
            if values.get(key):
                self.links.append((key, values[key] or ""))

errors: list[str] = []
missing_pages = REQUIRED - {p.name for p in HTML_FILES}
if missing_pages:
    errors.append(f"missing required pages: {', '.join(sorted(missing_pages))}")

missing_search = REQUIRED_SEARCH_FILES - {p.name for p in ROOT.iterdir() if p.is_file()}
if missing_search:
    errors.append(f"missing search engine files: {', '.join(sorted(missing_search))}")

robots_path = ROOT / "robots.txt"
if robots_path.exists():
    robots_text = robots_path.read_text(encoding="utf-8")
    if "Sitemap: https://lens.dataplicity.com/sitemap.xml" not in robots_text:
        errors.append("robots.txt: missing Sitemap line for https://lens.dataplicity.com/sitemap.xml")

sitemap_path = ROOT / "sitemap.xml"
if sitemap_path.exists():
    sitemap_text = sitemap_path.read_text(encoding="utf-8")
    expected_locs: set[str] = set()
    for page in HTML_FILES:
        if page.name in SITEMAP_EXCLUDE:
            continue
        if page.name == "index.html":
            expected_locs.add(f"{SITE_ORIGIN}/")
        else:
            expected_locs.add(f"{SITE_ORIGIN}/{page.name}")
    for loc in sorted(expected_locs):
        if f"<loc>{loc}</loc>" not in sitemap_text:
            errors.append(f"sitemap.xml: missing {loc}")
    if "<loc>https://lens.dataplicity.com/404.html</loc>" in sitemap_text:
        errors.append("sitemap.xml: should not include 404.html")
    if "<loc>https://lens.dataplicity.com/index.html</loc>" in sitemap_text:
        errors.append("sitemap.xml: use / for the home page, not index.html")

llms_path = ROOT / "llms.txt"
if llms_path.exists():
    llms_text = llms_path.read_text(encoding="utf-8")
    if "# Dataplicity Lens" not in llms_text:
        errors.append("llms.txt: missing Dataplicity Lens heading")
    if "https://lens.dataplicity.com/" not in llms_text:
        errors.append("llms.txt: missing site home URL")

parsed: dict[Path, Links] = {}
for page in HTML_FILES:
    parser = Links()
    parser.feed(page.read_text(encoding="utf-8"))
    parsed[page] = parser
    if not page.read_text(encoding="utf-8").lower().startswith("<!doctype html>"):
        errors.append(f"{page.name}: missing doctype")

for page, parser in parsed.items():
    for kind, target in parser.links:
        if target.startswith(("http://", "https://", "mailto:", "tel:", "data:")):
            continue
        if target.startswith("#"):
            if target[1:] and target[1:] not in parser.ids:
                errors.append(f"{page.name}: missing fragment {target}")
            continue
        parsed_url = urlparse(target)
        local_path = (page.parent / parsed_url.path).resolve()
        try:
            local_path.relative_to(ROOT)
        except ValueError:
            errors.append(f"{page.name}: link escapes site root: {target}")
            continue
        if not local_path.exists():
            errors.append(f"{page.name}: missing {kind} target: {target}")
            continue
        if parsed_url.fragment and local_path.suffix == ".html":
            target_parser = parsed.get(local_path)
            if target_parser and parsed_url.fragment not in target_parser.ids:
                errors.append(f"{page.name}: missing fragment {target}")

if errors:
    print("GitHub Pages validation failed:", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)
print(f"Validated {len(HTML_FILES)} HTML pages and all local links in {ROOT}")
