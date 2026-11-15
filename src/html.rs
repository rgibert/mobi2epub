//! Turns MOBI's loose HTML into well-formed XHTML chapters.
//!
//! MOBI 6 markup is HTML 3-era: unclosed tags, `<font>`, `<mbp:pagebreak/>`,
//! `<img recindex="N">` for images and `<a filepos="N">` links that point at
//! byte offsets in the text. This module tokenizes it leniently, rewrites
//! those constructs, balances tags, and splits chapters on page breaks.

use crate::mobi::cp1252;
use std::collections::{BTreeSet, HashMap, HashSet};

const ANCHOR_TAG: &str = "mobi2epub-anchor";
/// Delimiter for link placeholders; stripped from text so it can't collide.
const PH: char = '\u{1}';

pub struct TocEntry {
    pub title: String,
    pub file: usize,
    pub id: String,
}

pub struct Output {
    /// Body markup of each chapter.
    pub chapters: Vec<String>,
    pub toc: Vec<TocEntry>,
    /// recindex values of images actually referenced.
    pub used_images: BTreeSet<usize>,
}

pub fn chapter_file(i: usize) -> String {
    format!("part{:04}.xhtml", i + 1)
}

/// Convert MOBI HTML. `images` maps recindex -> file name under ../images/.
pub fn convert(raw: &[u8], utf8: bool, images: &HashMap<usize, String>) -> Output {
    let with_anchors = insert_anchors(raw, utf8);
    let text = crate::mobi::decode_text(&with_anchors, utf8);
    let mut c = Conv::new(images);
    tokenize(&text, &mut c);
    c.finish()
}

/// Convert plain PalmDOC text: paragraphs separated by newlines.
pub fn convert_plain(raw: &[u8]) -> Output {
    let text = crate::mobi::decode_text(raw, false);
    let mut body = String::new();
    for para in text.split('\n') {
        let t = para.trim();
        if !t.is_empty() {
            body.push_str("<p>");
            escape_text(t, &mut body);
            body.push_str("</p>\n");
        }
    }
    Output { chapters: vec![body], toc: Vec::new(), used_images: BTreeSet::new() }
}

