#!/usr/bin/env python3
"""Validate local files and HTML anchors in a generated documentation site."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


@dataclass
class Page:
    anchors: set[str] = field(default_factory=set)
    references: list[str] = field(default_factory=list)


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.page = Page()

    def handle_starttag(
        self, tag: str, attributes: list[tuple[str, str | None]]
    ) -> None:
        for name, value in attributes:
            if value is None:
                continue
            if name == "id" or (tag == "a" and name == "name"):
                self.page.anchors.add(value)
            if name in {"href", "src"}:
                self.page.references.append(value)

    def handle_startendtag(
        self, tag: str, attributes: list[tuple[str, str | None]]
    ) -> None:
        self.handle_starttag(tag, attributes)


def parse_page(path: Path) -> Page:
    parser = PageParser()
    parser.feed(path.read_text(encoding="utf-8"))
    return parser.page


def normalized_base_path(value: str) -> str:
    return f"/{value.strip('/')}/"


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: check-doc-links.py SITE_DIRECTORY SITE_BASE_PATH", file=sys.stderr)
        return 2

    site_root = Path(sys.argv[1]).resolve()
    base_path = normalized_base_path(sys.argv[2])
    pages = {path.resolve(): parse_page(path) for path in site_root.rglob("*.html")}
    failures: list[str] = []
    checked_pages = 0

    for source, page in pages.items():
        source_label = source.relative_to(site_root)
        # Rustdoc and Javadoc validate authored documentation during their own
        # builds. Their generated pages may contain optional dependency links
        # with no local target when dependencies are intentionally excluded.
        if source_label.parts[0] == "api" and source_label != Path("api/index.html"):
            continue
        checked_pages += 1
        for reference in page.references:
            url = urlsplit(reference)
            if url.scheme or url.netloc:
                continue

            raw_path = unquote(url.path)
            if not raw_path:
                target = source
            elif raw_path.startswith("/"):
                if not raw_path.startswith(base_path):
                    failures.append(
                        f"{source_label}: {reference!r} escapes Pages base path {base_path!r}"
                    )
                    continue
                target = site_root / raw_path.removeprefix(base_path)
            else:
                target = source.parent / raw_path

            target = target.resolve()
            if site_root not in target.parents and target != site_root:
                failures.append(f"{source_label}: {reference!r} escapes the site tree")
                continue
            if target.is_dir():
                target = target / "index.html"
            if not target.is_file():
                failures.append(f"{source_label}: {reference!r} has no local target")
                continue

            fragment = url.fragment
            if fragment and target.suffix == ".html":
                target_page = pages.get(target)
                anchors = target_page.anchors if target_page is not None else set()
                candidates = {fragment, unquote(fragment)}
                source_range = re.fullmatch(r"([0-9]+)-([0-9]+)", fragment)
                range_exists = source_range is not None and all(
                    line in anchors for line in source_range.groups()
                )
                if candidates.isdisjoint(anchors) and not range_exists:
                    failures.append(
                        f"{source_label}: {reference!r} has no matching HTML anchor"
                    )

    if failures:
        print("Broken generated documentation links:", file=sys.stderr)
        for failure in sorted(failures):
            print(f"  {failure}", file=sys.stderr)
        return 1

    print(
        f"Checked {checked_pages} hand-authored HTML pages: "
        "all local links and anchors resolve."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
