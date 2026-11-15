#!/usr/bin/env python3
"""Validate an EPUB produced by mobi2epub against the MOBI it came from.

Checks the ZIP container, XML well-formedness of every XML file, that every
manifest item exists, that all internal links and images resolve, and that the
book's words survive conversion unchanged.

Usage: check_epub.py BOOK.mobi BOOK.epub
"""
import html
import posixpath
import re
import sys
import xml.etree.ElementTree as ET
import zipfile

sys.path.insert(0, __import__("os").path.dirname(__file__))
from make_mobi import build_html, resolve_fileposes  # noqa: E402

NS = {"x": "http://www.w3.org/1999/xhtml", "opf": "http://www.idpf.org/2007/opf"}


def fail(msg):
    print("FAIL:", msg)
    sys.exit(1)


def main(mobi_path, epub_path):
    z = zipfile.ZipFile(epub_path)
    if z.testzip() is not None:
        fail("bad CRC")
    infos = z.infolist()
    if infos[0].filename != "mimetype" or infos[0].compress_type != zipfile.ZIP_STORED:
        fail("mimetype must be first and stored")
    if z.read("mimetype") != b"application/epub+zip":
        fail("bad mimetype content")

    docs = {}
    for name in z.namelist():
        if name.endswith((".xml", ".opf", ".ncx", ".xhtml")):
            try:
                docs[name] = ET.fromstring(z.read(name))
            except ET.ParseError as e:
                fail(f"{name} is not well-formed: {e}")

    opf = docs["OEBPS/content.opf"]
    manifest = {i.get("href") for i in opf.iterfind(".//opf:item", NS)}
    for href in manifest:
        if "OEBPS/" + href not in z.namelist():
            fail(f"manifest item missing: {href}")

    ids = {}
    for name, root in docs.items():
        ids[name] = {e.get("id") for e in root.iter() if e.get("id")}

    words = []
    for name in sorted(n for n in docs if re.search(r"part\d+\.xhtml$", n)):
        root = docs[name]
        base = posixpath.dirname(name)
        for el in root.iter():
            for attr in ("href", "src"):
                v = el.get(attr)
                if not v or v.endswith(".css") or v.startswith("http"):
                    continue
                path, _, frag = v.partition("#")
                target = posixpath.normpath(posixpath.join(base, path)) if path else name
                if target not in z.namelist():
                    fail(f"{name}: broken {attr} {v}")
                if frag and frag not in ids.get(target, ()):
                    fail(f"{name}: missing fragment {v}")
        body = root.find("x:body", NS)
        words += "".join(body.itertext()).split()

    for name in ("OEBPS/nav.xhtml", "OEBPS/toc.ncx"):
        root = docs[name]
        for el in root.iter():
            v = el.get("href") or el.get("src")
            if v:
                path, _, frag = v.partition("#")
                target = "OEBPS/" + path
                if target not in z.namelist() or (frag and frag not in ids[target]):
                    fail(f"{name}: broken link {v}")

    # Expected words from the source HTML.
    enc = "utf-8" if "utf8" in mobi_path else "cp1252"
    src, _ = resolve_fileposes(build_html("utf8" if "utf8" in mobi_path else "cp1252"), enc)
    src = src.decode(enc)
    src = src[src.index("<body>"):]
    src = re.sub(r"<!--.*?-->", "", src, flags=re.S)
    src = re.sub(r"<[/a-zA-Z][^>]*>", " ", src)
    expected = []
    for w in html.unescape(src).split():
        expected.append(w)
    # Rejoin to compare on whitespace-normalized text (tags may split words differently).
    got, exp = "".join(words), "".join(expected)
    if got != exp:
        i = next((k for k, (a, b) in enumerate(zip(got, exp)) if a != b), min(len(got), len(exp)))
        fail(f"text mismatch at char {i}: got {got[i-40:i+40]!r} expected {exp[i-40:i+40]!r}")

    meta = opf.find("opf:metadata", NS)
    print(f"OK {epub_path}: {len(words)} words, {len(docs)} XML files, "
          f"title={meta.find('{http://purl.org/dc/elements/1.1/}title').text!r}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