/// Insert a marker tag at every byte offset referenced by a filepos link so
/// the converter can emit an id there.
fn insert_anchors(raw: &[u8], utf8: bool) -> Vec<u8> {
    let mut targets: Vec<(usize, u64)> = Vec::new();
    let needle = b"filepos=";
    let mut i = 0;
    while i + needle.len() <= raw.len() {
        if raw[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            let mut j = i + needle.len();
            while j < raw.len() && (raw[j] == b'"' || raw[j] == b'\'') {
                j += 1;
            }
            let st = j;
            while j < raw.len() && raw[j].is_ascii_digit() {
                j += 1;
            }
            let parsed = std::str::from_utf8(&raw[st..j]).unwrap_or("").parse::<u64>();
            if let Some(n) = parsed.ok().filter(|&n| n < raw.len() as u64) {
                targets.push((adjust_pos(raw, n as usize, utf8), n));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if targets.is_empty() {
        return raw.to_vec();
    }
    targets.sort();
    targets.dedup();
    let mut out = Vec::with_capacity(raw.len() + targets.len() * 40);
    let mut last = 0;
    for (pos, n) in targets {
        out.extend_from_slice(&raw[last..pos]);
        out.extend_from_slice(format!("<{ANCHOR_TAG} n=\"{n}\">").as_bytes());
        last = pos;
    }
    out.extend_from_slice(&raw[last..]);
    out
}

/// Move a filepos target out of the middle of a tag or UTF-8 sequence.
fn adjust_pos(raw: &[u8], pos: usize, utf8: bool) -> usize {
    let mut pos = pos.min(raw.len());
    let lt = raw[..pos].iter().rposition(|&b| b == b'<');
    let gt = raw[..pos].iter().rposition(|&b| b == b'>');
    if let Some(lt) = lt {
        if gt.map_or(true, |g| g < lt) {
            pos = lt;
        }
    }
    if utf8 {
        while pos > 0 && pos < raw.len() && raw[pos] & 0xC0 == 0x80 {
            pos -= 1;
        }
    }
    pos
}

struct Open {
    src: String,
    out: &'static str,
    /// Attributes to use if the element is reopened in the next chapter.
    reopen_attrs: String,
    heading: bool,
}

struct Conv<'a> {
    images: &'a HashMap<usize, String>,
    chapters: Vec<String>,
    cur: String,
    has_content: bool,
    stack: Vec<Open>,
    pending: Vec<u64>,
    anchor_file: HashMap<u64, usize>,
    id_file: HashMap<String, usize>,
    toc: Vec<TocEntry>,
    heading: Option<(String, String)>,
    used: BTreeSet<usize>,
    skip: Option<(String, usize)>,
    ids: HashSet<String>,
    auto_id: usize,
}

const VOID: &[&str] = &["br", "hr", "img", "col", "wbr"];
const BLOCK: &[&str] = &[
    "p", "div", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "dl", "blockquote", "pre", "table",
    "hr", "figure", "section", "article", "aside", "header", "footer", "nav", "address",
];
const KEEP: &[&str] = &[
    "p", "div", "span", "a", "b", "i", "em", "strong", "u", "s", "sub", "sup", "small", "h1", "h2",
    "h3", "h4", "h5", "h6", "ul", "ol", "li", "dl", "dt", "dd", "blockquote", "pre", "code", "br",
    "hr", "img", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "col",
    "colgroup", "cite", "q", "abbr", "var", "kbd", "samp", "del", "ins", "figure", "figcaption",
    "section", "article", "aside", "header", "footer", "nav", "address", "wbr", "dfn", "mark",
];

fn intern(name: &str) -> Option<&'static str> {
    KEEP.iter().copied().find(|k| *k == name)
}

impl<'a> Conv<'a> {
    fn new(images: &'a HashMap<usize, String>) -> Self {
        Conv {
            images,
            chapters: Vec::new(),
            cur: String::new(),
            has_content: false,
            stack: Vec::new(),
            pending: Vec::new(),
            anchor_file: HashMap::new(),
            id_file: HashMap::new(),
            toc: Vec::new(),
            heading: None,
            used: BTreeSet::new(),
            skip: None,
            ids: HashSet::new(),
            auto_id: 0,
        }
    }

    fn text(&mut self, t: &str) {
        if self.skip.is_some() {
            return;
        }
        if !t.trim().is_empty() {
            self.flush_pending();
            self.has_content = true;
        }
        if let Some((_, h)) = self.heading.as_mut() {
            h.push_str(t);
        }
        escape_text(t, &mut self.cur);
    }

    fn flush_pending(&mut self) {
        for n in std::mem::take(&mut self.pending) {
            let id = format!("filepos{n}");
            if self.ids.insert(id.clone()) {
                self.cur.push_str(&format!("<a id=\"{id}\"></a>"));
                self.anchor_file.insert(n, self.chapters.len());
                self.has_content = true;
            }
        }
    }

    fn start(&mut self, name: &str, attrs: &[(String, String)], self_closing: bool) {
        if let Some((tag, depth)) = self.skip.as_mut() {
            if tag == name && !self_closing {
                *depth += 1;
            }
            return;
        }
        match name {
            ANCHOR_TAG => {
                if let Some(n) = attr(attrs, "n").and_then(|v| v.parse().ok()) {
                    self.pending.push(n);
                }
                return;
            }
            "head" | "guide" | "script" | "style" | "title" => {
                if !self_closing {
                    self.skip = Some((name.to_string(), 1));
                }
                return;
            }
            "mbp:pagebreak" => {
                self.page_break();
                return;
            }
            _ => {}
        }

        let mut style = String::new();
        let out: &'static str = match name {
            "font" => {
                font_style(attrs, &mut style);
                "span"
            }
            "center" => {
                style.push_str("text-align: center; ");
                "div"
            }
            "big" => {
                style.push_str("font-size: larger; ");
                "span"
            }
            "tt" => "code",
            "strike" => "s",
            _ => match intern(name) {
                Some(n) => n,
                None => return, // unknown or namespaced tag: drop it, keep content
            },
        };

        self.implicit_close(out);
        self.flush_pending();

        let mut a = String::new();
        let mut id = None;
        let mut src = None;
        let mut alt = String::new();
        let indentable = matches!(out, "p" | "div" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
        let sizable = matches!(out, "img" | "td" | "th" | "table" | "col");
        let mut img_pref: [Option<usize>; 3] = [None; 3];
        for (k, v) in attrs {
            match k.as_str() {
                "id" => id = Some(v.clone()),
                "name" if out == "a" => id = id.or_else(|| Some(v.clone())),
                "class" | "title" | "lang" | "dir" => push_attr(&mut a, k, v),
                "style" => {
                    let s = v.trim().trim_end_matches(';');
                    if !s.is_empty() {
                        style.push_str(s);
                        style.push_str("; ");
                    }
                }
                "align" => {
                    let v = v.to_ascii_lowercase();
                    if matches!(v.as_str(), "left" | "right" | "center" | "justify") && out != "img" {
                        style.push_str(&format!("text-align: {v}; "));
                    }
                }
                "width" => {
                    if let Some(l) = css_len(v) {
                        if indentable {
                            style.push_str(&format!("text-indent: {l}; "));
                        } else if sizable {
                            style.push_str(&format!("width: {l}; "));
                        }
                    }
                }
                "height" => {
                    if let Some(l) = css_len(v) {
                        if indentable {
                            style.push_str(&format!("margin-top: {l}; "));
                        } else if out == "img" {
                            style.push_str(&format!("height: {l}; "));
                        }
                    }
                }
                "colspan" | "rowspan" if matches!(out, "td" | "th") && is_number(v) => push_attr(&mut a, k, v),
                "start" if out == "ol" && is_number(v) => push_attr(&mut a, k, v),
                "value" if out == "li" && is_number(v) => push_attr(&mut a, k, v),
                "alt" if out == "img" => alt = v.clone(),
                "filepos" if out == "a" => {
                    if let Ok(n) = v.trim_matches(|c| c == '"' || c == '\'').parse::<u64>() {
                        a.push_str(&format!("{PH}F{n}{PH}"));
                    }
                }
                "href" if out == "a" => {
                    let v = v.trim();
                    if let Some(frag) = v.strip_prefix('#') {
                        a.push_str(&format!("{PH}I{}{PH}", sanitize_id(frag)));
                    } else if !v.is_empty() && !v.to_ascii_lowercase().starts_with("javascript:") {
                        push_attr(&mut a, "href", v);
                    }
                }
                "hirecindex" | "recindex" | "lorecindex" if out == "img" => {
                    let slot = match k.as_str() { "hirecindex" => 0, "recindex" => 1, _ => 2 };
                    img_pref[slot] = v.trim().parse().ok();
                }
                _ => {}
            }
        }
        if out == "img" {
            for idx in img_pref.into_iter().flatten() {
                if let Some(file) = self.images.get(&idx) {
                    src = Some(file.clone());
                    self.used.insert(idx);
                    break;
                }
            }
            if src.is_none() {
                return; // image we can't resolve: drop it
            }
        }

        // Attributes for a copy reopened after a page break: styling only,
        // no ids or links.
        let reopen_attrs = {
            let mut r = String::new();
            for (k, v) in attrs {
                if matches!(k.as_str(), "class" | "lang" | "dir") {
                    push_attr(&mut r, k, v);
                }
            }
            if !style.is_empty() {
                push_attr(&mut r, "style", style.trim());
            }
            r
        };

        let heading = matches!(out, "h1" | "h2" | "h3");
        let mut id = id.map(|v| sanitize_id(&v)).filter(|v| !self.ids.contains(v));
        if heading && id.is_none() && self.heading.is_none() {
            self.auto_id += 1;
            id = Some(format!("toc-{}", self.auto_id));
        }
        let mut tag = format!("<{out}");
        if let Some(id) = &id {
            self.ids.insert(id.clone());
            self.id_file.insert(id.clone(), self.chapters.len());
            push_attr(&mut tag, "id", id);
            self.has_content = true;
        }
        if let Some(src) = &src {
            push_attr(&mut tag, "src", &format!("../images/{src}"));
            push_attr(&mut tag, "alt", &alt);
            self.has_content = true;
        }
        tag.push_str(&a);
        if !style.is_empty() {
            push_attr(&mut tag, "style", style.trim());
        }

        if VOID.contains(&out) {
            tag.push_str("/>");
            self.cur.push_str(&tag);
            if out == "hr" {
                self.has_content = true;
            }
            return;
        }
        tag.push('>');
        self.cur.push_str(&tag);
        let is_toc_heading = heading && self.heading.is_none();
        if is_toc_heading {
            self.heading = Some((id.unwrap_or_default(), String::new()));
        }
        self.stack.push(Open { src: name.to_string(), out, reopen_attrs, heading: is_toc_heading });
        if self_closing {
            self.end(name);
        }
    }

    /// Mimic HTML's implied end tags so the output nests sensibly.
    fn implicit_close(&mut self, out: &str) {
        let pos_of = |stack: &[Open], name: &str| stack.iter().rposition(|o| o.out == name);
        let after = |stack: &[Open], target: Option<usize>, barrier: &[&str]| -> Option<usize> {
            let t = target?;
            let b = stack.iter().rposition(|o| barrier.contains(&o.out));
            if b.map_or(true, |b| b < t) { Some(t) } else { None }
        };
        let close_from = if BLOCK.contains(&out) {
            after(&self.stack, pos_of(&self.stack, "p"), &["div", "td", "th", "li", "blockquote", "dd"])
        } else {
            match out {
                "li" => after(&self.stack, pos_of(&self.stack, "li"), &["ul", "ol"]),
                "dt" | "dd" => {
                    let t = self.stack.iter().rposition(|o| o.out == "dt" || o.out == "dd");
                    after(&self.stack, t, &["dl"])
                }
                "tr" => after(&self.stack, pos_of(&self.stack, "tr"), &["table"]),
                "td" | "th" => {
                    let t = self.stack.iter().rposition(|o| o.out == "td" || o.out == "th");
                    after(&self.stack, t, &["tr", "table"])
                }
                "a" => pos_of(&self.stack, "a"),
                _ => None,
            }
        };
        if let Some(i) = close_from {
            self.close_to(i);
        }
    }

    fn close_to(&mut self, i: usize) {
        while self.stack.len() > i {
            let o = self.stack.pop().unwrap();
            self.cur.push_str(&format!("</{}>", o.out));
            if o.heading {
                if let Some((id, text)) = self.heading.take() {
                    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !title.is_empty() && !id.is_empty() {
                        self.toc.push(TocEntry { title, file: self.chapters.len(), id });
                    }
                }
            }
        }
    }

    fn end(&mut self, name: &str) {
        if let Some((tag, depth)) = self.skip.as_mut() {
            if tag == name {
                *depth -= 1;
                if *depth == 0 {
                    self.skip = None;
                }
            }
            return;
        }
        if let Some(i) = self.stack.iter().rposition(|o| o.src == name) {
            self.close_to(i);
        }
    }

    fn page_break(&mut self) {
        let reopen: Vec<(String, &'static str, String)> = self
            .stack
            .iter()
            .map(|o| (o.src.clone(), o.out, o.reopen_attrs.clone()))
            .collect();
        self.close_to(0);
        self.finish_chapter();
        for (src, out, attrs) in reopen {
            self.cur.push_str(&format!("<{out}{attrs}>"));
            self.stack.push(Open { src, out, reopen_attrs: attrs, heading: false });
        }
    }

    fn finish_chapter(&mut self) {
        let body = std::mem::take(&mut self.cur);
        if self.has_content {
            self.chapters.push(body);
        }
        self.has_content = false;
    }

    fn finish(mut self) -> Output {
        self.skip = None;
        self.flush_pending();
        self.close_to(0);
        self.finish_chapter();
        if self.chapters.is_empty() {
            self.chapters.push(String::new());
        }
        let anchor_file = &self.anchor_file;
        let id_file = &self.id_file;
        let chapters = self
            .chapters
            .iter()
            .enumerate()
            .map(|(i, ch)| resolve_links(ch, i, anchor_file, id_file))
            .collect();
        Output { chapters, toc: self.toc, used_images: self.used }
    }
}

/// Replace link placeholders with real hrefs now that every target's
/// chapter is known.
fn resolve_links(
    ch: &str,
    this: usize,
    anchor_file: &HashMap<u64, usize>,
    id_file: &HashMap<String, usize>,
) -> String {
    let mut out = String::with_capacity(ch.len());
    let mut parts = ch.split(PH);
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    let mut is_ph = true;
    for part in parts {
        if is_ph {
            let target = if let Some(n) = part.strip_prefix('F') {
                n.parse::<u64>()
                    .ok()
                    .and_then(|n| anchor_file.get(&n).map(|&f| (f, format!("filepos{n}"))))
            } else if let Some(id) = part.strip_prefix('I') {
                id_file.get(id).map(|&f| (f, id.to_string()))
            } else {
                None
            };
            if let Some((f, frag)) = target {
                let href = if f == this { format!("#{frag}") } else { format!("{}#{frag}", chapter_file(f)) };
                push_attr(&mut out, "href", &href);
            }
        } else {
            out.push_str(part);
        }
        is_ph = !is_ph;
    }
    out
}

fn font_style(attrs: &[(String, String)], style: &mut String) {
    if let Some(c) = attr(attrs, "color") {
        let c = c.trim();
        if !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '#') {
            style.push_str(&format!("color: {c}; "));
        }
    }
    if let Some(s) = attr(attrs, "size") {
        let s = s.trim();
        let size = match s {
            "1" | "-2" => Some("x-small"),
            "2" | "-1" => Some("small"),
            "3" | "0" | "+0" => Some("medium"),
            "4" | "+1" => Some("large"),
            "5" | "+2" => Some("x-large"),
            "6" | "+3" => Some("xx-large"),
            "7" | "+4" => Some("xx-large"),
            _ => None,
        };
        match size {
            Some(sz) => style.push_str(&format!("font-size: {sz}; ")),
            None => {
                if let Some(l) = css_len(s) {
                    style.push_str(&format!("font-size: {l}; "));
                }
            }
        }
    }
    if let Some(f) = attr(attrs, "face") {
        let f: String = f.chars().filter(|c| c.is_alphanumeric() || " ,-".contains(*c)).collect();
        if !f.trim().is_empty() {
            style.push_str(&format!("font-family: {}; ", f.trim()));
        }
    }
}

fn attr<'b>(attrs: &'b [(String, String)], name: &str) -> Option<&'b str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn is_number(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty() && v.chars().all(|c| c.is_ascii_digit())
}

