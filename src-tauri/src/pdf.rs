//! The report PDF. `docs/roadmap/REDACTION.md` states the leak report is the
//! artifact an auditor reads, and that the console exports it as a PDF the user
//! may protect with a password. This module lays out that page: a title, one
//! row per entry, and a page break wherever the rows stop fitting.
//!
//! Rows print their values in full, because the report is an exact record and
//! not a redacted one. Text carries `WinAnsiEncoding`, the encoding the
//! standard-14 fonts ship with, so a Windows path or an accented name survives;
//! a code point with no byte in that encoding leaves as `?`.
//!
//! `lopdf` writes the object layer, the cross-reference table, and the standard
//! security handler. A file written with a password uses revision 6: AES-256
//! under a random 256-bit file key that a key derived from the password wraps.
//! Its strings and streams are ciphertext on disk, so the file opens with that
//! password and with no other.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::encryption::{EncryptionState, EncryptionVersion, Permissions};
use lopdf::{dictionary, Dictionary, Document, Object, Stream, StringFormat};
use windows::core::Error;
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

/// ISO 32000-2 §7.6.4.4.1: a password contributes at most 127 bytes. A longer
/// password opens the file by its first 127 bytes only, so this module refuses
/// the longer password instead.
const MAX_PASSWORD: usize = 127;

// The page is A4 portrait, in points.
const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const SIDE: f32 = 36.0;
const TOP: f32 = 44.0;
/// Rows stop above this line, so the footer band stays clear.
const FOOT: f32 = 46.0;
const FOOT_Y: f32 = 24.0;
const TITLE: f32 = 15.0;
const HEAD: f32 = 8.5;
const CELL: f32 = 8.0;
const LEAD: f32 = 9.6;
/// Air above and below the text of a row.
const PAD: f32 = 3.0;
/// Courier advances 0.6 em for every byte. Helvetica-Bold has no width table
/// here, so its advance is estimated high enough that a header never overruns
/// the column it was measured against.
const MONO: f32 = 0.6;
const BOLD: f32 = 0.58;
/// Where a baseline sits inside a line box.
const ASCENT: f32 = 0.78;

fn why(error: Error) -> String {
    format!("0x{:08X}", error.code().0 as u32)
}

fn random(bytes: &mut [u8]) -> Result<(), String> {
    unsafe { BCryptGenRandom(None, bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(why)
}

/// A strong password for a report the user typed no password for: 144 random
/// bits over an alphabet that survives reading aloud, copying, and typing.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(crate) fn password() -> String {
    let mut bytes = [0u8; 18];
    // A machine that cannot fill 18 random bytes cannot seal a store either,
    // and a guessed password is worse than no file at all.
    random(&mut bytes).unwrap_or_else(|e| panic!("BCryptGenRandom: {e}"));
    let mut out = String::with_capacity(24);
    let mut bits = 0u32;
    let mut pool = 0u32;
    for &byte in &bytes {
        pool = (pool << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(char::from(ALPHABET[((pool >> bits) & 63) as usize]));
        }
    }
    out
}

// ------------------------------------------------------------------ text ----

/// The bytes `WinAnsiEncoding` names. A code point the encoding has no byte for
/// leaves as `?`, and a control character leaves as a space, except the newline,
/// which `break_lines` turns into a line break.
fn ansi(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        out.push(match ch as u32 {
            0x0a => b'\n',
            0x20..=0x7f => ch as u8,
            0xa0..=0xff => ch as u8,
            code => CP1252
                .iter()
                .find(|&&(point, _)| point == code)
                .map_or(b'?', |&(_, byte)| byte),
        });
    }
    out
}

/// The Windows-1252 bytes above Latin-1, as the code point each one names.
const CP1252: &[(u32, u8)] = &[
    (0x20ac, 0x80),
    (0x201a, 0x82),
    (0x0192, 0x83),
    (0x201e, 0x84),
    (0x2026, 0x85),
    (0x2020, 0x86),
    (0x2021, 0x87),
    (0x02c6, 0x88),
    (0x2030, 0x89),
    (0x0160, 0x8a),
    (0x2039, 0x8b),
    (0x0152, 0x8c),
    (0x017d, 0x8e),
    (0x2018, 0x91),
    (0x2019, 0x92),
    (0x201c, 0x93),
    (0x201d, 0x94),
    (0x2022, 0x95),
    (0x2013, 0x96),
    (0x2014, 0x97),
    (0x02dc, 0x98),
    (0x2122, 0x99),
    (0x0161, 0x9a),
    (0x203a, 0x9b),
    (0x0153, 0x9c),
    (0x017e, 0x9e),
    (0x0178, 0x9f),
];

