#!/usr/bin/env python3
"""A hermetic Torznab indexer and external web fixture for the R7 corpus.

Serves ``GET /api`` for ``t=caps`` and ``t=search`` with a fixed item set and
remembers every request, so the corpus can compare what each server sent to
the indexer. ``GET /_requests`` returns and clears that log.

``/_fixture/*`` stands in for the external sites ``/msx/proxy`` reaches:
``echo`` answers any method with what it received, ``redirect`` sends a 302 to
``echo`` and ``missing`` answers 404. The log records their method, ``X-*``
headers and body as well.
"""

from __future__ import annotations

import http.server
import json
import sys
import threading
import urllib.parse
from xml.sax.saxutils import escape, quoteattr

import discovery_probe
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

    def read_body(self) -> tuple[str, bytes]:
        """The body and how it was framed: Go's client sends a proxied body of
        unknown length chunked."""
        if "chunked" in self.headers.get("Transfer-Encoding", "").lower():
            data = b""
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    while self.rfile.readline().strip():
                        pass
                    return "chunked", data
                data += self.rfile.read(size)
                self.rfile.readline()
        if "Content-Length" in self.headers:
            return "length", self.rfile.read(int(self.headers["Content-Length"]))
        return "none", b""

    def fixture(self, url: urllib.parse.SplitResult) -> None:
        framing, raw = self.read_body()
        body = raw.decode("utf-8", "replace")
        headers = {name.lower(): value for name, value in self.headers.items() if name.lower().startswith("x-")}
        if self.command == "NOTIFY":
            # A UPnP event: its subscription id is random, so only its presence
            # is kept.
            for name in ("content-type", "nt", "nts", "seq"):
                if name in self.headers:
                    headers[name] = self.headers[name]
            if "sid" in self.headers:
                headers["sid"] = "<set>"
        with LOCK:
            LOG.append({"method": self.command, "path": url.path, "query": url.query, "headers": headers,
                        "framing": framing, "body": body})
        if url.path == "/_fixture/echo":
            reply = json.dumps({"method": self.command, "query": url.query, "headers": headers, "body": body})
            data = reply.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("X-Fixture-Reply", "not forwarded")
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)
        elif url.path == "/_fixture/redirect":
            self.send_response(302)
            self.send_header("Location", "/_fixture/echo?redirected=1")
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self.reply(404, "missing fixture", "text/plain; charset=utf-8")

    def do_POST(self) -> None:
        self.fixture(urllib.parse.urlsplit(self.path))

    def do_PUT(self) -> None:
        self.fixture(urllib.parse.urlsplit(self.path))

    def do_NOTIFY(self) -> None:
        self.fixture(urllib.parse.urlsplit(self.path))

    def do_HEAD(self) -> None:
        self.fixture(urllib.parse.urlsplit(self.path))

    def discovery(self, url: urllib.parse.SplitResult) -> None:
        query = dict(urllib.parse.parse_qsl(url.query))
        source = query.get("source", "")
        clear = query.get("clear") == "1"
        if url.path == "/_mdns/query":
            result: object = discovery_probe.mdns_query(
                query["name"], query.get("type", "PTR"), source, float(query.get("wait", "1.5")))
        elif url.path == "/_mdns/recorded":
            result = discovery_probe.mdns_recorded(source, clear)
        elif url.path == "/_ssdp/notify":
            result = discovery_probe.ssdp_notifications(source, clear)
        elif url.path == "/_ssdp/search":
            result = discovery_probe.ssdp_search(query.get("st", "ssdp:all"), int(query.get("mx", "1")), source)
        else:
            self.reply(404, "missing probe", "text/plain; charset=utf-8")
            return
        self.reply(200, json.dumps(result, ensure_ascii=False), "application/json")

    def do_GET(self) -> None:
        url = urllib.parse.urlsplit(self.path)
        if url.path.startswith(("/_mdns/", "/_ssdp/")):
            self.discovery(url)
            return
        if url.path.startswith("/_fixture/"):
            self.fixture(url)
            return
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
    discovery_probe.start()
    http.server.ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
