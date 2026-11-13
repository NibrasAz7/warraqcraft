//! Minimal PDF writer for WarraqCraft's native typesetting path —
//! enough for typeset output: pages, positioned text runs in an
//! embedded TrueType (CIDFontType2 / Identity-H / ToUnicode), and
//! filled rectangles for rules. Written fresh from the PDF 32000
//! reference; no third-party PDF code.
//!
//! Everything the typesetter hands us is untrusted: sizes are bounded,
//! glyph ids are masked to u16, malformed fonts error rather than
//! panic. The writer never shells out and works identically on wasm —
//! callers hand it font bytes and take `Vec<u8>` back.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A parse/emit failure — callers map to their own error type.
#[derive(Debug)]
pub struct PdfError(pub String);

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PdfError {}

/// Big-endian u16 at `off`, bounds-checked.
fn be16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(off)?, *b.get(off + 1)?]))
}
/// Big-endian u32 at `off`, bounds-checked.
fn be32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(off)?, *b.get(off + 1)?, *b.get(off + 2)?, *b.get(off + 3)?]))
}
/// Big-endian i16 at `off`, bounds-checked.
fn be16s(b: &[u8], off: usize) -> Option<i16> {
    Some(i16::from_be_bytes([*b.get(off)?, *b.get(off + 1)?]))
}

// ---------- TrueType parsing ----------

/// The slice of a TTF we need: glyph mapping, advances, and the
/// metrics typesetting uses. Raw bytes are kept so the font can be
/// embedded into the PDF whole.
pub struct Ttf {
    /// `unitsPerEm` from `head`.
    pub units_per_em: u16,
    /// `ascender`/`descender` from `hhea`, in font units.
    pub ascender: i16,
    pub descender: i16,
    /// Raw font file — embedded as FontFile2.
    pub bytes: Vec<u8>,
    /// PostScript name (nameID 6) for BaseFont, sanitized.
    pub ps_name: String,
    cmap4: Option<(usize, usize)>, // (offset, length) of a format-4 subtable
    cmap12: Option<(usize, usize)>,
    num_glyphs: u16,
    /// Per-glyph advances in font units, resolved eagerly at parse time.
    advances: Vec<u16>,
    /// Table directory — tag → (offset, length), kept for subsetting.
    dir: BTreeMap<[u8; 4], (usize, usize)>,
}

impl Ttf {
    /// Parse table directory + the tables we consume. Garbage → Err.
    pub fn parse(bytes: &[u8]) -> Result<Ttf, PdfError> {
        let bad = || PdfError("not a usable TrueType font".to_string());
        if bytes.len() < 12 {
            return Err(bad());
        }
        let num_tables = be16(bytes, 4).ok_or_else(bad)? as usize;
        if num_tables > 256 {
            return Err(bad());
        }
        let mut t: BTreeMap<[u8; 4], (usize, usize)> = BTreeMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let (Some(tag), Some(off), Some(len)) = (bytes.get(rec..rec + 4), be32(bytes, rec + 8), be32(bytes, rec + 12)) else {
                return Err(bad());
            };
            let (off, len) = (off as usize, len as usize);
            if off.checked_add(len).is_none_or(|end| end > bytes.len()) {
                return Err(bad());
            }
            t.insert([tag[0], tag[1], tag[2], tag[3]], (off, len));
        }
        let need = |name: &[u8; 4]| t.get(name).copied().ok_or_else(bad);
        let (head, _) = need(b"head")?;
        let (hhea, _) = need(b"hhea")?;
        let (maxp, _) = need(b"maxp")?;
        let (hmtx, _) = need(b"hmtx")?;
        let (cmap, cmap_len) = need(b"cmap")?;

        let units_per_em = be16(bytes, head + 18).ok_or_else(bad)?;
        if units_per_em == 0 {
            return Err(bad());
        }
        let ascender = be16s(bytes, hhea + 4).ok_or_else(bad)?;
        let descender = be16s(bytes, hhea + 6).ok_or_else(bad)?;
        let num_hmetrics = be16(bytes, hhea + 34).ok_or_else(bad)?;
        let num_glyphs = be16(bytes, maxp + 4).ok_or_else(bad)?;
        if num_hmetrics == 0 || num_glyphs == 0 {
            return Err(bad());
        }

