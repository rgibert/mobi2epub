//! Assembles an EPUB 3 package (with an EPUB 2 NCX for older readers).

use crate::html::{chapter_file, escape, Output};
use crate::mobi::Book;
use crate::zip::ZipWriter;
use std::collections::HashMap;

pub struct Image {
    pub file: String,
    pub media_type: &'static str,
    pub data: Vec<u8>,
}

/// Identify an image record by its magic bytes.
pub fn sniff_image(d: &[u8]) -> Option<(&'static str, &'static str)> {
    if d.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else {
        None
    }
}

/// Map every image record to a file name, keyed by MOBI recindex (1-based).
pub fn collect_images(book: &Book) -> HashMap<usize, Image> {
    let mut map = HashMap::new();
    let Some(first) = book.first_image else { return map };
    for rec in first..book.record_count() {
        let data = book.record(rec).unwrap_or(&[]);
        if let Some((ext, media_type)) = sniff_image(data) {
            let idx = rec - first + 1;
            map.insert(idx, Image { file: format!("image{idx:05}.{ext}"), media_type, data: data.to_vec() });
        }
    }
    map
}

pub struct Stats {
    pub chapters: usize,
    pub images: usize,
}

pub fn build(book: &Book, conv: &Output, images: &HashMap<usize, Image>) -> (Vec<u8>, Stats) {
    let (now_iso, dos_time, dos_date) = now();
    let mut zip = ZipWriter::new(dos_time, dos_date);
    zip.add("mimetype", b"application/epub+zip", false);
    zip.add(
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
        true,
    );

    let lang = book
        .language
        .as_deref()
        .map(|l| l.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en".to_string());
    let title = if book.title.trim().is_empty() { "Untitled".to_string() } else { book.title.trim().to_string() };

    // Cover: the EXTH cover offset is 0-based from the first image record.
    let cover_idx = book.cover_offset.map(|o| o + 1).filter(|i| images.contains_key(i));
    let mut used: Vec<usize> = conv.used_images.iter().copied().collect();
    if let Some(c) = cover_idx {
        if !used.contains(&c) {
            used.push(c);
            used.sort();
        }
    }

    let mut manifest = String::new();
    let mut spine = String::new();
    manifest.push_str(r#"    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="css" href="style.css" media-type="text/css"/>
"#);

    if let Some(c) = cover_idx {
        let img = &images[&c];
        let body = format!(
            "<div class=\"cover\"><img src=\"../images/{}\" alt=\"{}\"/></div>",
            img.file,
            escape(&title)
        );
        zip.add("OEBPS/text/cover.xhtml", xhtml_page(&title, &lang, &body).as_bytes(), true);
        manifest.push_str("    <item id=\"cover\" href=\"text/cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n");
        spine.push_str("    <itemref idref=\"cover\"/>\n");
    }

    for (i, body) in conv.chapters.iter().enumerate() {
        let file = chapter_file(i);
        let id = file.trim_end_matches(".xhtml");
        zip.add(&format!("OEBPS/text/{file}"), xhtml_page(&title, &lang, body).as_bytes(), true);
        manifest.push_str(&format!(
            "    <item id=\"{id}\" href=\"text/{file}\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("    <itemref idref=\"{id}\"/>\n"));
    }

    for idx in &used {
        let img = &images[idx];
        // JPEG/PNG/GIF are already compressed; store them as-is.
        zip.add(&format!("OEBPS/images/{}", img.file), &img.data, false);
        let props = if Some(*idx) == cover_idx { " properties=\"cover-image\"" } else { "" };
        manifest.push_str(&format!(
            "    <item id=\"img{idx:05}\" href=\"images/{}\" media-type=\"{}\"{props}/>\n",
            img.file, img.media_type
        ));
    }

    // Table of contents: headings if there are any, else one entry per part.
    let mut toc: Vec<(String, String)> = conv
        .toc
        .iter()
        .map(|t| (t.title.clone(), format!("text/{}#{}", chapter_file(t.file), t.id)))
        .collect();
    if toc.is_empty() {
        if conv.chapters.len() == 1 {
            toc.push((title.clone(), format!("text/{}", chapter_file(0))));
        } else {
            for i in 0..conv.chapters.len() {
                toc.push((format!("Part {}", i + 1), format!("text/{}", chapter_file(i))));
            }
        }
    }

    let identifier = match &book.isbn {
        Some(isbn) if !isbn.trim().is_empty() => format!("urn:isbn:{}", isbn.trim()),
        _ => format!("urn:uuid:{}", uuid_from(&[title.as_bytes(), &book.text].concat())),
    };

    let mut meta = String::new();
    meta.push_str(&format!("    <dc:identifier id=\"bookid\">{}</dc:identifier>\n", escape(&identifier)));
    meta.push_str(&format!("    <dc:title>{}</dc:title>\n", escape(&title)));
    meta.push_str(&format!("    <dc:language>{}</dc:language>\n", escape(&lang)));
    for a in &book.authors {
        meta.push_str(&format!("    <dc:creator>{}</dc:creator>\n", escape(a)));
    }
    if let Some(p) = book.publisher.as_deref().filter(|s| !s.is_empty()) {
        meta.push_str(&format!("    <dc:publisher>{}</dc:publisher>\n", escape(p)));
    }
    if let Some(d) = book.description.as_deref().filter(|s| !s.is_empty()) {
        meta.push_str(&format!("    <dc:description>{}</dc:description>\n", escape(d)));
    }
    for s in book.subjects.iter().filter(|s| !s.is_empty()) {
        meta.push_str(&format!("    <dc:subject>{}</dc:subject>\n", escape(s)));
    }
    if let Some(d) = book.date.as_deref().and_then(valid_date) {
        meta.push_str(&format!("    <dc:date>{d}</dc:date>\n"));
    }
    meta.push_str(&format!("    <meta property=\"dcterms:modified\">{now_iso}</meta>\n"));
    if cover_idx.is_some() {
        meta.push_str(&format!("    <meta name=\"cover\" content=\"img{:05}\"/>\n", cover_idx.unwrap()));
    }

    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid" xml:lang="{lang}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
{meta}  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine toc="ncx">
{spine}  </spine>
</package>
"#
    );
    zip.add("OEBPS/content.opf", opf.as_bytes(), true);

    let mut nav_items = String::new();
    let mut ncx_points = String::new();
    for (i, (label, href)) in toc.iter().enumerate() {
        nav_items.push_str(&format!("      <li><a href=\"{}\">{}</a></li>\n", escape(href), escape(label)));
        ncx_points.push_str(&format!(
            "    <navPoint id=\"np{n}\" playOrder=\"{n}\"><navLabel><text>{}</text></navLabel><content src=\"{}\"/></navPoint>\n",
            escape(label),
            escape(href),
            n = i + 1
        ));
    }
    let nav = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="{lang}" xml:lang="{lang}">
<head><meta charset="utf-8"/><title>Contents</title></head>
<body>
  <nav epub:type="toc" id="toc">
    <h1>Contents</h1>
    <ol>
{nav_items}    </ol>
  </nav>
</body>
</html>
"#
    );
    zip.add("OEBPS/nav.xhtml", nav.as_bytes(), true);

    let ncx = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="{}"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle><text>{}</text></docTitle>
  <navMap>
{ncx_points}  </navMap>
</ncx>
"#,
        escape(&identifier),
        escape(&title)
    );
    zip.add("OEBPS/toc.ncx", ncx.as_bytes(), true);

    zip.add(
        "OEBPS/style.css",
        b"body { margin: 0 2%; }\n\
p { margin: 0 0 0.4em 0; }\n\
img { max-width: 100%; height: auto; }\n\
.cover { text-align: center; }\n\
.cover img { max-height: 98vh; }\n",
        true,
    );

    (zip.finish(), Stats { chapters: conv.chapters.len(), images: used.len() })
}

