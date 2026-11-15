//! Reading MOBI / PalmDOC containers: PalmDB records, the MOBI and EXTH
//! headers, and the two text compression schemes (PalmDOC LZ77 and HUFF/CDIC).

pub type Result<T> = std::result::Result<T, String>;

fn be16(d: &[u8], off: usize) -> Result<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("unexpected end of data reading u16 at {off}"))
}

fn be32(d: &[u8], off: usize) -> Result<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("unexpected end of data reading u32 at {off}"))
}

/// A parsed book: metadata plus the raw (decompressed) HTML text.
pub struct Book {
    data: Vec<u8>,
    records: Vec<(usize, usize)>,
    pub title: String,
    pub authors: Vec<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub isbn: Option<String>,
    pub subjects: Vec<String>,
    pub date: Option<String>,
    pub language: Option<String>,
    /// Decompressed book text (HTML for MOBI, plain text for PalmDOC).
    pub text: Vec<u8>,
    pub utf8: bool,
    /// True for MOBI files (HTML markup); false for plain PalmDOC text.
    pub is_html: bool,
    /// Index of the first image record, if the book has images.
    pub first_image: Option<usize>,
    /// Cover image, as a 0-based offset from `first_image`.
    pub cover_offset: Option<usize>,
}

impl Book {
    pub fn record(&self, i: usize) -> Option<&[u8]> {
        self.records.get(i).map(|&(s, e)| &self.data[s..e])
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

pub fn decode_text(b: &[u8], utf8: bool) -> String {
    if utf8 {
        String::from_utf8_lossy(b).into_owned()
    } else {
        b.iter().filter_map(|&c| cp1252(c)).collect()
    }
}

/// Map a Windows-1252 byte to a char (None for the five undefined bytes).
pub fn cp1252(c: u8) -> Option<char> {
    const HIGH: [u16; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
        0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
        0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    match c {
        0x80..=0x9F => match HIGH[(c - 0x80) as usize] {
            0 => None,
            u => char::from_u32(u as u32),
        },
        _ => Some(c as char),
    }
}

pub fn parse(data: Vec<u8>) -> Result<Book> {
    if data.len() < 78 {
        return Err("file is too small to be a MOBI/PalmDOC book".into());
    }
    let db_type = &data[60..68];
    let is_mobi = db_type == b"BOOKMOBI";
    let is_palmdoc = db_type == b"TEXtREAd";
    if !is_mobi && !is_palmdoc {
        return Err(format!(
            "not a MOBI file (PalmDB type is {:?})",
            String::from_utf8_lossy(db_type)
        ));
    }

    let num = be16(&data, 76)? as usize;
    let mut offsets = Vec::with_capacity(num);
    for i in 0..num {
        offsets.push(be32(&data, 78 + i * 8)? as usize);
    }
    let mut records = Vec::with_capacity(num);
    for i in 0..num {
        let start = offsets[i].min(data.len());
        let end = offsets.get(i + 1).copied().unwrap_or(data.len()).min(data.len());
        records.push((start, end.max(start)));
    }
    if records.is_empty() {
        return Err("book has no records".into());
    }

    let palm_name = {
        let raw = &data[0..32];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(32);
        String::from_utf8_lossy(&raw[..end]).replace('_', " ")
    };

    let (r0s, r0e) = records[0];
    let rec0 = &data[r0s..r0e];
    let compression = be16(rec0, 0)?;
    let text_length = be32(rec0, 4)? as usize;
    let text_records = be16(rec0, 8)? as usize;
    let encryption = be16(rec0, 12)?;
    if encryption != 0 {
        return Err("this book is DRM-protected (encrypted) and cannot be converted".into());
    }

    let mut book = Book {
        data: Vec::new(),
        records: Vec::new(),
        title: palm_name,
        authors: Vec::new(),
        publisher: None,
        description: None,
        isbn: None,
        subjects: Vec::new(),
        date: None,
        language: None,
        text: Vec::new(),
        utf8: false,
        is_html: false,
        first_image: None,
        cover_offset: None,
    };

    let mut extra_flags = 0u16;
    let mut huff_range = None;

    if is_mobi && rec0.get(16..20) == Some(b"MOBI") {
        book.is_html = true;
        let header_len = be32(rec0, 20)? as usize;
        book.utf8 = be32(rec0, 28)? == 65001;
        let version = be32(rec0, 36).unwrap_or(0);
        if version >= 8 {
            return Err("this is a KF8 (AZW3) book; only MOBI 6 / hybrid MOBI files are supported".into());
        }
        let name_off = be32(rec0, 84).unwrap_or(0) as usize;
        let name_len = be32(rec0, 88).unwrap_or(0) as usize;
        let first_image = be32(rec0, 108).unwrap_or(u32::MAX);
        if first_image != u32::MAX && (first_image as usize) < records.len() && first_image > 0 {
            book.first_image = Some(first_image as usize);
        }
        if compression == 17480 {
            let off = be32(rec0, 112)? as usize;
            let count = be32(rec0, 116)? as usize;
            huff_range = Some((off, count));
        }
        if header_len >= 0xE4 && rec0.len() >= 16 + 0xE4 {
            extra_flags = be16(rec0, 16 + 0xE2)?;
        }
        if let Some(name) = rec0.get(name_off..name_off.saturating_add(name_len)) {
            if !name.is_empty() {
                book.title = decode_text(name, book.utf8);
            }
        }
        let exth_flags = be32(rec0, 128).unwrap_or(0);
        if exth_flags & 0x40 != 0 {
            read_exth(&mut book, rec0, 16 + header_len)?;
        }
    }

    // Decompress text records.
    let mut huff = match huff_range {
        Some((off, count)) => {
            let recs: Vec<&[u8]> = (off..off + count)
                .map(|i| {
                    records
                        .get(i)
                        .map(|&(s, e)| &data[s..e])
                        .ok_or_else(|| "HUFF/CDIC record index out of range".to_string())
                })
                .collect::<Result<_>>()?;
            Some(Huff::new(&recs)?)
        }
        None => None,
    };

    let mut text = Vec::with_capacity(text_length);
    for i in 1..=text_records {
        let Some(&(s, e)) = records.get(i) else { break };
        let mut rec = &data[s..e];
        if extra_flags != 0 {
            let trail = trailing_size(rec, extra_flags);
            rec = &rec[..rec.len().saturating_sub(trail)];
        }
        match compression {
            1 => text.extend_from_slice(rec),
            2 => palmdoc_decompress(rec, &mut text),
            17480 => {
                let h = huff.as_mut().ok_or("missing HUFF tables")?;
                text.extend(h.unpack(rec, 0)?);
            }
            other => return Err(format!("unsupported compression type {other}")),
        }
    }
    if text.len() > text_length && text_length > 0 {
        text.truncate(text_length);
    }
    book.text = text;
    book.data = data;
    book.records = records;
    Ok(book)
}

fn read_exth(book: &mut Book, rec0: &[u8], off: usize) -> Result<()> {
    if rec0.get(off..off + 4) != Some(b"EXTH") {
        return Ok(());
    }
    let count = be32(rec0, off + 8)? as usize;
    let mut p = off + 12;
    for _ in 0..count {
        let ty = be32(rec0, p)?;
        let len = be32(rec0, p + 4)? as usize;
        if len < 8 || p + len > rec0.len() {
            break;
        }
        let payload = &rec0[p + 8..p + len];
        let s = || decode_text(payload, book.utf8).trim().to_string();
        match ty {
            100 => {
                let a = s();
                if !a.is_empty() {
                    book.authors.push(a);
                }
            }
            101 => book.publisher = Some(s()),
            103 => book.description = Some(s()),
            104 => book.isbn = Some(s()),
            105 => book.subjects.push(s()),
            106 => book.date = Some(s()),
            201 if payload.len() >= 4 => book.cover_offset = Some(be32(payload, 0)? as usize),
            503 => {
                let t = s();
                if !t.is_empty() {
                    book.title = t;
                }
            }
            524 => book.language = Some(s()),
            _ => {}
        }
        p += len;
    }
    Ok(())
}

/// Size of the trailing entries appended to a text record (per the
/// MOBI header's extra-data flags), which must be stripped before decoding.
fn trailing_size(rec: &[u8], flags: u16) -> usize {
    let size = rec.len();
    let mut num = 0usize;
    let mut f = flags >> 1;
    while f != 0 {
        if f & 1 != 0 {
            // Backward-encoded variable length integer.
            let mut end = size.saturating_sub(num);
            let (mut val, mut bits) = (0usize, 0u32);
            while end > 0 {
                let v = rec[end - 1];
                val |= ((v & 0x7F) as usize) << bits;
                bits += 7;
                end -= 1;
                if v & 0x80 != 0 || bits >= 28 {
                    break;
                }
            }
            num += val;
        }
        f >>= 1;
    }
    if flags & 1 != 0 && num < size {
        num += (rec[size - num - 1] & 0x3) as usize + 1;
    }
    num.min(size)
}

/// PalmDOC LZ77 decompression.
pub fn palmdoc_decompress(d: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    let mut i = 0;
    while i < d.len() {
        let c = d[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + c as usize).min(d.len());
                out.extend_from_slice(&d[i..end]);
                i = end;
            }
            0x80..=0xBF => {
                if i >= d.len() {
                    break;
                }
                let m = ((c as usize) << 8) | d[i] as usize;
                i += 1;
                let dist = (m >> 3) & 0x7FF;
                let len = (m & 7) + 3;
                if dist == 0 || dist > out.len() - start {
                    continue;
                }
                for _ in 0..len {
                    out.push(out[out.len() - dist]);
                }
            }
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            _ => out.push(c),
        }
    }
}

/// HUFF/CDIC decompressor (used by Amazon's own MOBI tooling).
struct Huff {
    dict1: Vec<(u32, bool, u64)>,
    mincode: [u64; 33],
    maxcode: [u64; 33],
    dict: Vec<Option<(Vec<u8>, bool)>>,
}

impl Huff {
    fn new(recs: &[&[u8]]) -> Result<Self> {
        let huff = recs.first().ok_or("missing HUFF record")?;
        if huff.get(0..8) != Some(b"HUFF\x00\x00\x00\x18") {
            return Err("invalid HUFF record".into());
        }
        let off1 = be32(huff, 8)? as usize;
        let off2 = be32(huff, 12)? as usize;
        let mut dict1 = Vec::with_capacity(256);
        for i in 0..256 {
            let v = be32(huff, off1 + i * 4)?;
            let codelen = v & 0x1F;
            let term = v & 0x80 != 0;
            let max = ((((v >> 8) as u64) + 1) << (32 - codelen)).wrapping_sub(1);
            dict1.push((codelen, term, max));
        }
        let mut mincode = [0u64; 33];
        let mut maxcode = [0u64; 33];
        maxcode[0] = (1u64 << 32) - 1;
        for len in 1..=32usize {
            let mn = be32(huff, off2 + (len - 1) * 8)? as u64;
            let mx = be32(huff, off2 + (len - 1) * 8 + 4)? as u64;
            mincode[len] = mn << (32 - len);
            maxcode[len] = ((mx + 1) << (32 - len)) - 1;
        }
        let mut dict = Vec::new();
        for cdic in &recs[1..] {
            if cdic.get(0..8) != Some(b"CDIC\x00\x00\x00\x10") {
                return Err("invalid CDIC record".into());
            }
            let phrases = be32(cdic, 8)? as usize;
            let bits = be32(cdic, 12)?.min(31);
            let n = (1usize << bits).min(phrases.saturating_sub(dict.len()));
            for i in 0..n {
                let off = be16(cdic, 16 + i * 2)? as usize;
                let blen = be16(cdic, 16 + off)? as usize;
                let s = 18 + off;
                let e = s + (blen & 0x7FFF);
                let slice = cdic.get(s..e).ok_or("CDIC entry out of range")?;
                dict.push(Some((slice.to_vec(), blen & 0x8000 != 0)));
            }
        }
        Ok(Huff { dict1, mincode, maxcode, dict })
    }

