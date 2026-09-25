"""mDNS (Bonjour) and SSDP (DLNA) observation for the R7 corpus.

The fixture service imports this module: background listeners record every
mDNS response and SSDP NOTIFY on the fixture network, and query helpers send
an mDNS question or an SSDP M-SEARCH. Results keep only packets from the
target under test and replace its address with ``<target>``, so the reference
and candidate corpora compare as JSON.
"""

from __future__ import annotations

import os
import socket
import struct
import threading
import time
from typing import Any

MDNS_GROUP = ("224.0.0.251", 5353)
SSDP_GROUP = ("239.255.255.250", 1900)
TYPES = {1: "A", 12: "PTR", 16: "TXT", 28: "AAAA", 33: "SRV", 47: "NSEC"}

_lock = threading.Lock()
_mdns: list[tuple[float, str, bytes]] = []
_notify: list[tuple[float, str, bytes]] = []
_mdns_socket: socket.socket | None = None


def _local_address() -> str:
    """The fixture networks have no default route, so multicast needs the
    interface named explicitly: ``DISCOVERY_ADDRESS`` when the service sits
    on several networks."""
    return os.environ.get("DISCOVERY_ADDRESS") or socket.gethostbyname(socket.gethostname())


def _multicast_socket(group: tuple[str, int]) -> socket.socket:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    sock.bind(("", group[1]))
    local = socket.inet_aton(_local_address())
    membership = struct.pack("4s4s", socket.inet_aton(group[0]), local)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, membership)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, local)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    return sock


def _listen(sock: socket.socket, sink: list[tuple[float, str, bytes]], keep) -> None:
    while True:
        data, (address, _port) = sock.recvfrom(65536)
        if keep(data):
            with _lock:
                sink.append((time.monotonic(), address, data))
                del sink[:-2000]


def start() -> None:
    """Starts both listeners; a network without multicast leaves them idle."""
    global _mdns_socket
    _mdns_socket = _multicast_socket(MDNS_GROUP)
    # Responses only: the QR bit of the DNS header.
    threading.Thread(target=_listen, args=(_mdns_socket, _mdns, lambda data: len(data) > 2 and data[2] & 0x80),
                     daemon=True).start()
    ssdp = _multicast_socket(SSDP_GROUP)
    threading.Thread(target=_listen, args=(ssdp, _notify, lambda data: data.startswith(b"NOTIFY ")),
                     daemon=True).start()


FIXTURE_SUBNETS = ("172.31.250.", "172.31.252.")


def _resolve(host: str) -> set[str]:
    """Every address of the target. Each target has the same host number on
    the fixture bridge and on the discovery network, and announces both."""
    addresses = {info[4][0] for info in socket.getaddrinfo(host, None, socket.AF_INET)}
    for address in list(addresses):
        for subnet in FIXTURE_SUBNETS:
            if address.startswith(FIXTURE_SUBNETS):
                addresses.add(subnet + address.rsplit(".", 1)[1])
    return addresses


# DNS messages -------------------------------------------------------------

def _name(data: bytes, offset: int) -> tuple[str, int]:
    labels: list[str] = []
    end = None
    for _ in range(128):
        length = data[offset]
        if length & 0xC0 == 0xC0:
            if end is None:
                end = offset + 2
            offset = ((length & 0x3F) << 8) | data[offset + 1]
            continue
        offset += 1
        if length == 0:
            break
        labels.append(data[offset:offset + length].decode("utf-8", "replace"))
        offset += length
    return ".".join(labels) + ".", end if end is not None else offset