        // Advance table: numHMetrics (advance,lsb) pairs then lsb-only.
        let mut advances = Vec::with_capacity(num_glyphs as usize);
        for g in 0..num_glyphs as usize {
            let idx = g.min(num_hmetrics as usize - 1);
            let adv = be16(bytes, hmtx + idx * 4).ok_or_else(bad)?;
            advances.push(adv);
        }

        // cmap: prefer a Unicode format-12 subtable, then format-4.
        let mut cmap4 = None;
        let mut cmap12 = None;
        let nsub = be16(bytes, cmap + 2).ok_or_else(bad)? as usize;
        for i in 0..nsub.min(64) {
            let rec = cmap + 4 + i * 8;
            let (Some(pid), Some(_eid), Some(sub)) = (be16(bytes, rec), be16(bytes, rec + 2), be32(bytes, rec + 4)) else {
                break;
            };
            let off = cmap + sub as usize;
            let Some(fmt) = be16(bytes, off) else { continue };
            // Unicode-bearing subtables only (platform 0/3).
            if pid != 0 && pid != 3 {
                continue;
            }
            if fmt == 12 && cmap12.is_none() {
                cmap12 = Some((off, cmap_len.saturating_sub(sub as usize)));
            } else if fmt == 4 {
                cmap4 = Some((off, cmap_len.saturating_sub(sub as usize)));
            }
        }

        // PostScript name (nameID 6) — best-effort; fall back generic.
        let mut ps_name = "WarraqFont".to_string();
        if let Some(&(name_off, name_len)) = t.get(b"name") {
            if let Some(n) = name_string(bytes, name_off, name_len, 6) {
                ps_name = n.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(63).collect();
                if ps_name.is_empty() {
                    ps_name = "WarraqFont".to_string();
                }
            }
        }

