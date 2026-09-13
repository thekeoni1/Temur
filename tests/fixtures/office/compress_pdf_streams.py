#!/usr/bin/env python3
"""Flate-compress the content streams of a PDF that temur's write path wrote.

Why this exists (T62 Ruling T62-4): temur's PDF writer emits uncompressed
content streams, so a PDF it wrote carries its own body text as plain bytes.
The grep tool skips a file only when a NUL byte appears in its first 4,096
bytes (src/tools/grep.rs:113), and such a PDF has none, so grep searches it as
text and finds the prose inside the content stream. Real PDFs, and the
sample-resume.pdf fixture, are flate-compressed and do carry early NULs, which
is why grep skips them. Task 13 measures what a model does when grep skips a
document, so its fixture has to be the compressed shape; otherwise the task
measures a route that does not exist on real input.

This rewrites bytes only. The page text is untouched, and the read tool
inflates FlateDecode streams, so the extracted text is byte-identical before
and after (tests/tools.rs a_pdf_with_a_flate_stream_reads_as_text and
an_uncompressed_pdf_reads_the_same_way assert that parity in general; the
eval's own preflight asserts it for this fixture).

The input shape is temur's writer output: objects 1..N laid out in order, each
"N 0 obj", with a cross-reference stream as the last object (/Type/XRef,
/W[1 4 2], all entries type 1 generation 0). Compressing a stream moves every
later object, so the xref stream is rebuilt from the new offsets.

Deterministic: zlib at a fixed level, objects re-emitted in order. The eval
asserts the resulting sha256 and hands the same bytes to every arm, so a zlib
version difference stops the run instead of quietly changing the document.

Assumes the writer's layout: objects are located by searching for "N 0 obj",
which would mis-parse if that string ever appeared inside a content stream.
It does not for the pinned ferry-review.md, and the eval's read-back parity
STOP would catch it if a future source made it so.

Usage: compress_pdf_streams.py <in.pdf> <out.pdf>
"""

import re
import sys
import zlib

ZLIB_LEVEL = 9


def parse_xref(data):
    """Return (xref_obj_num, xref_obj_start, first_obj_num, count, width)."""
    m = re.search(
        rb"(\d+) 0 obj\s*<<([^>]*?/Type\s*/XRef[^>]*?)>>\s*stream\r?\n", data, re.S
    )
    if not m:
        raise SystemExit("not a temur-written PDF: no cross-reference stream found")
    num = int(m.group(1))
    d = m.group(2)
    w = re.search(rb"/W\s*\[\s*(\d+)\s+(\d+)\s+(\d+)\s*\]", d)
    idx = re.search(rb"/Index\s*\[\s*(\d+)\s+(\d+)\s*\]", d)
    root = re.search(rb"/Root\s+(\d+) 0 R", d)
    if not (w and idx and root):
        raise SystemExit("cross-reference stream lacks /W, /Index or /Root")
    width = tuple(int(g) for g in w.groups())
    if width != (1, 4, 2):
        raise SystemExit(f"unexpected /W {width}; this script handles [1 4 2]")
    return num, m.start(), int(idx.group(1)), int(idx.group(2)), int(root.group(1))


def object_spans(data, first, count, xref_num, xref_start):
    """Byte span of every object, keyed by number, read from the xref stream."""
    offsets = {}
    for n in range(first, first + count):
        m = re.search(rb"(?<![0-9])%d 0 obj" % n, data)
        if not m:
            raise SystemExit(f"object {n} not found")
        offsets[n] = m.start()
    order = sorted(offsets)
    spans = {}
    for i, n in enumerate(order):
        end = offsets[order[i + 1]] if i + 1 < len(order) else len(data)
        body = data[offsets[n] : end]
        cut = body.rfind(b"endobj")
        if cut == -1:
            raise SystemExit(f"object {n} has no endobj")
        spans[n] = body[: cut + len(b"endobj")]
    return order, spans


def compress_object(body):
    """Compress a plain content stream. Returns the new object body, or None."""
    m = re.match(
        rb"(\d+) 0 obj\s*<<\s*/Length\s+(\d+)\s*>>\s*stream\r?\n", body, re.S
    )
    if not m:
        return None
    num, length = int(m.group(1)), int(m.group(2))
    start = m.end()
    payload = body[start : start + length]
    if len(payload) != length:
        raise SystemExit(f"object {num}: /Length {length} runs past the object")
    packed = zlib.compress(payload, ZLIB_LEVEL)
    return (
        b"%d 0 obj\n<</Length %d/Filter/FlateDecode>>stream\n" % (num, len(packed))
        + packed
        + b"\nendstream\nendobj"
    )


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    src, dst = sys.argv[1], sys.argv[2]
    data = open(src, "rb").read()

    xref_num, xref_start, first, count, root = parse_xref(data)
    order, spans = object_spans(data, first, count, xref_num, xref_start)
    header = data[: data.find(b"%d 0 obj" % order[0])]

    out = bytearray(header)
    offsets = {}
    compressed = 0
    for n in order:
        if n == xref_num:
            continue
        offsets[n] = len(out)
        packed = compress_object(spans[n])
        if packed is not None:
            compressed += 1
            out += packed
        else:
            out += spans[n]
        out += b"\n"

    # The xref stream records its own offset, so it is placed last.
    offsets[xref_num] = len(out)
    table = bytearray()
    for n in range(first, first + count):
        table += bytes([1]) + offsets[n].to_bytes(4, "big") + (0).to_bytes(2, "big")
    xref_off = offsets[xref_num]
    out += b"%d 0 obj\n<</Root %d 0 R/Type/XRef/Size %d/W[1 4 2]/Index[%d %d]/Length %d>>stream\n" % (
        xref_num, root, first + count, first, count, len(table),
    )
    out += table
    out += b"\nendstream\nendobj\n\nstartxref\n%d\n%%%%EOF\n" % xref_off

    open(dst, "wb").write(bytes(out))
    if compressed == 0:
        raise SystemExit("no content stream was compressed; fixture unchanged")
    print(f"compressed {compressed} content streams: {len(data)} -> {len(out)} bytes")


if __name__ == "__main__":
    main()
