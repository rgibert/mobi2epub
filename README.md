# mobi2epub

A small command-line tool that converts MOBI ebooks to EPUB, shipped as a
single static binary written in Rust.

## Usage

```sh
mobi2epub book.mobi                  # writes book.epub next to it
mobi2epub -o out.epub book.mobi      # choose the output path
mobi2epub *.mobi                     # convert many at once
mobi2epub -f book.mobi               # overwrite an existing .epub
```

## What it handles

- MOBI 6 files, including the MOBI 6 part of hybrid MOBI/KF8 files
- PalmDOC (`.prc`/`.pdb`) plain-text books
- Uncompressed, PalmDOC (LZ77) and HUFF/CDIC compressed text
- Windows-1252 and UTF-8 text
- Metadata: title, authors, publisher, description, subjects, date, language, ISBN
- Cover image and inline images (JPEG, PNG, GIF)
- Internal `filepos` links (e.g. the book's own table of contents)
- Chapter splitting on page breaks, and a table of contents built from headings

The output is EPUB 3 with an EPUB 2 NCX for older readers. MOBI's legacy
HTML (unclosed tags, `<font>`, `<center>`, `width`/`height` paragraph
attributes and so on) is cleaned up into well-formed XHTML.

## Not supported

- DRM-protected books
- KF8-only books (`.azw3`, or MOBI files with no MOBI 6 section)

## Crash reporting

Release builds report crashes and unreadable books to
[Sentry](https://sentry.io) so problems can be fixed. A report contains the
error or stack trace, the tool version, OS and architecture, and for
unreadable books the file extension and size. File names, paths and book
contents are never sent.

To turn reporting off, set `MOBI2EPUB_NO_TELEMETRY=1`. To send reports to
your own Sentry project instead, set `SENTRY_DSN`. Builds from source have
reporting off unless `MOBI2EPUB_SENTRY_DSN` is set at build time or
`SENTRY_DSN` at run time.

## Building

```sh
cargo build --release
# binary: target/release/mobi2epub
```

## Testing

The tests generate synthetic MOBI files covering each compression type and
encoding, convert them, and check the EPUB structure, links and text:

```sh
python3 tests/make_mobi.py test-out
./target/release/mobi2epub test-out/*.mobi
for f in test-out/*.mobi; do python3 tests/check_epub.py "$f" "${f%.mobi}.epub"; done
```

## Releases

Publishing a GitHub release triggers a workflow that builds binaries for
Linux amd64 and arm64 (static, musl), macOS arm64, and Windows x64 and
ARM64, and attaches them to the release (`.tar.gz`, or `.zip` on Windows).
Set the `SENTRY_DSN` repository secret to enable crash reporting in those
builds. Pull requests and pushes to `main` run the same builds without
publishing anything.