/// A PDF literal string. The delimiters and the escape itself carry a backslash,
/// and any byte a reader could take for a delimiter or a line terminator carries
/// an octal escape.
fn literal(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'(');
    for &byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            0x20..=0x7e => out.push(byte),
            _ => {
                out.push(b'\\');
                out.push(b'0' + byte / 64);
                out.push(b'0' + (byte / 8) % 8);
                out.push(b'0' + byte % 8);
            }
        }
    }
    out.push(b')');
    out
}

/// UTF-16BE behind a byte-order mark, so a title keeps every code point.
fn utf16be(text: &str) -> Vec<u8> {
    let mut out = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Greedy word wrap over the bytes a column holds. A word longer than the
/// column splits at the column edge, because a leaked path or token has no
/// space to break at and still has to print in full.
fn break_lines(text: &[u8], width: usize) -> Vec<Vec<u8>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<u8>> = Vec::new();
    for part in text.split(|&byte| byte == b'\n') {
        let mut line: Vec<u8> = Vec::new();
        for word in part
            .split(|&byte| byte == b' ')
            .filter(|word| !word.is_empty())
        {
            let mut rest = word;
            loop {
                let gap = usize::from(!line.is_empty());
                if line.len() + gap + rest.len() <= width {
                    if gap == 1 {
                        line.push(b' ');
                    }
                    line.extend_from_slice(rest);
                    break;
                }
                if line.is_empty() {
                    line.extend_from_slice(&rest[..width]);
                    rest = &rest[width..];
                    if rest.is_empty() {
                        break;
                    }
                } else {
                    lines.push(std::mem::take(&mut line));
                }
            }
        }
        lines.push(line);
    }
    if lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

/// The civil date and clock of one Unix second in UTC: year, month, day, hour,
/// minute, second. No chrono lives in this crate, so the civil-from-days split
/// runs here, and both the Info dictionary and the `Sent` column read it.
fn civil(now: u64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = i64::try_from(now).unwrap_or_default();
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = (if z < 0 { z - 146_096 } else { z }).div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, time / 3600, (time / 60) % 60, time % 60)
}

/// `D:YYYYMMDDHHmmSSZ` for the Info dictionary.
fn stamp(now: u64) -> String {
    let (year, month, day, hour, minute, second) = civil(now);
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z")
}