        Ok(Ttf { units_per_em, ascender, descender, bytes: bytes.to_vec(), ps_name, cmap4, cmap12, num_glyphs, advances, dir: t })
    }

    /// Unicode scalar → glyph id (0 = .notdef), via format 12 then 4.
    pub fn gid(&self, c: char) -> u16 {
        let cp = c as u32;
        if let Some((off, len)) = self.cmap12 {
            if let Some(g) = cmap12_lookup(&self.bytes, off, len, cp) {
                return if g < self.num_glyphs { g } else { 0 };
            }
        }
        if let Some((off, len)) = self.cmap4 {
            if let Ok(bmp) = u16::try_from(cp) {
                if let Some(g) = cmap4_lookup(&self.bytes, off, len, bmp) {
                    return g;
                }
            }
        }
        0
    }

    /// Advance width of a glyph in font units.
    pub fn advance(&self, gid: u16) -> u16 {
        self.advances.get(gid as usize).copied().unwrap_or(0)
    }

    /// Advance width of a character in font units (0 for .notdef-miss).
    pub fn char_width(&self, c: char) -> u16 {
        self.advance(self.gid(c))
    }

    /// Font-units → pt scale at `size` pt.
    pub fn scale(&self, size: f64) -> f64 {
        size / self.units_per_em as f64
    }

    /// Advance of `c` in pt at `size`.
    pub fn width_pt(&self, c: char, size: f64) -> f64 {
        self.char_width(c) as f64 * self.scale(size)
    }

    /// Rebuild the font containing only `used` gids (plus .notdef),
    /// **preserving glyph ids** — content streams already reference
    /// them, so unused slots become empty glyphs rather than a
    /// renumber. Composite components are pulled in (ids unchanged),
    /// glyf/loca/hmtx are truncated at the highest used gid, cmap is
    /// regenerated from the used chars, head/hhea/maxp patched, and
    /// `name`/`OS/2`/`post` copy verbatim. `None` on anything
    /// unexpected — callers embed the whole font instead.
    pub fn subset(&self, used: &BTreeMap<u16, char>) -> Option<Vec<u8>> {
        let src = &self.bytes;
        let tab = |t: &[u8; 4]| -> Option<(usize, usize)> { self.dir.get(t).copied() };
        let (go, gl) = tab(b"glyf")?;
        let glyf = src.get(go..go + gl)?;
        let (lo, _) = tab(b"loca")?;
        let (hmo, _) = tab(b"hmtx")?;
        let (hdo, hdl) = tab(b"head")?;
        let (hh, _) = tab(b"hhea")?;
        let (mo, _) = tab(b"maxp")?;
        let nhmet = be16(src, hh + 34)? as usize;
        let long_loca = be16s(src, hdo + 50)? != 0;
        let ng = self.num_glyphs as usize;
        let mut loc = Vec::with_capacity(ng + 1);
        for g in 0..=ng {
            let v = if long_loca { be32(src, lo + g * 4)? as usize } else { be16(src, lo + g * 2)? as usize * 2 };
            if v > gl {
                return None;
            }
            loc.push(v);
        }
        // Glyph closure — pull in composite components, then renumber.
        let mut set: std::collections::BTreeSet<u16> = used.keys().copied().filter(|&g| (g as usize) < ng).collect();
        set.insert(0);
        let mut stack: Vec<u16> = set.iter().copied().collect();
        while let Some(g) = stack.pop() {
            let (s, e) = (loc[g as usize], loc[g as usize + 1]);
            if e <= s + 10 || be16s(glyf, s)? >= 0 {
                continue; // empty or simple glyph
            }
            let mut p = s + 10;
            while let (Some(fl), Some(gid)) = (be16(glyf, p), be16(glyf, p + 2)) {
                p += 8; // flags + glyph id + two arg words
                if fl & 0x0008 != 0 {
                    p += 2;
                } else if fl & 0x0040 != 0 {
                    p += 4;
                } else if fl & 0x0080 != 0 {
                    p += 8;
                }
                if (gid as usize) < ng && set.insert(gid) {
                    stack.push(gid);
                }
                if fl & 0x0020 == 0 || p > e {
                    break;
                }
            }
        }
        let max_g = (*set.iter().next_back()?) as usize;
        // glyf — slots 0..=max_g; unused glyphs get empty bodies but
        // keep their ids so existing content stays valid.
        let mut nglyf = Vec::new();
        let mut nloca = Vec::with_capacity(max_g + 2);
        for g in 0..=max_g {
            nloca.push(nglyf.len() as u32);
            if set.contains(&(g as u16)) {
                let (s, e) = (loc[g], loc[g + 1]);
                nglyf.extend_from_slice(glyf.get(s..e)?);
                while nglyf.len() % 4 != 0 {
                    nglyf.push(0);
                }
            }
        }
        nloca.push(nglyf.len() as u32);
        // hmtx — one (advance, lsb) pair per retained glyph id.
        let mut nhmtx = Vec::with_capacity((max_g + 1) * 4);
        for g in 0..=max_g {
            let (adv, lsb) = if g < nhmet {
                (be16(src, hmo + g * 4)?, be16(src, hmo + g * 4 + 2)?)
            } else {
                (be16(src, hmo + (nhmet - 1) * 4)?, be16(src, hmo + nhmet * 4 + (g - nhmet) * 2)?)
            };
            nhmtx.extend_from_slice(&adv.to_be_bytes());
            nhmtx.extend_from_slice(&lsb.to_be_bytes());
        }
        // cmap — char → gid (unchanged ids), sorted for grouping.
        let mut pairs: Vec<(u32, u32)> = used.iter().filter_map(|(&g, &c)| ((g as usize) < ng).then_some((c as u32, g as u32))).collect();
        pairs.sort_unstable();
        let nsub = if pairs.iter().all(|&(c, _)| c <= 0xFFFF) { 2 } else { 1 };
        let fmt4_len = if nsub == 2 { 16 + (pairs.len() + 1) * 8 + pairs.len() * 2 } else { 0 };
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&[0, 0, 0, nsub as u8]);
        if nsub == 2 {
            cmap.extend_from_slice(&[0, 3, 0, 1]);
            cmap.extend_from_slice(&((4 + nsub * 8) as u32).to_be_bytes());
        }
        cmap.extend_from_slice(&[0, 3, 0, 10]);
        cmap.extend_from_slice(&((4 + nsub * 8 + fmt4_len) as u32).to_be_bytes());
        if nsub == 2 {
            // format 4 — one single-char segment per char, glyph ids in
            // the trailing array (idRangeOffset uniform, see spec §cmap4).
            let segs = pairs.len() + 1;
            let mut pw = 1usize;
            let mut es = 0usize;
            while pw * 2 <= segs {
                pw *= 2;
                es += 1;
            }
            cmap.extend_from_slice(&4u16.to_be_bytes());
            cmap.extend_from_slice(&(fmt4_len as u16).to_be_bytes());
            cmap.extend_from_slice(&[0, 0]);
            cmap.extend_from_slice(&(segs as u16 * 2).to_be_bytes());
            cmap.extend_from_slice(&(pw as u16 * 2).to_be_bytes());
            cmap.extend_from_slice(&(es as u16).to_be_bytes());
            cmap.extend_from_slice(&(segs as u16 * 2 - pw as u16 * 2).to_be_bytes());
            for &(c, _) in &pairs {
                cmap.extend_from_slice(&(c as u16).to_be_bytes());
            }
            cmap.extend_from_slice(&0xFFFFu16.to_be_bytes());
            cmap.extend_from_slice(&[0, 0]);
            for &(c, _) in &pairs {
                cmap.extend_from_slice(&(c as u16).to_be_bytes());
            }
            cmap.extend_from_slice(&0xFFFFu16.to_be_bytes());
            for _ in &pairs {
                cmap.extend_from_slice(&0u16.to_be_bytes());
            }
            cmap.extend_from_slice(&1u16.to_be_bytes()); // idDelta terminator
            for _ in &pairs {
                cmap.extend_from_slice(&(segs as u16 * 2).to_be_bytes());
            }
            cmap.extend_from_slice(&0u16.to_be_bytes());
            for &(_, g) in &pairs {
                cmap.extend_from_slice(&(g as u16).to_be_bytes());
            }
        }
        // format 12 — contiguous (char,gid) groups.
        let mut groups: Vec<(u32, u32, u32)> = Vec::new();
        for &(c, g) in &pairs {
            match groups.last_mut() {
                Some(gr) if gr.1 + 1 == c && gr.2 + (c - gr.0) == g => gr.1 = c,
                _ => groups.push((c, c, g)),
            }
        }
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&((16 + groups.len() * 12) as u32).to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for &(a, b, g) in &groups {
            cmap.extend_from_slice(&a.to_be_bytes());
            cmap.extend_from_slice(&b.to_be_bytes());
            cmap.extend_from_slice(&g.to_be_bytes());
        }
        // loca — long format.
        let mut nloca_b = Vec::with_capacity(nloca.len() * 4);
        for v in &nloca {
            nloca_b.extend_from_slice(&v.to_be_bytes());
        }
        // head/hhea/maxp patched copies.
        let mut nhead = src.get(hdo..hdo + hdl)?.to_vec();
        nhead.get_mut(8..12)?.copy_from_slice(&0u32.to_be_bytes());
        nhead.get_mut(50..52)?.copy_from_slice(&1i16.to_be_bytes());
        let mut nhhea = src.get(hh..hh + tab(b"hhea")?.1)?.to_vec();
        nhhea.get_mut(34..36)?.copy_from_slice(&((max_g + 1) as u16).to_be_bytes());
        let mut nmaxp = src.get(mo..mo + tab(b"maxp")?.1)?.to_vec();
        nmaxp.get_mut(4..6)?.copy_from_slice(&((max_g + 1) as u16).to_be_bytes());
        // Assemble — sorted directory, 4-byte-aligned tables.
        let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"cmap", cmap),
            (*b"glyf", nglyf),
            (*b"head", nhead),
            (*b"hhea", nhhea),
            (*b"hmtx", nhmtx),
            (*b"loca", nloca_b),
            (*b"maxp", nmaxp),
        ];
        for tag in [b"name", b"post", b"OS/2"] {
            if let Some(&(o, l)) = self.dir.get(tag) {
                if let Some(d) = src.get(o..o + l) {
                    tables.push((*tag, d.to_vec()));
                }
            }
        }
        tables.sort_by_key(|(t, _)| *t);
        let nt = tables.len() as u16;
        let mut pw = 1u16;
        let mut es = 0u16;
        while pw * 2 <= nt {
            pw *= 2;
            es += 1;
        }
        let mut out = Vec::new();
        out.extend_from_slice(&0x00010000u32.to_be_bytes());
        out.extend_from_slice(&nt.to_be_bytes());
        out.extend_from_slice(&(pw * 16).to_be_bytes());
        out.extend_from_slice(&es.to_be_bytes());
        out.extend_from_slice(&(nt * 16 - pw * 16).to_be_bytes());
        let mut off = 12 + nt as usize * 16;
        let mut recs = Vec::new();
        for (tag, data) in &tables {
            recs.push((*tag, table_sum(data), off as u32, data.len() as u32));
            off += (data.len() + 3) & !3;
        }
        for (tag, cs, o, l) in &recs {
            out.extend_from_slice(tag);
            out.extend_from_slice(&cs.to_be_bytes());
            out.extend_from_slice(&o.to_be_bytes());
            out.extend_from_slice(&l.to_be_bytes());
        }
        let head_off = recs.iter().find(|(t, ..)| t == b"head").map(|(.., o, _)| *o as usize)?;
        for (_, data) in &tables {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        // checkSumAdjustment — the whole file, that field held at 0.
        let adj = 0xB1B0AFBAu32.wrapping_sub(table_sum(&out));
        out.get_mut(head_off + 8..head_off + 12)?.copy_from_slice(&adj.to_be_bytes());
        Some(out)
    }
}

