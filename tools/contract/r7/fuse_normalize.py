#!/usr/bin/env python3
"""Normalizes fuse_probe.sh output for comparison: torrent addition times
(epoch seconds and ls dates) and generated inode numbers differ between
captures; everything else is compared exactly."""

import re
import sys

text = sys.stdin.read()
text = re.sub(r"\b(1[0-9]{9}|2[0-9]{9})\b", lambda m: "<mtime>" if int(m.group(1)) >= 946684800 else m.group(1), text)
text = re.sub(r"\b[A-Z][a-z]{2} [ 0-9][0-9] [0-9]{2}:[0-9]{2}\b", "<date>", text)
text = re.sub(r"\|([0-9]+)$", lambda m: "|" + ("0" if m.group(1) == "0" else "<ino>"), text, flags=re.M)
sys.stdout.write(text)