/// The `Sent` column: the instant of one send, in UTC, so a reader who is not
/// in this time zone can still place it.
pub(crate) fn clock(now: u64) -> String {
    let (year, month, day, hour, minute, second) = civil(now);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

// ---------------------------------------------------------------- layout ----

/// One page of the table: its content stream, and the band of page rows still
/// have left.
struct Page {
    ops: Vec<u8>,
    /// The top of the row band, just under the column headers.
    top: f32,
    cursor: f32,
}

struct Sheet {
    title: Vec<Vec<u8>>,
    head: Vec<Vec<Vec<u8>>>,
    head_h: f32,
    /// Column width in cell characters.
    cols: Vec<usize>,
    /// The left edge of each column.
    x: Vec<f32>,
    right: f32,
    pages: Vec<Page>,
}

impl Sheet {
    /// Measure the table before any row lands. The columns take the page in
    /// proportion to what they hold, and everything wraps at its own width.
    fn new(title: &str, columns: &[&str], grid: &[Vec<Vec<u8>>]) -> Sheet {
        let chars = ((PAGE_W - 2.0 * SIDE) / (CELL * MONO)).floor() as usize;
        let mut cols: Vec<usize> = columns
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut want = name.len().clamp(4, 24);
                for row in grid {
                    want = want.max(row.get(i).map_or(0, Vec::len).min(48));
                }
                want
            })
            .collect();
        let mut sum: usize = cols.iter().sum();
        while sum > chars {
            let widest = cols
                .iter()
                .enumerate()
                .max_by_key(|(_, &col)| col)
                .map(|(i, _)| i);
            let Some(widest) = widest else { break };
            if cols[widest] <= 1 {
                break;
            }
            cols[widest] -= 1;
            sum -= 1;
        }
        if sum < chars && !cols.is_empty() {
            let each = (chars - sum) / cols.len();
            for col in &mut cols {
                *col += each;
            }
            cols[0] += (chars - sum) % cols.len();
        }
        let mut x = Vec::with_capacity(cols.len());
        let mut left = SIDE;
        for &col in &cols {
            x.push(left);
            left += col as f32 * CELL * MONO;
        }
        let head: Vec<Vec<Vec<u8>>> = columns
            .iter()
            .enumerate()
            .map(|(i, name)| break_lines(&ansi(name), cells_of(cols[i], HEAD)))
            .collect();
        let head_h = head.iter().map(Vec::len).max().unwrap_or(1) as f32 * LEAD + 2.0 * PAD;
        Sheet {
            title: break_lines(
                &ansi(title),
                ((PAGE_W - 2.0 * SIDE) / (TITLE * BOLD)).floor() as usize,
            ),
            head,
            head_h,
            cols,
            x,
            right: left,
            pages: Vec::new(),
        }
    }

    /// Start a page. The title sits on the first one only.
    fn open(&mut self) {
        let numbered = !self.pages.is_empty();
        let mut page = Page {
            ops: Vec::new(),
            top: PAGE_H - TOP,
            cursor: PAGE_H - TOP,
        };
        if !numbered {
            for (k, line) in self.title.iter().enumerate() {
                show(
                    &mut page.ops,
                    "F1",
                    TITLE,
                    SIDE,
                    page.cursor - TITLE * ASCENT - k as f32 * (TITLE + 4.0),
                    line,
                );
            }
            page.cursor -= self.title.len().max(1) as f32 * (TITLE + 4.0) + 8.0;
        }
        self.head_block(&mut page);
        self.pages.push(page);
    }

    /// The column header row, which repeats on every page so a loose page still
    /// names its columns.
    fn head_block(&mut self, page: &mut Page) {
        let h = self.head_h;
        fill(&mut page.ops, SIDE, page.cursor - h, self.right - SIDE, h);
        for (i, lines) in self.head.iter().enumerate() {
            for (k, line) in lines.iter().enumerate() {
                let y = page.cursor - PAD - HEAD * ASCENT - k as f32 * LEAD;
                show(&mut page.ops, "F1", HEAD, self.x[i] + 2.0, y, line);
            }
        }
        rule(
            &mut page.ops,
            SIDE,
            page.cursor - h,
            self.right,
            page.cursor - h,
        );
        page.cursor -= h;
        page.top = page.cursor;
    }

    /// One row, on a fresh page once it stops fitting above the footer band.
    fn row(&mut self, cells: &[Vec<u8>]) {
        if self.pages.is_empty() {
            self.open();
        }
        // One character of gutter, so a long value never touches the rule that
        // divides two columns.
        let lines: Vec<Vec<Vec<u8>>> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| break_lines(cell, self.cols[i].saturating_sub(1).max(1)))
            .collect();
        let height = lines.iter().map(Vec::len).max().unwrap_or(1) as f32 * LEAD + 2.0 * PAD;
        if self
            .pages
            .last()
            .is_some_and(|page| page.cursor - height < FOOT)
        {
            self.close();
            self.open();
        }
        let page = self.pages.last_mut().expect("one open page");
        for (i, lines) in lines.iter().enumerate() {
            for (k, line) in lines.iter().enumerate() {
                let y = page.cursor - PAD - CELL * ASCENT - k as f32 * LEAD;
                show(&mut page.ops, "F2", CELL, self.x[i] + 2.0, y, line);
            }
        }
        page.cursor -= height;
        rule(&mut page.ops, SIDE, page.cursor, self.right, page.cursor);
    }

    /// Close the open page: the column rules span the rows that landed on it.
    fn close(&mut self) {
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        let (top, bottom) = (page.top, page.cursor);
        for i in 0..=self.x.len() {
            let x = self.x.get(i).copied().unwrap_or(self.right);
            rule(&mut page.ops, x, bottom, x, top);
        }
        rule(&mut page.ops, SIDE, top, self.right, top);
    }

    /// Name each page, now that the count is known.
    fn finish(&mut self, title: &str, total: usize) {
        self.close();
        for (i, page) in self.pages.iter_mut().enumerate() {
            let label = ansi(&format!("{title}  -  page {} of {total}", i + 1));
            let x = ((PAGE_W - label.len() as f32 * CELL * MONO) / 2.0).max(SIDE);
            show(&mut page.ops, "F2", CELL, x, FOOT_Y, &label);
        }
    }
}