/// Table checksum — big-endian u32 words, tail zero-padded.
fn table_sum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |acc, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        acc.wrapping_add(u32::from_be_bytes(w))
    })
}

/// nameID `want` as UTF-8 — first Windows-Unicode or Macintosh record
/// that decodes. Bounded scan; malformed tables give `None`.
fn name_string(b: &[u8], off: usize, len: usize, want: u16) -> Option<String> {
    let storage = be16(b, off + 4)? as usize + off;
    let count = be16(b, off + 2)? as usize;
    for i in 0..count.min(512) {
        let rec = off + 6 + i * 12;
        let (Some(pid), Some(eid), Some(nid), Some(rlen), Some(roff)) =
            (be16(b, rec), be16(b, rec + 2), be16(b, rec + 6), be16(b, rec + 8), be16(b, rec + 10))
        else {
            continue;
        };
        if nid != want {
            continue;
        }
        let (Some(start), Some(end)) = (storage.checked_add(roff as usize), storage.checked_add(roff as usize + rlen as usize)) else {
            continue;
        };
        if end > off + len || end > b.len() {
            continue;
        }
        let raw = &b[start..end];
        if pid == 3 || pid == 0 {
            // UTF-16BE.
            let mut s = String::new();
            for j in (0..raw.len().saturating_sub(1)).step_by(2) {
                let cp = u16::from_be_bytes([raw[j], raw[j + 1]]);
                if let Some(c) = char::from_u32(cp as u32) {
                    s.push(c);
                }
            }
            if !s.is_empty() {
                return Some(s);
            }
        } else if pid == 1 && eid == 0 {
            let s: String = raw.iter().map(|&c| c as char).collect();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Format-12 lookup: sorted groups (start,end,startGlyphID).
fn cmap12_lookup(b: &[u8], off: usize, _len: usize, cp: u32) -> Option<u16> {
    let n = be32(b, off + 12)? as usize;
    let mut lo = 0usize;
    let mut hi = n.min(100_000);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let rec = off + 16 + mid * 12;
        let (Some(start), Some(end), Some(g0)) = (be32(b, rec), be32(b, rec + 4), be32(b, rec + 8)) else {
            return None;
        };
        if cp < start {
            hi = mid;
        } else if cp > end {
            lo = mid + 1;
        } else {
            return u16::try_from(g0 + (cp - start)).ok();
        }
    }
    None
}

/// Format-4 lookup: segment mapping with optional glyphIdArray.
fn cmap4_lookup(b: &[u8], off: usize, _len: usize, cp: u16) -> Option<u16> {
    let seg2 = be16(b, off + 6)? as usize; // segCountX2
    let segs = seg2 / 2;
    let end_codes = off + 14;
    let start_codes = end_codes + seg2 + 2;
    let id_deltas = start_codes + seg2;
    let id_offs = id_deltas + seg2;
    for i in 0..segs.min(4096) {
        let (Some(end), Some(start)) = (be16(b, end_codes + i * 2), be16(b, start_codes + i * 2)) else {
            return None;
        };
        if cp < start || cp > end {
            continue;
        }
        let delta = be16(b, id_deltas + i * 2)?;
        let roff_addr = id_offs + i * 2;
        let roff = be16(b, roff_addr)?;
        if roff == 0 {
            return Some(cp.wrapping_add(delta));
        }
        let glyph_addr = roff_addr + roff as usize + (cp - start) as usize * 2;
        let g = be16(b, glyph_addr)?;
        return Some(if g == 0 { 0 } else { g.wrapping_add(delta) });
    }
    None
}

// ---------- PDF writing ----------

/// One text run inside a page: hex-encoded glyph ids at a baseline
/// position, in a font resource + size. `gids` are Identity-H CIDs.
#[derive(Debug, Clone)]
pub struct TextRun {
    pub font: String,
    pub size: f64,
    pub gids: Vec<u16>,
    /// Baseline x/y in PDF points (origin at bottom-left).
    pub x: f64,
    pub y: f64,
}

/// A filled rectangle (fraction bars, radical overlines, rules).
#[derive(Debug, Clone)]
pub struct FillRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A page under construction.
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub runs: Vec<TextRun>,
    pub rects: Vec<FillRect>,
}

impl Page {
    pub fn new(width: f64, height: f64) -> Self {
        Page { width, height, runs: Vec::new(), rects: Vec::new() }
    }
    /// Positioned glyph run — callers pass glyph ids, not characters.
    pub fn text(&mut self, font: &str, size: f64, x: f64, y: f64, gids: Vec<u16>) {
        self.runs.push(TextRun { font: font.to_string(), size, gids, x, y });
    }
    /// Filled rule.
    pub fn rule(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.rects.push(FillRect { x, y, w, h });
    }
}

/// Document under construction: register fonts once, add pages, save.
pub struct Doc {
    fonts: BTreeMap<String, Ttf>,
    /// font resource name → (ToUnicode map: gid → char).
    uni: BTreeMap<String, BTreeMap<u16, char>>,
    pages: Vec<Page>,
}

impl Doc {
    pub fn new() -> Self {
        Doc { fonts: BTreeMap::new(), uni: BTreeMap::new(), pages: Vec::new() }
    }

    /// Register a TrueType under a resource name ("F1"). The gid→char
    /// map feeds the ToUnicode CMap so text stays copyable.
    pub fn font(&mut self, name: &str, ttf: Ttf, map: BTreeMap<u16, char>) {
        self.fonts.insert(name.to_string(), ttf);
        self.uni.insert(name.to_string(), map);
    }

    pub fn add_page(&mut self, p: Page) {
        self.pages.push(p);
    }

    /// Serialize the document — xref table, trailer, `%PDF-1.5`.
    pub fn save(&self) -> Result<Vec<u8>, PdfError> {
        if self.pages.is_empty() {
            return Err(PdfError("no pages".to_string()));
        }
        // Object layout: 1 catalog, 2 pages-root, then per font:
        //   fontfile, descriptor, cidfont, type0, tounicode
        // then per page: content stream + page dict.
        let nfonts = self.fonts.len();
        let font_base = 3usize;
        let page_base = font_base + nfonts * 5;
        let total = page_base + self.pages.len() * 2;
        let mut objs: Vec<Vec<u8>> = vec![Vec::new(); total + 1]; // 1-indexed

        objs[1] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();

        // Kids list of the Pages node.
        let kids: Vec<String> = (0..self.pages.len()).map(|i| format!("{} 0 R", page_base + i * 2 + 1)).collect();
        objs[2] = format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), self.pages.len()).into_bytes();

        // Fonts.
        let mut font_res = String::new();
        for (i, (name, ttf)) in self.fonts.iter().enumerate() {
            let b = font_base + i * 5;
            let (ff, fd, cid, f0, tu) = (b, b + 1, b + 2, b + 3, b + 4);
            font_res.push_str(&format!("/{name} {f0} 0 R "));

            // FontFile2 — a used-glyph subset when it rebuilds cleanly,
            // the whole TTF otherwise.
            let embedded = self.uni.get(name).and_then(|m| ttf.subset(m)).unwrap_or_else(|| ttf.bytes.clone());
            objs[ff] = stream(&format!("<< /Length {} /Length1 {} >>", embedded.len(), embedded.len()), &embedded);
            // FontDescriptor — metrics in glyph space (1000 upem).
            let upm = ttf.units_per_em as f64;
            let asc = (ttf.ascender as f64 * 1000.0 / upm).round() as i32;
            let desc = (ttf.descender as f64 * 1000.0 / upm).round() as i32;
            objs[fd] = format!(
                "<< /Type /FontDescriptor /FontName /{} /Flags 32 /FontBBox [0 {desc} 1000 {asc}] /Ascent {asc} /Descent {desc} /CapHeight {asc} /ItalicAngle 0 /StemV 80 /FontFile2 {ff} 0 R >>",
                ttf.ps_name
            )
            .into_bytes();
            // CIDFontType2 — CID = glyph id (CIDToGIDMap Identity).
            let w = widths_array(ttf);
            objs[cid] = format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {fd} 0 R /W {w} /CIDToGIDMap /Identity >>",
                ttf.ps_name
            )
            .into_bytes();
            // ToUnicode — bfchar pairs, chunked to the 100-entry limit.
            objs[tu] = tounicode_stream(self.uni.get(name));
            // Type0 wrapper.
            objs[f0] = format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{} /Encoding /Identity-H /DescendantFonts [{cid} 0 R] /ToUnicode {tu} 0 R >>",
                ttf.ps_name
            )
            .into_bytes();
        }

        // Pages + content.
        for (i, page) in self.pages.iter().enumerate() {
            let (content, pobj) = (page_base + i * 2, page_base + i * 2 + 1);
            let body = page_stream(page);
            objs[content] = stream(&format!("<< /Length {} >>", body.len()), &body);
            objs[pobj] = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << {font_res}>> >> /Contents {content} 0 R >>",
                page.width, page.height
            )
            .into_bytes();
        }

        // Serialize: header, objects, xref, trailer.
        let mut out: Vec<u8> = b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = vec![0usize; total + 1];
        for (id, body) in objs.iter().enumerate().skip(1) {
            offsets[id] = out.len();
            let _ = writeln!(out_as_string(&mut out), "{id} 0 obj");
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        let _ = writeln!(out_as_string(&mut out), "xref\n0 {}", total + 1);
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in offsets.iter().skip(1) {
            let _ = writeln!(out_as_string(&mut out), "{off:010} 00000 n ");
        }
        let _ = writeln!(out_as_string(&mut out), "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF", total + 1);
        Ok(out)
    }
}

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