fn xhtml_page(title: &str, lang: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="{lang}" xml:lang="{lang}">
<head>
<meta charset="utf-8"/>
<title>{}</title>
<link rel="stylesheet" type="text/css" href="../style.css"/>
</head>
<body>
{body}
</body>
</html>
"#,
        escape(title)
    )
}

/// Accept YYYY, YYYY-MM or YYYY-MM-DD prefixes of a MOBI date.
fn valid_date(d: &str) -> Option<String> {
    let d = d.trim();
    let b = d.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) && b.get(7) == Some(&b'-') && digits(8..10) {
        Some(d[..10].to_string())
    } else if digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) {
        Some(d[..7].to_string())
    } else if digits(0..4) && b.get(4).map_or(true, |c| !c.is_ascii_digit()) {
        Some(d[..4].to_string())
    } else {
        None
    }
}

/// Deterministic UUID derived from the book content (FNV-1a based).
fn uuid_from(data: &[u8]) -> String {
    let fnv = |seed: u64| {
        let mut h = seed;
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    };
    let a = fnv(0xcbf2_9ce4_8422_2325);
    let b = fnv(0x8422_2325_cbf2_9ce4);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_be_bytes());
    bytes[8..].copy_from_slice(&b.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Current UTC time as ISO 8601 plus DOS time/date for the ZIP entries.
fn now() -> (String, u16, u16) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let iso = format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z");
    let dos_time = ((hh << 11) | (mm << 5) | (ss / 2)) as u16;
    let dos_date = (((y - 1980).clamp(0, 127) << 9) | (m << 5) | d) as u16;
    (iso, dos_time, dos_date)
}
