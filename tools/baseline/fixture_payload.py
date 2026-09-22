"""Shared deterministic bytes for R1 fixture generation and verification."""

from __future__ import annotations

import hashlib
import struct

SEED = b"rustorr-r1-fixture-v1"
GENERATOR_CHUNK = 1024 * 1024


def payload(size: int, offset: int = 0) -> bytes:
    """Return one generator chunk using the frozen R1 v1 algorithm."""
    output = bytearray()
    counter = offset // len(SEED)
    while len(output) < size:
        output.extend(hashlib.sha256(SEED + struct.pack(">Q", counter)).digest())
        counter += 1
    return bytes(output[:size])


def payload_range(offset: int, size: int) -> bytes:
    """Reconstruct arbitrary bytes from a file emitted in 1 MiB chunks."""
    if offset < 0 or size < 0:
        raise ValueError("offset and size must be non-negative")
    output = bytearray()
    position = offset
    remaining = size
    while remaining:
        chunk_start = position // GENERATOR_CHUNK * GENERATOR_CHUNK
        within = position - chunk_start
        take = min(remaining, GENERATOR_CHUNK - within)
        generated = payload(within + take, chunk_start)
        output.extend(generated[within : within + take])
        position += take
        remaining -= take
    return bytes(output)