/// Push a formatted ASCII string into a byte buffer — fmt::Write on a
/// Vec<u8> shim to keep writes infallible-looking.
fn out_as_string(out: &mut Vec<u8>) -> OutString<'_> {
    OutString(out)
}

struct OutString<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for OutString<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// `stream` dict header + data + endstream.
fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(dict.len() + data.len() + 24);
    v.extend_from_slice(dict.as_bytes());
    v.extend_from_slice(b"\nstream\n");
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// `/W` array: per-glyph widths in 1000-upem space, as ranges — one
/// `c [w]` run per glyph used is simplest and legal.
fn widths_array(ttf: &Ttf) -> String {
    // Emit every glyph's width; readers accept a full 0..numGlyphs run
    // as `start [w0 w1 …]` chunked under PDF's practical array limits.
    let mut s = String::from("[0 [");
    let upm = ttf.units_per_em as f64;
    for (i, a) in ttf.advances.iter().enumerate() {
        if i > 0 && i % 128 == 0 {
            s.push_str(&format!("] {i} ["));
        } else if i > 0 {
            s.push(' ');
        }
        s.push_str(&(((*a as f64) * 1000.0 / upm).round() as i32).to_string());
    }
    s.push_str("]]");
    s
}

/// ToUnicode CMap stream: bfchar entries gid→UTF-16BE, chunked at 100.
fn tounicode_stream(map: Option<&BTreeMap<u16, char>>) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &char)> = map.map(|m| m.iter().collect()).unwrap_or_default();
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (gid, c) in chunk {
            let mut buf = [0u16; 2];
            let enc = c.encode_utf16(&mut buf);
            let _ = write!(s, "<{gid:04X}> <",);
            for u in enc.iter() {
                let _ = write!(s, "{u:04X}");
            }
            s.push_str(">\n");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    stream(&format!("<< /Length {} >>", s.len()), s.as_bytes())
}

