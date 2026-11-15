#!/usr/bin/env python3
"""Generate synthetic MOBI 6 files for testing mobi2epub.

Covers: no / PalmDOC / HUFF-CDIC compression, cp1252 and UTF-8 text,
trailing record entries, EXTH metadata, images, a cover and filepos links.

Usage: make_mobi.py OUTDIR
"""
import heapq
import itertools
import os
import re
import struct
import sys
import zlib

RECORD_SIZE = 4096


def png_1x1():
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
    idat = zlib.compress(b"\x00\xff\x00\x00")
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


FAKE_JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00" + bytes(range(64)) + b"\xff\xd9"


# ---------- PalmDOC ----------
def palmdoc_compress(data):
    out = bytearray()
    i, n = 0, len(data)
    while i < n:
        best, bd = 0, 0
        lo = max(0, i - 2047)
        for length in range(min(10, n - i), 2, -1):
            p = data.rfind(data[i:i + length], lo, i)
            if p != -1 and p + length <= i:
                best, bd = length, i - p
                break
        if best >= 3:
            out += struct.pack(">H", 0x8000 | (bd << 3) | (best - 3))
            i += best
            continue
        c = data[i]
        if c == 0x20 and i + 1 < n and 0x40 <= data[i + 1] <= 0x7F:
            out.append(data[i + 1] ^ 0x80)
            i += 2
        elif c == 0 or 0x09 <= c <= 0x7F:
            out.append(c)
            i += 1
        else:
            j = i
            while j < n and j - i < 8 and not (data[j] == 0 or 0x09 <= data[j] <= 0x7F):
                j += 1
            out.append(j - i)
            out += data[i:j]
            i = j
    return bytes(out)


