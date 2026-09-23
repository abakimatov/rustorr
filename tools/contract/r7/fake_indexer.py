#!/usr/bin/env python3
"""A hermetic Torznab indexer for the R7 corpus.

Serves ``GET /api`` for ``t=caps`` and ``t=search`` with a fixed item set and
remembers every request, so the corpus can compare what each server sent to
the indexer. ``GET /_requests`` returns and clears that log.
"""

from __future__ import annotations

import http.server
import json
import sys
import threading
import urllib.parse
from xml.sax.saxutils import escape, quoteattr

from fixtures import TORZNAB_ITEMS

API_KEY = "fixture-key"
LOG: list[dict[str, str]] = []
LOCK = threading.Lock()


def caps() -> str:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<caps><server title="FakeIndexer"/><searching><search available="yes" supportedParams="q"/></searching>'
        '<categories><category id="2000" name="Movies"/><category id="5000" name="TV"/></categories></caps>'
    )


def error(code: int, description: str) -> str:
    return f'<?xml version="1.0" encoding="UTF-8"?><error code="{code}" description={quoteattr(description)}/>'


def search(query: str) -> str:
    words = query.lower().split()
    items = []
    for item in TORZNAB_ITEMS:
        if words and not all(word in item["title"].lower() for word in words):
            continue
        attrs = [f'<torznab:attr name="seeders" value="{item["seeders"]}"/>',
                 f'<torznab:attr name="peers" value="{item["peers"]}"/>']
        if item["magnet"]:
            attrs.append(f'<torznab:attr name="magneturl" value={quoteattr(item["magnet"])}/>')
        items.append(
            "<item>"
            f"<title>{escape(item['title'])}</title>"
            f"<link>{escape(item['link'])}</link>"
            f"<pubDate>{item['pubDate']}</pubDate>"
            f"<size>{item['size']}</size>"
            f"<jackettindexer>{item['indexer']}</jackettindexer>"
            f'<enclosure url={quoteattr(item["link"])} length="{item["size"]}" type="application/x-bittorrent"/>'
            + "".join(attrs)
            + "</item>"
        )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>'
        + "".join(items)
        + "</channel></rss>"
    )


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        return

    def reply(self, status: int, body: str, content_type: str) -> None:
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        url = urllib.parse.urlsplit(self.path)
        if url.path == "/_requests":
            with LOCK:
                body = json.dumps(LOG)
                LOG.clear()
            self.reply(200, body, "application/json")
            return
        with LOCK:
            LOG.append({"path": url.path, "query": url.query})
        query = dict(urllib.parse.parse_qsl(url.query))
        if url.path.rstrip("/") != "/api":
            self.reply(404, "not found", "text/plain")
        elif query.get("apikey") != API_KEY:
            self.reply(200, error(100, "Invalid API Key"), "application/xml")
        elif query.get("t") == "caps":
            self.reply(200, caps(), "application/xml")
        elif query.get("t") == "search":
            self.reply(200, search(query.get("q", "")), "application/rss+xml")
        else:
            self.reply(200, error(202, "No such function"), "application/xml")


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9117
    http.server.ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