/// Page content stream: rect fills then text runs.
fn page_stream(p: &Page) -> Vec<u8> {
    let mut s = String::new();
    for r in &p.rects {
        let _ = writeln!(s, "{:.3} {:.3} {:.3} {:.3} re f", r.x, r.y, r.w, r.h);
    }
    for t in &p.runs {
        if t.gids.is_empty() {
            continue;
        }
        let mut hex = String::with_capacity(t.gids.len() * 4);
        for g in &t.gids {
            let _ = write!(hex, "{g:04X}");
        }
        let _ = writeln!(s, "BT /{} {:.3} Tf 1 0 0 1 {:.3} {:.3} Tm <{hex}> Tj ET", t.font, t.size, t.x, t.y);
    }
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_shape_is_valid_pdf() {
        // Font-free doc: one page, one rule.
        let mut d = Doc::new();
        let mut p = Page::new(200.0, 100.0);
        p.rule(10.0, 10.0, 50.0, 1.0);
        d.add_page(p);
        let pdf = d.save().unwrap();
        assert!(pdf.starts_with(b"%PDF-1.5"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("startxref"));
        assert!(text.contains("xref\n0 "));
    }

    #[test]
    fn empty_doc_is_an_error() {
        assert!(Doc::new().save().is_err());
    }

    #[test]
    fn hostile_font_bytes_error_not_panic() {
        for junk in [&b""[..], &[0u8; 12][..], &[0xFF; 64][..]] {
            assert!(Ttf::parse(junk).is_err());
        }
    }

    #[test]
    fn real_system_font_parses() {
        for cand in
            [r"C:\Windows\Fonts\arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf"]
        {
            let Ok(bytes) = std::fs::read(cand) else { continue };
            let t = Ttf::parse(&bytes).unwrap();
            assert!(t.units_per_em > 0);
            let gid = t.gid('A');
            assert!(gid > 0, "cmap should map 'A' in {cand}");
            assert!(t.advance(gid) > 0);
            return;
        }
        // No known system font — nothing to assert on this host.
    }

    /// The whole point of the writer: a document with embedded TrueType
    /// must parse in a real PDF reader (hayro-syntax, which the preview
    /// pane's rasterizer sits on).
    #[test]
    fn emitted_pdf_parses_in_a_real_reader() {
        let Some(bytes) =
            [r"C:\Windows\Fonts\arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf"]
                .iter()
                .find_map(|p| std::fs::read(p).ok())
        else {
            return; // no known font on this host — structure test covers the rest
        };
        let ttf = Ttf::parse(&bytes).unwrap();
        let mut map = std::collections::BTreeMap::new();
        let text = "x+y=2";
        let gids: Vec<u16> = text.chars().map(|c| ttf.gid(c)).collect();
        for (i, c) in text.chars().enumerate() {
            if let Some(g) = gids.get(i) {
                map.insert(*g, c);
            }
        }
        let mut doc = Doc::new();
        doc.font("F1", ttf, map);
        let mut page = Page::new(200.0, 100.0);
        page.text("F1", 14.0, 20.0, 50.0, gids);
        page.rule(20.0, 45.0, 60.0, 0.8);
        doc.add_page(page);
        let pdf = doc.save().unwrap();
        let parsed = hayro_syntax::Pdf::new(pdf).unwrap_or_else(|e| panic!("our PDF should parse in hayro: {e:?}"));
        assert_eq!(parsed.pages().len(), 1);
    }

    /// A subset must re-parse, map the same chars to the same advances
    /// (renumbered gids), keep .notdef, and shrink dramatically.
    #[test]
    fn subset_renumbers_and_reuses() {
        let Some(bytes) =
            ["C:\\Windows\\Fonts\\arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf"]
                .iter()
                .find_map(|p| std::fs::read(p).ok())
        else {
            return;
        };
        let ttf = Ttf::parse(&bytes).unwrap();
        let chars: Vec<char> = "Hello, World!0123".chars().collect();
        let used: BTreeMap<u16, char> = chars.iter().map(|&c| (ttf.gid(c), c)).collect();
        let sub_bytes = ttf.subset(&used).unwrap();
        assert!(sub_bytes.len() < bytes.len() / 4, "{} vs {}", sub_bytes.len(), bytes.len());
        let sub = Ttf::parse(&sub_bytes).unwrap();
        for &c in &chars {
            let old_gid = ttf.gid(c);
            if old_gid == 0 {
                continue;
            }
            let new_gid = sub.gid(c);
            assert!(new_gid > 0, "{c:?} unmapped in subset");
            // Advance must travel with the glyph.
            assert_eq!(sub.advance(new_gid), ttf.advance(old_gid), "{c:?} advance changed");
        }
        // Whole-PDF round trip with a subsetted font still parses.
        let mut doc = Doc::new();
        let mut page = Page::new(200.0, 100.0);
        let gids: Vec<u16> = "Hi".chars().map(|c| sub.gid(c)).collect();
        page.text("F1", 12.0, 10.0, 50.0, gids);
        doc.font("F1", ttf, used);
        doc.add_page(page);
        let pdf = doc.save().unwrap();
        assert_eq!(hayro_syntax::Pdf::new(pdf).unwrap().pages().len(), 1);
    }
}