def _records(data: bytes, source: set[str]) -> list[dict[str, Any]]:
    _id, _flags, questions, answers, authority, additional = struct.unpack("!6H", data[:12])
    offset = 12
    for _ in range(questions):
        _, offset = _name(data, offset)
        offset += 4
    out = []
    sections = ["answer"] * answers + ["authority"] * authority + ["additional"] * additional
    for section in sections:
        name, offset = _name(data, offset)
        kind, klass, ttl, length = struct.unpack("!HHIH", data[offset:offset + 10])
        offset += 10
        rdata = data[offset:offset + length]
        start = offset
        offset += length
        type_name = TYPES.get(kind, str(kind))
        if type_name == "A":
            value: Any = socket.inet_ntoa(rdata)
            value = "<target>" if value in source else value
        elif type_name == "AAAA":
            value = socket.inet_ntop(socket.AF_INET6, rdata)
        elif type_name == "PTR":
            value = _name(data, start)[0]
        elif type_name == "SRV":
            priority, weight, port = struct.unpack("!HHH", rdata[:6])
            value = {"priority": priority, "weight": weight, "port": port, "target": _name(data, start + 6)[0]}
        elif type_name == "TXT":
            strings, index = [], 0
            while index < len(rdata):
                size = rdata[index]
                strings.append(rdata[index + 1:index + 1 + size].decode("utf-8", "replace"))
                index += 1 + size
            value = strings
        else:
            value = rdata.hex()
        out.append({"section": section, "name": name, "type": type_name, "flush": bool(klass & 0x8000),
                    "class": klass & 0x7FFF, "ttl": ttl, "data": value})
    return out


def _question(name: str, kind: int) -> bytes:
    labels = b"".join(bytes([len(part)]) + part.encode() for part in name.rstrip(".").split("."))
    return struct.pack("!6H", 0, 0, 1, 0, 0, 0) + labels + b"\x00" + struct.pack("!HH", kind, 1)


def _unique(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    seen, out = set(), []
    for record in records:
        key = repr(sorted(record.items()))
        if key not in seen:
            seen.add(key)
            out.append(record)
    return sorted(out, key=lambda record: (record["section"], record["type"], record["name"], repr(record["data"])))


def mdns_query(name: str, kind: str, source_host: str, wait: float) -> dict[str, Any]:
    """Asks the group for ``name`` and returns every record the target
    answered with, in its packets received while waiting."""
    source = _resolve(source_host)
    started = time.monotonic()
    assert _mdns_socket is not None
    code = {value: key for key, value in TYPES.items()}[kind]
    _mdns_socket.sendto(_question(name, code), MDNS_GROUP)
    time.sleep(wait)
    with _lock:
        packets = [data for at, address, data in _mdns if at >= started and address in source]
    return {"packets": len(packets) > 0, "records": _unique([r for data in packets for r in _records(data, source)])}


def mdns_recorded(source_host: str, clear: bool) -> dict[str, Any]:
    """Everything the target announced since the last clear."""
    source = _resolve(source_host)
    with _lock:
        packets = [data for _, address, data in _mdns if address in source]
        if clear:
            _mdns[:] = [entry for entry in _mdns if entry[1] not in source]
    return {"packets": len(packets) > 0, "records": _unique([r for data in packets for r in _records(data, source)])}


# SSDP ---------------------------------------------------------------------

def _message(data: bytes, source: set[str]) -> dict[str, Any]:
    text = data.decode("utf-8", "replace")
    for address in source:
        text = text.replace(address, "<target>")
    head, _, _ = text.partition("\r\n\r\n")
    lines = head.split("\r\n")
    return {"start": lines[0], "headers": [line for line in lines[1:]]}


def ssdp_notifications(source_host: str, clear: bool) -> list[dict[str, Any]]:
    source = _resolve(source_host)
    with _lock:
        packets = [data for _, address, data in _notify if address in source]
        if clear:
            _notify[:] = [entry for entry in _notify if entry[1] not in source]
    messages = [_message(data, source) for data in packets]
    unique = {repr(message): message for message in messages}
    return sorted(unique.values(), key=repr)


def ssdp_search(target: str, mx: int, source_host: str) -> list[dict[str, Any]]:
    source = _resolve(source_host)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(_local_address()))
    sock.bind(("", 0))
    request = (
        "M-SEARCH * HTTP/1.1\r\n"
        f"HOST: {SSDP_GROUP[0]}:{SSDP_GROUP[1]}\r\n"
        'MAN: "ssdp:discover"\r\n'
        f"MX: {mx}\r\n"
        f"ST: {target}\r\n\r\n"
    ).encode()
    sock.sendto(request, SSDP_GROUP)
    deadline = time.monotonic() + mx + 0.5
    messages = []
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        sock.settimeout(remaining)
        try:
            data, (address, _port) = sock.recvfrom(65536)
        except socket.timeout:
            break
        if address in source:
            messages.append(_message(data, source))
    sock.close()
    return sorted(messages, key=repr)