/// How many cell bytes fit where one header line of `size` would fit the same
/// column.
fn cells_of(col: usize, size: f32) -> usize {
    (col as f32 * CELL * MONO / (size * BOLD)).floor() as usize
}

fn show(out: &mut Vec<u8>, font: &str, size: f32, x: f32, y: f32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    out.extend_from_slice(format!("BT /{font} {size:.1} Tf 1 0 0 1 {x:.2} {y:.2} Tm ").as_bytes());
    out.extend_from_slice(&literal(bytes));
    out.extend_from_slice(b" Tj ET\n");
}

fn rule(out: &mut Vec<u8>, x1: f32, y1: f32, x2: f32, y2: f32) {
    out.extend_from_slice(
        format!("0.6 w 0.45 G {x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S\n").as_bytes(),
    );
}

fn fill(out: &mut Vec<u8>, x: f32, y: f32, w: f32, h: f32) {
    out.extend_from_slice(format!("0.93 g {x:.2} {y:.2} {w:.2} {h:.2} re f 0 g\n").as_bytes());
}

// ------------------------------------------------------------- document ----

/// One row per entry, in the encoding the fonts carry, trimmed and padded to
/// the column count.
fn encode(columns: &[&str], rows: &[Vec<String>]) -> Vec<Vec<Vec<u8>>> {
    rows.iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| ansi(row.get(i).map_or("", String::as_str)))
                .collect()
        })
        .collect()
}

/// A standard-14 font with the encoding the page text carries.
fn font(name: &str) -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => name,
        "Encoding" => "WinAnsiEncoding",
    }
}