/// Validate a MOBI length value and turn it into CSS.
fn css_len(v: &str) -> Option<String> {
    let v = v.trim().to_ascii_lowercase();
    let num_end = v
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && c == '-')))
        .map(|(i, _)| i)
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(num_end);
    if num.is_empty() || num == "-" || num.parse::<f64>().is_err() {
        return None;
    }
    match unit {
        "" if num.parse::<f64>().ok() == Some(0.0) => Some("0".into()),
        "" => Some(format!("{num}px")),
        "px" | "pt" | "em" | "ex" | "%" | "in" | "cm" | "mm" | "pc" | "rem" => Some(v),
        _ => None,
    }
}

fn sanitize_id(v: &str) -> String {
    let s: String = v
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' })
        .collect();
    match s.chars().next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => s,
        _ => format!("id_{s}"),
    }
}

fn push_attr(out: &mut String, k: &str, v: &str) {
    out.push(' ');
    out.push_str(k);
    out.push_str("=\"");
    escape_attr(v, out);
    out.push('"');
}

fn valid_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n') || (c >= ' ' && c != '\u{FFFE}' && c != '\u{FFFF}')
}

pub fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if valid_xml_char(c) => out.push(c),
            _ => {}
        }
    }
}

pub fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if valid_xml_char(c) => out.push(c),
            _ => {}
        }
    }
}