# ---------- HUFF/CDIC ----------
class HuffEncoder:
    def __init__(self, sample, phrases, recursive_phrase):
        self.phrases = sorted(set(phrases + [recursive_phrase]), key=len, reverse=True)
        self.recursive = recursive_phrase
        syms = [bytes([b]) for b in range(256)] + self.phrases
        freq = {s: 1 for s in syms}
        for tok in self.tokenize(sample, self.phrases):
            freq[tok] += 1
        # Huffman code lengths.
        cnt = itertools.count()
        heap = [(f, next(cnt), [s]) for s, f in freq.items()]
        heapq.heapify(heap)
        depth = {s: 0 for s in syms}
        while len(heap) > 1:
            f1, _, a = heapq.heappop(heap)
            f2, _, b = heapq.heappop(heap)
            for s in a + b:
                depth[s] += 1
            heapq.heappush(heap, (f1 + f2, next(cnt), a + b))
        assert max(depth.values()) <= 24
        order = sorted(syms, key=lambda s: (depth[s], s))
        # Canonical codes, then complemented (MOBI wants long codes numerically small).
        self.code = {}
        self.index = {}
        start, first, M, mincode = {}, {}, {}, {}
        code, prev_len = 0, order[0] and depth[order[0]]
        code = 0
        prev_len = depth[order[0]]
        for idx, s in enumerate(order):
            l = depth[s]
            if l != prev_len:
                code <<= (l - prev_len)
                prev_len = l
            if l not in first:
                first[l], start[l] = code, idx
            comp = ((1 << l) - 1) - code
            self.code[s] = (comp, l)
            self.index[s] = idx
            code += 1
        lengths = sorted(first)
        count = {l: sum(1 for s in syms if depth[s] == l) for l in lengths}
        for l in lengths:
            M[l] = start[l] + (1 << l) - 1 - first[l]
            mincode[l] = ((1 << l) - 1) - (first[l] + count[l] - 1)
        self.order = order
        # dict1
        dict1 = []
        for p in range(256):
            entry = None
            for s in order:
                c, l = self.code[s]
                if l <= 8 and (p >> (8 - l)) == c:
                    entry = l | 0x80 | (M[l] << 8)
                    break
            if entry is None:
                lens = [l for s in order for (c, l) in [self.code[s]] if l > 8 and (c >> (l - 8)) == p]
                entry = min(lens) if lens else 9
            dict1.append(entry)
        dict2 = []
        for l in range(1, 33):
            if l in M:
                dict2 += [mincode[l], M[l]]
            else:
                dict2 += [(1 << l) - 1 if l < 32 else 0xFFFFFFFF, 0]
        self.huff = b"HUFF" + struct.pack(">III", 0x18, 0x18, 0x18 + 1024) + bytes(8) + \
            struct.pack(">256I", *dict1) + struct.pack(">64I", *dict2)
        # CDIC: entries in dictionary order; the recursive phrase is stored compressed.
        n = len(order)
        bits = max(1, (n - 1).bit_length())
        table, body = [], bytearray()
        for s in order:
            if s == self.recursive:
                data = self.encode(s, [p for p in self.phrases if p != s])
                flag = 0
            else:
                data, flag = s, 0x8000
            table.append(n * 2 + len(body))
            body += struct.pack(">H", len(data) | flag) + data
        self.cdic = b"CDIC" + struct.pack(">III", 0x10, n, bits) + struct.pack(">%dH" % n, *table) + bytes(body)

    @staticmethod
    def tokenize(data, phrases):
        i = 0
        while i < len(data):
            for p in phrases:
                if data.startswith(p, i):
                    yield p
                    i += len(p)
                    break
            else:
                yield data[i:i + 1]
                i += 1

    def encode(self, data, phrases=None):
        bits = []
        for tok in self.tokenize(data, self.phrases if phrases is None else phrases):
            c, l = self.code[tok]
            bits.append(format(c, "0%db" % l))
        s = "".join(bits)
        s += "0" * (-len(s) % 8)
        return int(s, 2).to_bytes(len(s) // 8, "big") if s else b""


# ---------- book assembly ----------
def build_html(encoding):
    chapters = []
    toc_links = "".join(
        f'<p><a filepos={{{{F:ch{i}}}}}>Chapter {i}</a></p>' for i in range(1, 6))
    head = ('<html><head><title>ignored</title><guide><reference type="toc" title="TOC" '
            'filepos={{F:toc}} /></guide></head><body>')
    chapters.append('{{A:toc}}<h2>Table of Contents</h2>' + toc_links +
                    '<p><a href="#nowhere">bad link</a> <a filepos=99999999>past end</a></p>')
    words = ("lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor "
             "incididunt ut labore et dolore magna aliqua").split()
    for i in range(1, 6):
        paras = []
        for j in range(40):
            w = " ".join(words[(i * 7 + j + k) % len(words)] for k in range(12 + j % 9))
            paras.append(f'<p width="{"-19pt" if j % 2 else "0"}" height="{j % 3}em" align=justify>{w}.')
        extra = ''
        if i == 1:
            extra = ('<p>Entities: caf&eacute; &mdash; &#146;quoted&#148; &nbsp;x &bogus; AT&T 3 < 4 > 2</p>'
                     '<p><font size="+2" color="red" face="Georgia">Big red</font> <b><i>bold italic</b></i>'
                     '<center>Centered</center><ul><li>one<li>two<li>three</ul>'
                     '<table><tr><td>a<td colspan=2>b<tr><td>c</table>'
                     '<p>image: <img recindex="00002" align="baseline"> and a missing one '
                     '<img recindex="00099"></p>'
                     '<p>Back to <a filepos={{F:toc}}>contents</a>. Jump to <a filepos={{F:mid3}}>middle of 3</a></p>'
                     '<!-- a comment --><mbp:section><mbp:nu>x</mbp:nu></mbp:section>'
                     '<p id="dup">first</p><p id="dup">second</p><p id="9bad">numeric id</p>')
            extra += '<p>Accents: ' + ("Ünïcödé “smart” — ellipsis…" if encoding == "utf8"
                                       else "Français “smart” —") + '</p>'
        if i == 3:
            paras.insert(20, '{{A:mid3}}<p>MIDDLE OF CHAPTER THREE</p>')
        chapters.append(f'{{{{A:ch{i}}}}}<h1 align=center>Chapter {i}</h1>' + extra + "".join(paras))
    html = head + "<mbp:pagebreak/>".join(chapters) + "<mbp:pagebreak/></body></html>"
    return html


def resolve_fileposes(html, enc):
    names = {}

    def link(m):
        names.setdefault(m.group(1), len(names))
        return "%010d" % (9000000000 + names[m.group(1)])
    html = re.sub(r"\{\{F:(\w+)\}\}", link, html)
    data = html.encode(enc)
    positions = {}
    out = bytearray()
    pos = 0
    for m in re.finditer(rb"\{\{A:(\w+)\}\}", data):
        out += data[pos:m.start()]
        positions[m.group(1).decode()] = len(out)
        pos = m.end()
    out += data[pos:]
    out = bytes(out)
    for name, idx in names.items():
        out = out.replace(b"%010d" % (9000000000 + idx), b"%010d" % positions[name])
    return out, positions


def make_book(path, compression, encoding, with_images=True):
    enc = "utf-8" if encoding == "utf8" else "cp1252"
    html = build_html(encoding)
    text, _ = resolve_fileposes(html, enc)
    chunks = [text[i:i + RECORD_SIZE] for i in range(0, len(text), RECORD_SIZE)]
    huff = None
    if compression == 17480:
        huff = HuffEncoder(text, [b"<p>", b"</p>", b"lorem ", b"dolor ", b" et ", b"<p width=\""],
                           b"consectetur adipiscing")
    extra_flags = 3  # multibyte overlap byte + one TBS-style entry
    text_recs = []
    for ch in chunks:
        if compression == 1:
            body = ch
        elif compression == 2:
            body = palmdoc_compress(ch)
        else:
            body = huff.encode(ch)
        # multibyte byte (0 extra bytes), then a 3-byte trailing entry with backward size.
        body += b"\x00" + b"XY" + bytes([0x83])
        text_recs.append(body)

    images = [FAKE_JPEG, png_1x1(), FAKE_JPEG + b"cover"] if with_images else []
    first_image = 1 + len(text_recs) if images else 0xFFFFFFFF
    huff_off = 1 + len(text_recs) + len(images)
    huff_recs = [huff.huff, huff.cdic] if huff else []

    title = "Test Bóok" if encoding == "utf8" else "Test Book"
    exth_items = [
        (100, "Jane Author"), (100, "John Coauthor"), (101, "Test Press"),
        (103, "A <b>description</b> & more"), (105, "Testing"), (106, "2020-05-17T00:00:00+00:00"),
        (503, title), (524, "en-US"),
    ]
    exth = b""
    for t, v in exth_items:
        b = v.encode(enc)
        exth += struct.pack(">II", t, 8 + len(b)) + b
    if images:
        exth += struct.pack(">III", 201, 12, 2)  # cover = third image
    exth_count = len(exth_items) + (1 if images else 0)
    exth = b"EXTH" + struct.pack(">II", 12 + len(exth), exth_count) + exth
    exth += b"\x00" * (-len(exth) % 4)

    header_len = 0xE8
    name = b"Full Name From Header"
    rec0 = bytearray(16 + header_len)
    struct.pack_into(">HHIHHHH", rec0, 0, compression, 0, len(text), len(text_recs), RECORD_SIZE, 0, 0)
    rec0[16:20] = b"MOBI"
    struct.pack_into(">IIII", rec0, 20, header_len, 2, 65001 if encoding == "utf8" else 1252, 12345)
    struct.pack_into(">I", rec0, 36, 6)
    struct.pack_into(">I", rec0, 80, 1 + len(text_recs))
    name_off = 16 + header_len + len(exth)
    struct.pack_into(">II", rec0, 84, name_off, len(name))
    struct.pack_into(">I", rec0, 108, first_image)
    struct.pack_into(">II", rec0, 112, huff_off if huff else 0, len(huff_recs))
    struct.pack_into(">I", rec0, 128, 0x40)
    struct.pack_into(">H", rec0, 16 + 0xE2, extra_flags)
    rec0 = bytes(rec0) + exth + name + b"\x00\x00"

    records = [rec0] + text_recs + images + huff_recs + [b"FLIS\x00\x00\x00\x08", b"\xe9\x8e\r\n"]
    write_pdb(path, "Test_Book", records)
    return text


def write_pdb(path, name, records):
    hdr = bytearray(78)
    hdr[0:len(name)] = name.encode()
    hdr[60:68] = b"BOOKMOBI"
    struct.pack_into(">H", hdr, 76, len(records))
    off = 78 + 8 * len(records) + 2
    table = b""
    for i, r in enumerate(records):
        table += struct.pack(">II", off, i * 2)
        off += len(r)
    with open(path, "wb") as f:
        f.write(bytes(hdr) + table + b"\x00\x00" + b"".join(records))


if __name__ == "__main__":
    outdir = sys.argv[1]
    os.makedirs(outdir, exist_ok=True)
    for comp, cname in [(1, "none"), (2, "palmdoc"), (17480, "huff")]:
        for enc in ["cp1252", "utf8"]:
            make_book(os.path.join(outdir, f"{cname}-{enc}.mobi"), comp, enc)
    make_book(os.path.join(outdir, "noimages-utf8.mobi"), 2, "utf8", with_images=False)
    print("ok")