pub(crate) fn table(
    title: &str,
    columns: &[&str],
    rows: &[Vec<String>],
    password: Option<&str>,
) -> Result<Vec<u8>, String> {
    if let Some(secret) = password {
        if secret.len() > MAX_PASSWORD {
            return Err(format!(
                "a PDF password holds {MAX_PASSWORD} bytes, this one holds {}",
                secret.len()
            ));
        }
    }
    let grid = encode(columns, rows);
    let mut sheet = Sheet::new(title, columns, &grid);
    for row in &grid {
        sheet.row(row);
    }
    if sheet.pages.is_empty() {
        sheet.open();
    }
    let total = sheet.pages.len();
    sheet.finish(title, total);

    let mut doc = Document::with_version("1.7");
    let bold = doc.add_object(font("Helvetica-Bold"));
    let mono = doc.add_object(font("Courier"));
    let pages = doc.add_object(
        dictionary! { "Type" => "Pages", "Kids" => Object::Array(Vec::new()), "Count" => 0 },
    );
    let mut kids: Vec<Object> = Vec::with_capacity(total);
    for page in &sheet.pages {
        let content = doc.add_object(Stream::new(dictionary! {}, page.ops.clone()));
        let id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages),
            "MediaBox" => Object::Array(vec![0.into(), PAGE_W.into(), PAGE_H.into(), 0.into()]),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(bold), "F2" => Object::Reference(mono) },
            },
            "Contents" => Object::Reference(content),
        });
        kids.push(Object::Reference(id));
    }
    let tree = doc.get_dictionary_mut(pages).map_err(|e| e.to_string())?;
    tree.set("Kids", Object::Array(kids));
    tree.set("Count", i64::try_from(total).unwrap_or_default());

    let catalog =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages) });
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let info = doc.add_object(dictionary! {
        "Producer" => Object::String(b"First Mate".to_vec(), StringFormat::Literal),
        "CreationDate" => Object::String(stamp(now).into_bytes(), StringFormat::Literal),
        "Title" => Object::String(utf16be(title), StringFormat::Hexadecimal),
    });
    doc.trailer.set("Root", Object::Reference(catalog));
    doc.trailer.set("Info", Object::Reference(info));

    if let Some(secret) = password {
        let mut key = [0u8; 32];
        random(&mut key)?;
        let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
        let state = EncryptionState::try_from(EncryptionVersion::V5 {
            encrypt_metadata: true,
            crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
            file_encryption_key: &key,
            stream_filter: b"StdCF".to_vec(),
            string_filter: b"StdCF".to_vec(),
            owner_password: secret,
            user_password: secret,
            permissions: Permissions::all(),
        })
        .map_err(|e| e.to_string())?;
        doc.encrypt(&state).map_err(|e| e.to_string())?;
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<&'static str> {
        vec![
            "When",
            "Conversation",
            "Value",
            "Class",
            "Provider",
            "Model",
        ]
    }

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec![
                "2026-10-12 09:14".to_string(),
                "Contract review".to_string(),
                "C:\\Users\\Renée\\Desktop\\api_key=sk-live-0p3n.txt".to_string(),
                "api-key".to_string(),
                "openai".to_string(),
                "gpt-4o".to_string(),
            ],
            vec![
                "2026-10-12 09:20".to_string(),
                "Résumé (draft, final)".to_string(),
                "4111 1111 1111 1111".to_string(),
                "card".to_string(),
                "lm-studio".to_string(),
                "qwen2.5:7b-instruct".to_string(),
            ],
        ]
    }

    #[test]
    fn a_literal_string_escapes_its_delimiters() {
        assert_eq!(literal(b"(a)b\\c\x01"), b"(\\(a\\)b\\\\c\\001)".to_vec());
        assert_eq!(literal(b"ok"), b"(ok)".to_vec());
    }

    #[test]
    fn ansi_keeps_what_winansi_names() {
        assert_eq!(ansi("Renée"), vec![b'R', b'e', b'n', 0xe9, b'e']);
        assert_eq!(ansi("C:\\temp"), br"C:\temp".to_vec());
        assert_eq!(ansi("quote — ok"), b"quote \x97 ok".to_vec());
        assert_eq!(ansi("marker ⚠"), b"marker ?".to_vec());
    }

    #[test]
    fn break_lines_splits_a_long_token_and_keeps_every_byte() {
        let text = br"C:\Users\dusti\secrets.txt".to_vec();
        let lines = break_lines(&text, 12);
        assert!(lines.len() > 2, "a 27 byte path splits");
        assert!(lines.iter().all(|line| line.len() <= 12));
        assert_eq!(lines.concat(), text);
    }

    #[test]
    fn stamp_reads_the_epoch_and_a_leap_day() {
        assert_eq!(stamp(0), "D:19700101000000Z");
        assert_eq!(stamp(1_728_000_000), "D:20241004000000Z");
        assert_eq!(stamp(951_782_400), "D:20000229000000Z");
    }

    #[test]
    fn clock_names_the_instant_in_utc() {
        assert_eq!(clock(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(clock(1_760_000_000), "2025-10-09 08:53:20 UTC");
    }

    #[test]
    fn a_plain_table_prints_the_values() {
        let pdf = table("Leak report", &columns(), &rows(), None).expect("plain table");
        let doc = Document::load_mem(&pdf).expect("a reader opens the file");
        assert_eq!(doc.get_pages().len(), 1, "one page");
        assert!(!doc.is_encrypted(), "a plain table carries no handler");
        let text = doc.extract_text(&[1]).expect("the text of page 1");
        assert!(text.contains("Leak report"), "title");
        assert!(
            text.contains("Renée"),
            "an accented name survives WinAnsiEncoding"
        );
        // A cell wraps at its column width, so the path reads across lines.
        let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            flat.contains(r"C:\Users\Renée\Desktop\api_key=sk-live-0p3n.txt"),
            "a Windows path"
        );
        assert!(text.contains("4111 1111 1111 1111"), "the value in full");
        assert!(text.contains("page 1 of 1"), "footer");
    }

    /// Every value reaches the file as a string or a stream of one document, so
    /// a password that works leaves nothing of the report in the clear.
    #[test]
    fn a_password_table_hides_every_value() {
        let pdf = table("Leak report", &columns(), &rows(), Some("correct horse"))
            .expect("encrypted table");
        let text = String::from_utf8_lossy(&pdf).into_owned();
        for clear in [
            "sk-live",
            "Contract review",
            "gpt-4o",
            "Leak report",
            "api-key",
            "qwen2.5",
        ] {
            assert!(!text.contains(clear), "{clear} left the file in the clear");
        }
        let doc = Document::load_mem(&pdf).expect("a reader opens the file");
        assert!(doc.is_encrypted(), "the export names its handler");
    }

    /// ISO 32000-2 lets a reader ignore a password past 127 bytes, so a longer
    /// password must not reach a reader that would open on a prefix of it.
    #[test]
    fn a_password_longer_than_the_format_allows_is_refused() {
        let one = vec![vec!["x".to_string()]];
        let columns = ["Value"];
        assert!(
            table("Report", &columns, &one, Some(&"x".repeat(MAX_PASSWORD))).is_ok(),
            "127 bytes pass"
        );
        let long = "x".repeat(MAX_PASSWORD + 1);
        let why = table("Report", &columns, &one, Some(&long)).expect_err("128 bytes fail");
        assert!(why.contains("127"), "the refusal names the limit: {why}");
    }

    #[test]
    fn a_long_table_paginates() {
        let columns = vec!["#", "Class", "Value"];
        let rows: Vec<Vec<String>> = (0..400)
            .map(|i| {
                vec![
                    format!("{i:03}"),
                    "api-key".to_string(),
                    format!("sk-live-{i:0>8}"),
                ]
            })
            .collect();
        let pdf = table("Long report", &columns, &rows, None).expect("long table");
        let doc = Document::load_mem(&pdf).expect("a reader opens the file");
        let pages = doc.get_pages();
        assert!(pages.len() > 4, "{} pages for 400 rows", pages.len());
        let last = *pages.keys().next_back().expect("the last page");
        let text = doc
            .extract_text(&[last])
            .expect("the text of the last page");
        assert!(
            text.contains(&format!("page {last} of {}", pages.len())),
            "the footer names the count"
        );
        assert!(
            text.contains(&format!("sk-live-{:0>8}", 399)),
            "the last row survives"
        );
    }

    #[test]
    fn a_generated_password_is_long_and_its_own() {
        let first = password();
        let second = password();
        assert_eq!(first.len(), 24, "144 bits at six bits a character");
        assert!(first.len() >= 20);
        assert_ne!(first, second, "two draws differ");
        assert!(
            first.chars().all(|c| c.is_ascii() && !c.is_whitespace()),
            "typeable anywhere"
        );
    }

    /// The independent reader check needs the bytes on disk. Run it with
    /// `cargo test -- --ignored`, then read the paths it prints.
    #[test]
    #[ignore = "writes samples for the reader check"]
    fn writes_samples_for_the_reader_check() {
        let dir = std::env::temp_dir().join("fm-pdf-check");
        std::fs::create_dir_all(&dir).expect("sample directory");
        let secret = "Sk-Report-2026-Passphrase";
        // The column set `lib.rs` exports, so a sample is the real report, and
        // the `Sent` column comes from `clock`.
        let columns = [
            "Sent",
            "Conversation",
            "Value",
            "Class",
            "Provider",
            "Model",
            "Base URL",
            "Carried by",
        ];
        let at = 1_760_000_000u64;
        let rows = vec![
            vec![
                clock(at),
                "Contract review".to_string(),
                "C:\\Users\\Renée\\Desktop\\api_key=sk-live-0p3n.txt".to_string(),
                "api-key".to_string(),
                "openai".to_string(),
                "gpt-4o".to_string(),
                "https://api.openai.com/v1".to_string(),
                "message".to_string(),
            ],
            vec![
                clock(at + 3_720),
                "Résumé (draft, final)".to_string(),
                "4111 1111 1111 1111".to_string(),
                "card".to_string(),
                "lm-studio".to_string(),
                "qwen2.5:7b-instruct".to_string(),
                "http://localhost:1234/v1".to_string(),
                "read_file".to_string(),
            ],
        ];
        let plain = table("First Mate leak report", &columns, &rows, None).unwrap();
        let locked = table("First Mate leak report", &columns, &rows, Some(secret)).unwrap();
        let columns = vec!["#", "Class", "Value"];
        let rows: Vec<Vec<String>> = (0..260)
            .map(|i| {
                vec![
                    format!("{i:03}"),
                    "api-key".to_string(),
                    format!("sk-live-{i:0>8}"),
                ]
            })
            .collect();
        let long = table("Long report", &columns, &rows, Some(secret)).unwrap();
        std::fs::write(dir.join("plain.pdf"), plain).unwrap();
        std::fs::write(dir.join("locked.pdf"), locked).unwrap();
        std::fs::write(dir.join("long.pdf"), long).unwrap();
        std::fs::write(dir.join("password.txt"), secret).unwrap();
        println!("samples in {}", dir.display());
    }
}