pub fn escape(s: &str) -> String {
    let mut o = String::new();
    escape_attr(s, &mut o);
    o
}

fn tokenize(s: &str, c: &mut Conv) {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if b[i] == b'<' {
            if s[i..].starts_with("<!--") {
                match s[i + 4..].find("-->") {
                    Some(p) => i += 4 + p + 3,
                    None => break,
                }
                continue;
            }
            if i + 1 < n && (b[i + 1] == b'!' || b[i + 1] == b'?') {
                match s[i..].find('>') {
                    Some(p) => i += p + 1,
                    None => break,
                }
                continue;
            }
            if i + 1 < n && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'/') {
                if let Some((tag, next)) = parse_tag(s, i) {
                    if tag.closing {
                        c.end(&tag.name);
                    } else {
                        c.start(&tag.name, &tag.attrs, tag.self_closing);
                    }
                    i = next;
                    continue;
                }
            }
            c.text("<");
            i += 1;
            continue;
        }
        let end = s[i..].find('<').map_or(n, |p| i + p);
        c.text(&decode_entities(&s[i..end]));
        i = end;
    }
}

struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

fn parse_tag(s: &str, start: usize) -> Option<(Tag, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = start + 1;
    let closing = b[j] == b'/';
    if closing {
        j += 1;
    }
    let ns = j;
    while j < n && (b[j].is_ascii_alphanumeric() || matches!(b[j], b':' | b'-' | b'_')) {
        j += 1;
    }
    if j == ns {
        return None;
    }
    let name = s[ns..j].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while j < n && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= n {
            return None;
        }
        match b[j] {
            b'>' => {
                j += 1;
                break;
            }
            b'/' => {
                self_closing = true;
                j += 1;
                continue;
            }
            _ => {}
        }
        self_closing = false;
        let st = j;
        while j < n
            && !b[j].is_ascii_whitespace()
            && b[j] != b'='
            && b[j] != b'>'
            && !(b[j] == b'/' && b.get(j + 1) == Some(&b'>'))
        {
            j += 1;
        }
        if j == st {
            j += 1;
            continue;
        }
        let key = s[st..j].to_ascii_lowercase();
        while j < n && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let mut value = String::new();
        if j < n && b[j] == b'=' {
            j += 1;
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && (b[j] == b'"' || b[j] == b'\'') {
                let q = b[j] as char;
                let end = s[j + 1..].find(q)? + j + 1;
                value = decode_entities(&s[j + 1..end]);
                j = end + 1;
            } else {
                let vs = j;
                while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                    j += 1;
                }
                value = decode_entities(&s[vs..j]);
            }
        }
        attrs.push((key, value));
    }
    Some((Tag { name, closing, self_closing, attrs }, j))
}