    fn unpack(&mut self, data: &[u8], depth: usize) -> Result<Vec<u8>> {
        if depth > 32 {
            return Err("HUFF dictionary recursion too deep".into());
        }
        let read64 = |pos: usize| -> u64 {
            let mut b = [0u8; 8];
            for (k, slot) in b.iter_mut().enumerate() {
                *slot = data.get(pos + k).copied().unwrap_or(0);
            }
            u64::from_be_bytes(b)
        };
        let mut out = Vec::new();
        let mut bitsleft = data.len() as i64 * 8;
        let mut pos = 0usize;
        let mut x = read64(pos);
        let mut n: i32 = 32;
        loop {
            if n <= 0 {
                pos += 4;
                x = read64(pos);
                n += 32;
            }
            let code = (x >> n) & 0xFFFF_FFFF;
            let (mut codelen, term, mut maxcode) = self.dict1[(code >> 24) as usize];
            if codelen == 0 {
                return Err("corrupt HUFF data".into());
            }
            if !term {
                while (codelen as usize) <= 32 && code < self.mincode[codelen as usize] {
                    codelen += 1;
                }
                if codelen > 32 {
                    return Err("corrupt HUFF data".into());
                }
                maxcode = self.maxcode[codelen as usize];
            }
            n -= codelen as i32;
            bitsleft -= codelen as i64;
            if bitsleft < 0 {
                break;
            }
            if maxcode < code {
                return Err("corrupt HUFF data".into());
            }
            let r = ((maxcode - code) >> (32 - codelen)) as usize;
            let entry = self
                .dict
                .get_mut(r)
                .ok_or("HUFF code refers to missing dictionary entry")?
                .take()
                .ok_or("recursive HUFF dictionary entry")?;
            let (bytes, literal) = entry;
            let expanded = if literal { bytes } else { self.unpack(&bytes, depth + 1)? };
            out.extend_from_slice(&expanded);
            self.dict[r] = Some((expanded, true));
        }
        Ok(out)
    }
}