const LATIN1: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf",
    "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para",
    "middot", "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave",
    "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute", "Ecirc",
    "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute", "Ocirc",
    "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute", "THORN",
    "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil", "egrave",
    "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve",
    "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc", "uuml",
    "yacute", "thorn", "yuml",
];

const OTHER_ENTITIES: &[(&str, u32)] = &[
    ("amp", 38), ("lt", 60), ("gt", 62), ("quot", 34), ("apos", 39), ("OElig", 338),
    ("oelig", 339), ("Scaron", 352), ("scaron", 353), ("Yuml", 376), ("fnof", 402), ("circ", 710),
    ("tilde", 732), ("ensp", 8194), ("emsp", 8195), ("thinsp", 8201), ("zwnj", 8204),
    ("zwj", 8205), ("lrm", 8206), ("rlm", 8207), ("ndash", 8211), ("mdash", 8212),
    ("lsquo", 8216), ("rsquo", 8217), ("sbquo", 8218), ("ldquo", 8220), ("rdquo", 8221),
    ("bdquo", 8222), ("dagger", 8224), ("Dagger", 8225), ("bull", 8226), ("hellip", 8230),
    ("permil", 8240), ("prime", 8242), ("Prime", 8243), ("lsaquo", 8249), ("rsaquo", 8250),
    ("euro", 8364), ("trade", 8482), ("larr", 8592), ("rarr", 8594), ("uarr", 8593),
    ("darr", 8595),
];

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let v = if let Some(h) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(h, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        if (0x80..=0x9F).contains(&v) {
            return cp1252(v as u8);
        }
        return char::from_u32(v).filter(|&c| valid_xml_char(c) || c == '\r');
    }
    if let Some(i) = LATIN1.iter().position(|&e| e == name) {
        return char::from_u32(160 + i as u32);
    }
    OTHER_ENTITIES
        .iter()
        .find(|(e, _)| *e == name)
        .and_then(|&(_, v)| char::from_u32(v))
}

/// Decode HTML character references; unknown ones are left as literal text.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let semi = rest[1..].char_indices().take(12).find(|&(_, c)| c == ';').map(|(i, _)| i + 1);
        match semi.and_then(|e| entity(&rest[1..e]).map(|c| (c, e))) {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
