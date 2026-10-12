//! Sensitive-info redaction. `docs/roadmap/REDACTION.md` is the design.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use regex::Regex;
use sha2::{Digest, Sha256};

/// A model that mangles a token still copies the pair.
const OPEN: char = '⟦';
const CLOSE: char = '⟧';

const MAX_TOKEN_CHARS: usize = 48;

static RE_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"⟦[^⟧]{1,48}⟧").unwrap());

/// The refusal names the key, because copying the handle back into the
/// transcript gives the model another handle to reuse.
pub(crate) fn stray_argument(args: &serde_json::Value) -> Option<String> {
    match args {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, value)| {
            if !has_token(value) {
                return None;
            }
            let inner = stray_argument(value).filter(|k| !k.is_empty());
            Some(inner.unwrap_or_else(|| key.clone()))
        }),
        serde_json::Value::Array(items) => items.iter().find_map(stray_argument),
        other => has_token(other).then(String::new),
    }
}

fn has_token(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => RE_TOKEN.is_match(s),
        serde_json::Value::Object(map) => map.values().any(has_token),
        serde_json::Value::Array(items) => items.iter().any(has_token),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Class {
    Card,
    Iban,
    Aba,
    Swift,
    Ssn,
    Crypto,
    ApiKey,
    Jwt,
    Email,
    Phone,
    UserPath,
    /// A US account number is 8 to 17 digits and is indistinguishable from an
    /// order id.
    Entity,
}

impl Class {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Class::Card => "CARD",
            Class::Iban => "IBAN",
            Class::Aba => "ABA",
            Class::Swift => "SWIFT",
            Class::Ssn => "SSN",
            Class::Crypto => "CRYPTO",
            Class::ApiKey => "APIKEY",
            Class::Jwt => "JWT",
            Class::Email => "EMAIL",
            Class::Phone => "PHONE",
            Class::UserPath => "USER_PATH",
            Class::Entity => "ENTITY",
        }
    }

    /// Text that two detectors claim goes to the earlier class: an IBAN holds
    /// an 18-digit run that passes Luhn as an Amex. A JWT holds a payload that
    /// can hold an email.
    fn rank(self) -> u8 {
        match self {
            Class::Entity => 0,
            Class::Jwt => 1,
            Class::Iban => 2,
            Class::Card => 3,
            Class::Swift => 4,
            Class::Crypto => 5,
            Class::ApiKey => 6,
            Class::UserPath => 7,
            Class::Aba => 8,
            Class::Ssn => 9,
            Class::Email => 10,
            Class::Phone => 11,
        }
    }

    const ALL: &'static [Class] = &[
        Class::Jwt,
        Class::Iban,
        Class::Card,
        Class::Swift,
        Class::Crypto,
        Class::ApiKey,
        Class::UserPath,
        Class::Aba,
        Class::Ssn,
        Class::Email,
        Class::Phone,
        Class::Entity,
    ];
}

/// ISO 3166-1 alpha-2. A BIC's country must be one of these codes. The country
/// check separates `DEUTDEFF` from the word `CORRESPONDS`.
const ISO_CC: &str = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ \
 BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE \
 DJ DK DM DO DZ EC EE EG ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS \
 GT GU GW GY HK HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW \
 KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV \
 MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA \
 RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ \
 TK TL TM TN TO TR TT TV TW TZ UA UG US UY UZ VA VC VE VG VI VU WF WS YE YT ZA ZM ZW XK";

fn is_country(cc: &str) -> bool {
    ISO_CC.split(' ').any(|c| c == cc)
}

/// An unlisted country has no length to check, so layer 1 refuses the match
/// rather than guess.
fn iban_len(cc: &str) -> Option<usize> {
    const TABLE: &[(&str, usize)] = &[
        ("AD", 24), ("AE", 23), ("AL", 28), ("AT", 20), ("AZ", 28), ("BA", 20), ("BE", 16),
        ("BG", 22), ("BH", 22), ("BR", 29), ("CH", 21), ("CR", 22), ("CY", 28), ("CZ", 24),
        ("DE", 22), ("DK", 18), ("DO", 28), ("EE", 20), ("EG", 29), ("ES", 24), ("FI", 18),
        ("FO", 24), ("FR", 27), ("GB", 22), ("GE", 22), ("GI", 23), ("GR", 27), ("GT", 28),
        ("HR", 19), ("HU", 28), ("IE", 22), ("IL", 23), ("IS", 26), ("IT", 27), ("JO", 30),
        ("KW", 30), ("KZ", 20), ("LB", 28), ("LI", 21), ("LT", 20), ("LU", 20), ("LV", 21),
        ("MC", 27), ("MD", 24), ("ME", 22), ("MK", 19), ("MT", 31), ("MU", 30), ("NL", 18),
        ("NO", 15), ("PK", 24), ("PL", 28), ("PS", 29), ("PT", 25), ("QA", 29), ("RO", 24),
        ("RS", 22), ("SA", 24), ("SC", 31), ("SE", 24), ("SI", 19), ("SK", 24), ("SM", 27),
        ("SV", 28), ("TL", 23), ("TN", 24), ("TR", 26), ("UA", 29), ("VA", 22), ("VG", 24),
        ("XK", 20),
    ];
    TABLE
        .iter()
        .find(|(c, _)| *c == cc)
        .map(|(_, n)| *n)
}

static RE_CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d[ -]?){12,18}\d\b").unwrap());
static RE_IBAN: LazyLock<Regex> = LazyLock::new(|| {
    // A printed IBAN uses groups of exactly four. A group of one to four would
    // let the tail of the next word join the match. The length check would then
    // reject a real IBAN.
    Regex::new(
        r"(?i)\b[A-Z]{2}\d{2}(?:[A-Z0-9]{8,30}|(?: [A-Z0-9]{4}){1,6}(?: [A-Z0-9]{1,4})?)\b",
    )
    .unwrap()
});
static RE_ABA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{9}\b").unwrap());
static RE_SWIFT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{4}[A-Z]{2}[0-9A-Z]{2}(?:[0-9A-Z]{3})?\b").unwrap());
static RE_SSN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap());
static RE_B58: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[13][a-km-zA-HJ-NP-Z1-9]{25,34}\b").unwrap());
static RE_BECH32: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bbc1[a-z0-9]{11,90}\b").unwrap());
static RE_JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}\b").unwrap()
});
static RE_API_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:(?:sk-ant-[A-Za-z0-9_-]{20,}|sk-proj-[A-Za-z0-9_-]{20,}",
        r"|sk-[A-Za-z0-9_-]{20,}|sk_(?:live|test)_[A-Za-z0-9]{20,}",
        r"|pk_(?:live|test)_[A-Za-z0-9]{16,}|gh[pousr]_[A-Za-z0-9]{36}",
        r"|github_pat_[A-Za-z0-9_]{22,}|xox[baprs]-[A-Za-z0-9-]{10,}",
        r"|(?:AK|AS)IA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|ya29\.[A-Za-z0-9_-]{24,}",
        r"|SG\.[A-Za-z0-9_-]{16,40}\.[A-Za-z0-9_-]{16,40}|glpat-[A-Za-z0-9_-]{20}",
        r"|npm_[A-Za-z0-9]{36}|hf_[A-Za-z0-9]{34}|dop_v1_[a-f0-9]{64}",
        r"|sq0(?:atp|csp)-[A-Za-z0-9_-]{20,}|SK[a-f0-9]{32})\b",
        r"|(?i:bearer)[ =]\x22?[A-Za-z0-9._~+/=-]{24,})"
    ))
    .unwrap()
});
static RE_EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,24}\b").unwrap());
static RE_PHONE_INTL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+\d{1,3}(?:[ .-]?\d){6,15}\b").unwrap());
static RE_PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\(?\d{3}\)?[ .-]\d{3}[ .-]\d{4}\b").unwrap());
/// `docs/roadmap/REDACTION.md` names this root, because the root holds mail
/// stores, browser profiles, and credential files.
static RE_USER_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b[A-Z]:\\+Users\\+[^\\/:*?"<>|\r\n]+(?:\\+[^\\/:*?"<>|\r\n]+)*"#).unwrap()
});
static RE_PROFILE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:%USERPROFILE%|\$env:USERPROFILE)(?:\\+[^\\/:*?"<>|\r\n]+)*"#).unwrap()
});

const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const BECH32_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

/// A Luhn-valid digit run outside these ranges is an order id or a tracking
/// number.
fn known_bin(digits: &str) -> bool {
    let p2: u32 = digits[..2].parse().unwrap_or(99);
    let p3: u32 = digits[..3].parse().unwrap_or(999);
    let p4: u32 = digits[..4].parse().unwrap_or(9999);
    p2 == 34
        || p2 == 37
        || p2 == 62
        || (300..=305).contains(&p3)
        || (2221..=2720).contains(&p4)
        || (3528..=3589).contains(&p4)
        || p4 == 6011
        || (6440..=6499).contains(&p4)
        || (51..=55).contains(&p2)
        || digits.starts_with('4')
}

/// ISO 13616 mod-97-10: move the four leading characters to the end, expand
/// letters to two digits, and require a remainder of 1.
fn mod97_ok(normalized: &str) -> bool {
    if normalized.len() < 8 {
        return false;
    }
    let rotated = format!("{}{}", &normalized[4..], &normalized[..4]);
    let mut rem = 0u32;
    for c in rotated.chars() {
        if c.is_ascii_digit() {
            rem = (rem * 10 + c as u32 - '0' as u32) % 97;
        } else {
            let v = 10 + c.to_ascii_uppercase() as u32 - 'A' as u32;
            rem = (rem * 100 + v) % 97;
        }
    }
    rem == 1
}

/// ABA routing numbers embed weights 3-7-1 in their own digits.
fn aba_ok(digits: &str) -> bool {
    let mut d = [0u32; 9];
    for (i, c) in digits.chars().enumerate() {
        if i >= 9 {
            return false;
        }
        match c.to_digit(10) {
            Some(v) => d[i] = v,
            None => return false,
        }
    }
    (3 * (d[0] + d[3] + d[6]) + 7 * (d[1] + d[4] + d[7]) + (d[2] + d[5] + d[8])) % 10 == 0
}

/// SSN validity is positional: the area, group, and serial each rule out
/// values that no issued number uses.
fn ssn_ok(ssn: &str) -> bool {
    let area: u32 = ssn[..3].parse().unwrap_or(999);
    let group: u32 = ssn[4..6].parse().unwrap_or(99);
    let serial: u32 = ssn[7..].parse().unwrap_or(9999);
    area != 0 && area != 666 && area < 900 && group != 0 && serial != 0
}

fn b58_decode(s: &str) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = B58.iter().position(|&b| b == c)? as u32;
        for byte in out.iter_mut() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|&b| b == b'1').count();
    let mut raw = vec![0u8; zeros];
    raw.extend(out.into_iter().rev());
    Some(raw)
}

fn base58_check_ok(s: &str) -> bool {
    let raw = match b58_decode(s) {
        Some(raw) if raw.len() >= 6 => raw,
        _ => return false,
    };
    let (body, sum) = raw.split_at(raw.len() - 4);
    let digest = Sha256::digest(Sha256::digest(body));
    &digest[..4] == sum
}

/// The five generators come from BIP 173. A wrong generator rejects every real
/// address.
fn polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut chk = 1u32;
    for v in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ff_ffff) << 5) ^ (*v as u32);
        for (i, gen) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= *gen;
            }
        }
    }
    chk
}

fn bech32_ok(s: &str) -> bool {
    if s != s.to_ascii_lowercase() {
        return false;
    }
    let sep = match s.rfind('1') {
        Some(sep) => sep,
        None => return false,
    };
    let hrp = &s[..sep];
    let data = &s[sep + 1..];
    if hrp != "bc" || data.len() < 6 {
        return false;
    }
    let mut values: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|b| b & 31));
    for c in data.bytes() {
        match BECH32_CHARSET.iter().position(|&x| x == c) {
            Some(v) => values.push(v as u8),
            None => return false,
        }
    }
    let chk = polymod(&values);
    chk == 1 || chk == 0x2bc8_30a3
}

/// ISO 9362 allows 8 or 11 characters. A 9- or 10-character all-caps run is not
/// a BIC.
fn swift_ok(token: &str) -> bool {
    (token.len() == 8 || token.len() == 11) && is_country(&token[4..6])
}

struct Hit {
    start: usize,
    end: usize,
    class: Class,
}

/// One value the detector found in text that is about to leave the machine.
/// `start` and `end` count `chars()`, because the console marks a row in
/// characters, not bytes. `left` says the Agent Model already holds the value,
/// which is what turns a redaction into a leak. `leak.rs` records it.
pub(crate) struct Found {
    pub value: String,
    pub class: &'static str,
    pub start: usize,
    pub end: usize,
    pub left: bool,
}

/// The token table for every conversation this machine has tokenized. The table
/// is the key that opens each marker, so the envelope seals the file and
/// `read_file` never decrypts it.
#[derive(Default, Serialize, Deserialize)]
struct Table {
    /// `⟦EMAIL_1⟧` → the real value.
    values: HashMap<String, String>,
    /// The real value → its token.
    tokens: HashMap<String, String>,
    /// A normalized token interior → its token, for the repair pass.
    repairs: HashMap<String, String>,
    counters: HashMap<Class, usize>,
}

impl Table {
    fn load(path: &Path) -> Table {
        let Ok(bytes) = std::fs::read(path) else {
            return Table::default();
        };
        let plain = crate::crypto::open(&bytes).unwrap_or(bytes);
        serde_json::from_slice(&plain).unwrap_or_default()
    }

    fn token(&mut self, class: Class, value: &str) -> String {
        if let Some(existing) = self.tokens.get(value) {
            return existing.clone();
        }
        let next = self.counters.entry(class).or_insert(0);
        *next += 1;
        let token = format!("{OPEN}{}_{}{CLOSE}", class.label(), *next);
        self.repairs.insert(normalize(&token), token.clone());
        self.values.insert(token.clone(), value.to_string());
        self.tokens.insert(value.to_string(), token.clone());
        token
    }

    fn resolve(&self, token: &str) -> Option<&str> {
        match self.values.get(token) {
            Some(value) => Some(value),
            None => self
                .repairs
                .get(&normalize(token))
                .and_then(|t| self.values.get(t))
                .map(String::as_str),
        }
    }

    /// A token this table never issued stays as written, because inventing a
    /// value is worse than showing a token.
    fn detokenize(&self, text: &str) -> String {
        if !text.contains(OPEN) {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(OPEN) {
            out.push_str(&rest[..i]);
            rest = &rest[i..];
            match rest[OPEN.len_utf8()..].find(CLOSE) {
                Some(rel) => {
                    let end = OPEN.len_utf8() + rel + CLOSE.len_utf8();
                    let token = &rest[..end];
                    out.push_str(self.resolve(token).unwrap_or(token));
                    rest = &rest[end..];
                }
                None => {
                    out.push_str(rest);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn breakdown(&self) -> Vec<(&'static str, usize)> {
        let mut rows: Vec<(&'static str, usize)> = Class::ALL
            .iter()
            .filter_map(|c| {
                self.counters
                    .get(c)
                    .map(|n| (c.label(), *n))
                    .filter(|(_, n)| *n > 0)
            })
            .collect();
        rows.sort_by_key(|(label, _)| *label);
        rows
    }

    fn count(&self) -> usize {
        self.counters.values().sum()
    }

    /// Only additions happen here, so one length sum detects a new mapping.
    fn len(&self) -> usize {
        self.values.len() + self.repairs.len()
    }
}

/// One table for the process, so a marker an earlier run wrote to a transcript
/// still resolves after a restart.
#[derive(Clone)]
struct Store {
    path: Option<PathBuf>,
    table: Arc<Mutex<Table>>,
}

impl Store {
    fn file(path: PathBuf) -> Store {
        Store {
            path: Some(path.clone()),
            table: Arc::new(Mutex::new(Table::load(&path))),
        }
    }

    fn memory() -> Store {
        Store {
            path: None,
            table: Arc::new(Mutex::new(Table::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A crash must not lose the mapping for a value a provider already saw, so
    /// a new mapping reaches the file before the request goes out.
    fn commit(&self, table: &Table, before: usize) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if table.len() == before {
            return;
        }
        let Ok(json) = serde_json::to_vec(table) else {
            return;
        };
        if let Ok(sealed) = crate::crypto::seal(&json) {
            let _ = std::fs::write(path, sealed);
        }
    }
}

/// `lib.rs` points the table at the sealed store before the first request. An
/// unset path leaves the table in memory, which is what a test wants.
static SHARED: OnceLock<Store> = OnceLock::new();

pub(crate) fn init(path: PathBuf) {
    let _ = SHARED.set(Store::file(path));
}

pub(crate) struct Redactor {
    store: Store,
    registry: Vec<String>,
    /// Text the Agent Model already holds in this conversation. The gate judges
    /// none of that text, because those words already left the PC.
    sent: HashSet<String>,
    /// The pass searches a value inside this text for the leak report.
    sent_blob: String,
    leaks: Mutex<Vec<Class>>,
}

impl Redactor {
    pub(crate) fn new() -> Self {
        Self {
            store: SHARED.get().cloned().unwrap_or_else(Store::memory),
            registry: Vec::new(),
            sent: HashSet::new(),
            sent_blob: String::new(),
            leaks: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    fn with_store(store: Store) -> Self {
        Self {
            store,
            registry: Vec::new(),
            sent: HashSet::new(),
            sent_blob: String::new(),
            leaks: Mutex::new(Vec::new()),
        }
    }

    /// A declared value has no shape, so only an exact match finds the value.
    pub(crate) fn seed(&mut self, value: &str) {
        if value.chars().count() >= 4 && !self.registry.iter().any(|v| v == value) {
            self.registry.push(value.to_string());
        }
    }

    /// A false positive on the instruction set replaces the whole set with one
    /// token, which breaks the run.
    pub(crate) fn pending(&self, msgs: &mut [serde_json::Value]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for msg in msgs.iter().rev() {
            if !leaves(msg.get("role").and_then(|r| r.as_str())) {
                continue;
            }
            scoreable(msg, &mut found);
        }
        self.unjudged(found)
    }

    pub(crate) fn pending_from<'a>(&self, texts: impl Iterator<Item = &'a str>) -> Vec<String> {
        self.unjudged(texts.map(str::to_string).collect())
    }

    /// A row the Agent Model already holds needs no new judgement, because
    /// those words already left the PC. The agent loop re-sends the whole
    /// window on every tool turn. `record` keeps those resends cheap.
    fn unjudged(&self, found: Vec<String>) -> Vec<String> {
        found.into_iter().filter(|m| !self.sent.contains(m)).collect()
    }

    pub(crate) fn note_sent(&mut self, rows: impl Iterator<Item = String>) {
        for row in rows {
            self.sent_blob.push_str(&row);
            self.sent_blob.push('\n');
            self.sent.insert(row);
        }
    }

    /// A cleared message counts as sent, so no later turn asks about it again.
    pub(crate) fn record(&mut self, message: &str) {
        self.sent.insert(message.to_string());
    }

    pub(crate) fn leaks(&self) -> Vec<&'static str> {
        self.leaks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|class| class.label())
            .collect()
    }

    /// A later audit cannot take back a value the Agent Model already holds.
    fn leaked(&self, value: &str) -> bool {
        !self.sent_blob.is_empty() && self.sent_blob.contains(value)
    }

    /// Detection with no store write, so a leak scan cannot inflate the chip
    /// count. `tokenize` issues a token and commits the map; `scan` reads the
    /// same detector and keeps neither.
    pub(crate) fn scan(&self, text: &str) -> Vec<Found> {
        let hits = self.hits(text);
        let mut found = Vec::with_capacity(hits.len());
        // `hits` is ascending and never overlaps, so one walk converts byte
        // offsets to char offsets. A count per hit would re-read the text for
        // every value of a long row.
        let mut byte = 0;
        let mut chars = 0;
        for hit in hits {
            chars += text[byte..hit.start].chars().count();
            byte = hit.start;
            let value = &text[hit.start..hit.end];
            found.push(Found {
                left: self.leaked(value),
                value: value.to_string(),
                class: hit.class.label(),
                start: chars,
                end: chars + value.chars().count(),
            });
        }
        found
    }

    pub(crate) fn tokenize(&mut self, text: &str) -> String {
        let mut table = self.store.lock();
        let before = table.len();
        let out = self.tokenize_in(&mut table, text);
        self.store.commit(&table, before);
        out
    }

    fn tokenize_in(&self, table: &mut Table, text: &str) -> String {
        if let Some(token) = table.tokens.get(text) {
            return token.clone();
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for hit in self.hits(text) {
            out.push_str(&text[last..hit.start]);
            let value = &text[hit.start..hit.end];
            if self.leaked(value) {
                let mut leaks = self
                    .leaks
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !leaks.contains(&hit.class) {
                    leaks.push(hit.class);
                }
            }
            out.push_str(&table.token(hit.class, value));
            last = hit.end;
        }
        out.push_str(&text[last..]);
        out
    }

    pub(crate) fn detokenize(&self, text: &str) -> String {
        self.store.lock().detokenize(text)
    }

    pub(crate) fn detokenize_stream(&self, carry: &mut String, chunk: &str) -> String {
        let mut buf = std::mem::take(carry);
        buf.push_str(chunk);
        let split = match buf.rfind(OPEN) {
            Some(i) if !buf[i..].contains(CLOSE) => i,
            _ => buf.len(),
        };
        let mut head = self.detokenize(&buf[..split]);
        let mut tail = buf[split..].to_string();
        if tail.chars().count() > MAX_TOKEN_CHARS {
            head.push_str(&tail);
            tail.clear();
        }
        *carry = tail;
        head
    }

    /// A run that ends inside a token shows the token, because the map holds no
    /// value for that token.
    pub(crate) fn flush(&self, carry: &mut String) -> String {
        let out = self.detokenize(carry);
        carry.clear();
        out
    }

    pub(crate) fn breakdown(&self) -> Vec<(&'static str, usize)> {
        self.store.lock().breakdown()
    }

    pub(crate) fn redacted_count(&self) -> usize {
        self.store.lock().count()
    }

    pub(crate) fn tokenize_msgs(&mut self, msgs: &mut [serde_json::Value]) {
        let mut table = self.store.lock();
        let before = table.len();
        for msg in msgs.iter_mut() {
            walk_strings(msg, &mut |s: &mut String| *s = self.tokenize_in(&mut table, s));
        }
        self.store.commit(&table, before);
    }

    /// Detokenize at execute, so the tool sees the value the provider never saw.
    pub(crate) fn detokenize_json(&self, value: &mut serde_json::Value) {
        let table = self.store.lock();
        walk_strings(value, &mut |s: &mut String| *s = table.detokenize(s));
    }

    fn hits(&self, text: &str) -> Vec<Hit> {
        let mut found: Vec<Hit> = Vec::new();
        registry_hits(text, &self.registry, &mut found);
        push_matches(&RE_JWT, text, Class::Jwt, &mut found);
        for m in RE_IBAN.find_iter(text) {
            let normalized: String = m
                .as_str()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_uppercase();
            if iban_len(&normalized[..2]) == Some(normalized.len()) && mod97_ok(&normalized) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Iban,
                });
            }
        }
        for m in RE_CARD.find_iter(text) {
            let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            if (13..=19).contains(&digits.len()) && luhn(&digits) && known_bin(&digits) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Card,
                });
            }
        }
        for m in RE_SWIFT.find_iter(text) {
            if swift_ok(m.as_str()) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Swift,
                });
            }
        }
        for m in RE_B58.find_iter(text) {
            if base58_check_ok(m.as_str()) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Crypto,
                });
            }
        }
        for m in RE_BECH32.find_iter(text) {
            if bech32_ok(m.as_str()) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Crypto,
                });
            }
        }
        push_matches(&RE_API_KEY, text, Class::ApiKey, &mut found);
        push_matches(&RE_USER_PATH, text, Class::UserPath, &mut found);
        push_matches(&RE_PROFILE_PATH, text, Class::UserPath, &mut found);
        for m in RE_ABA.find_iter(text) {
            if aba_ok(m.as_str()) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Aba,
                });
            }
        }
        for m in RE_SSN.find_iter(text) {
            if ssn_ok(m.as_str()) {
                found.push(Hit {
                    start: m.start(),
                    end: m.end(),
                    class: Class::Ssn,
                });
            }
        }
        push_matches(&RE_EMAIL, text, Class::Email, &mut found);
        push_matches(&RE_PHONE_INTL, text, Class::Phone, &mut found);
        push_matches(&RE_PHONE, text, Class::Phone, &mut found);

        found.sort_by_key(|h| (h.class.rank(), h.start, std::cmp::Reverse(h.end)));
        let mut kept: Vec<Hit> = Vec::new();
        for hit in found {
            let overlaps = kept
                .iter()
                .any(|k: &Hit| hit.start < k.end && k.start < hit.end);
            if !overlaps {
                kept.push(hit);
            }
        }
        kept.sort_by_key(|h| h.start);
        kept
    }
}

/// `⟦EMAIL_1⟧`, `⟦ Email 1 ⟧`, and `⟦email1⟧` all normalize to `EMAIL1`.
fn normalize(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn push_matches(re: &Regex, text: &str, class: Class, out: &mut Vec<Hit>) {
    for m in re.find_iter(text) {
        out.push(Hit {
            start: m.start(),
            end: m.end(),
            class,
        });
    }
}

fn registry_hits(text: &str, registry: &[String], out: &mut Vec<Hit>) {
    let mut values: Vec<&String> = registry.iter().collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    for value in values {
        let mut from = 0;
        while let Some(rel) = text[from..].find(value.as_str()) {
            let start = from + rel;
            out.push(Hit {
                start,
                end: start + value.len(),
                class: Class::Entity,
            });
            from = start + value.len();
        }
    }
}

/// The instruction set is First Mate's own text. An assistant row holds text
/// the provider wrote.
fn leaves(role: Option<&str>) -> bool {
    matches!(role, Some("user") | Some("tool"))
}

/// A flagged string leaves the machine as one handle, so a structural string
/// must never be offered: a flagged `"user"` would replace the role value and
/// break the request. Only what the model reads is scoreable.
fn scoreable(msg: &serde_json::Value, out: &mut Vec<String>) {
    match msg.get("content") {
        Some(serde_json::Value::String(text)) => out.push(text.clone()),
        Some(serde_json::Value::Array(parts)) => {
            for part in parts {
                if part.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    out.push(text.to_string());
                }
            }
        }
        _ => {}
    }
}

fn walk_strings(value: &mut serde_json::Value, f: &mut dyn FnMut(&mut String)) {
    match value {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                walk_strings(item, f);
            }
        }
        serde_json::Value::Object(entries) => {
            for (key, item) in entries.iter_mut() {
                // A screenshot part holds base64, and no detector finds a value
                // in base64. Running 12 detectors over 600 KB on every turn costs
                // more than the check protects.
                if key == "image_url" {
                    continue;
                }
                walk_strings(item, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "4111 1111 1111 1111";
    const MC: &str = "5500 0000 0000 0004";
    const IBAN: &str = "DE89 3704 0044 0532 0130 00";
    const ABA: &str = "021000021";
    const SWIFT: &str = "DEUTDEFF";
    const SSN: &str = "123-45-6789";
    const B58_ADDR: &str = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
    const BECH32_ADDR: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const API_KEY: &str = "sk-abcdefghij0123456789ABCDEFGHIJKL";
    const EMAIL: &str = "dusti@example.com";
    const PHONE: &str = "555-867-5309";
    const PATH: &str = "C:\\Users\\dusti\\Documents\\payroll.csv";

    #[test]
    fn every_class_round_trips() {
        for value in [
            CARD, MC, IBAN, ABA, SWIFT, SSN, B58_ADDR, BECH32_ADDR, JWT, API_KEY, EMAIL, PHONE,
            PATH,
        ] {
            let mut r = Redactor::new();
            let text = format!("before {value} after");
            let sent = r.tokenize(&text);
            assert!(sent.contains(OPEN), "{value} left in the clear: {sent}");
            assert!(!sent.contains(value), "{value} survived: {sent}");
            assert_eq!(r.detokenize(&sent), text, "{value} did not return");
        }
    }

    #[test]
    fn a_failed_checksum_stays_in_the_clear() {
        for value in [
            "4111 1111 1111 1112",
            "DE00 3704 0044 0532 0130 00",
            "021000022",
            "000-12-3456",
            "123-45-0000",
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN3",
        ] {
            let mut r = Redactor::new();
            assert_eq!(r.tokenize(value), value, "{value} was tokenized");
        }
    }

    #[test]
    fn iban_digits_are_not_also_read_as_a_card() {
        let mut r = Redactor::new();
        let sent = r.tokenize(&format!("wire to {IBAN} today"));
        assert_eq!(
            r.breakdown(),
            vec![("IBAN", 1)],
            "the 18-digit tail is an Amex shape: {sent}"
        );
    }

    #[test]
    fn a_registry_value_with_no_shape_leaves_tokenized() {
        let mut r = Redactor::new();
        r.seed("90821447");
        let sent = r.tokenize("my account is 90821447, order 90821448");
        assert!(sent.contains("90821448"), "an unseeded id leaked: {sent}");
        assert!(!sent.contains("90821447"), "a seeded value leaked: {sent}");
        assert_eq!(
            r.detokenize(&sent),
            "my account is 90821447, order 90821448"
        );
    }

    #[test]
    fn one_value_keeps_one_token_across_the_run() {
        let mut r = Redactor::new();
        let first = r.tokenize(EMAIL);
        let second = r.tokenize(&format!("reply to {EMAIL}"));
        assert_eq!(first, format!("{OPEN}EMAIL_1{CLOSE}"));
        assert!(second.contains(&first), "the token changed: {second}");
        assert_eq!(r.redacted_count(), 1);
    }

    #[test]
    fn a_mangled_token_still_detokenizes() {
        let mut r = Redactor::new();
        r.tokenize(EMAIL);
        for mangled in ["⟦email1⟧", "⟦ Email_1 ⟧", "⟦EMAIL-1⟧", "⟦email_1⟧"] {
            assert_eq!(r.detokenize(mangled), EMAIL, "{mangled} was not repaired");
        }
    }

    #[test]
    fn an_unknown_token_is_left_as_written() {
        let r = Redactor::new();
        assert_eq!(r.detokenize("⟦EMAIL_9⟧"), "⟦EMAIL_9⟧");
    }

    #[test]
    fn a_token_split_across_chunks_still_detokenizes() {
        let mut r = Redactor::new();
        let full = r.tokenize(&format!("email {EMAIL} and call {PHONE}"));
        let mut out = String::new();
        let mut carry = String::new();
        for piece in full.chars().collect::<Vec<char>>().chunks(3) {
            let text: String = piece.iter().collect();
            out.push_str(&r.detokenize_stream(&mut carry, &text));
        }
        out.push_str(&r.flush(&mut carry));
        assert_eq!(out, r.detokenize(&full), "the carry lost a boundary");
    }

    #[test]
    fn a_bare_bracket_does_not_hold_the_answer() {
        let r = Redactor::new();
        let mut carry = String::new();
        let mut out = String::new();
        out.push_str(&r.detokenize_stream(&mut carry, "opening a bracket ⟦ and then "));
        out.push_str(&r.detokenize_stream(&mut carry, &"x".repeat(80)));
        out.push_str(&r.flush(&mut carry));
        assert!(out.contains(&"x".repeat(80)), "the stream stalled: {out}");
    }

    #[test]
    fn a_fenced_config_block_returns_byte_identical() {
        let source = "here is the file:\n```json\n{\n  \"key\": \"sk-abcdefghij0123456789ABCDEFGHIJKL\",\n  \"mail\": \"dusti@example.com\"\n}\n```\nkeep it secret";
        let mut r = Redactor::new();
        let sent = r.tokenize(source);
        assert_eq!(r.breakdown().len(), 2, "both values must go: {sent}");
        assert_eq!(r.detokenize(&sent), source);
    }

    #[test]
    fn the_breakdown_names_classes_and_never_values() {
        let mut r = Redactor::new();
        r.tokenize(&format!("{CARD} {EMAIL} {ABA} {EMAIL}"));
        let rows = r.breakdown();
        assert_eq!(rows, vec![("ABA", 1), ("CARD", 1), ("EMAIL", 1)]);
        let flat: String = rows.iter().map(|(label, _)| *label).collect();
        assert!(
            !flat.contains('@') && !flat.contains("4111") && !flat.contains("021000021"),
            "a value reached the breakdown"
        );
    }

    #[test]
    fn the_json_walk_reaches_parts_and_tool_arguments() {
        let mut msgs = serde_json::json!([
            {"role": "user", "content": [{"type": "text", "text": EMAIL}]},
            {"role": "assistant", "tool_calls": [{"function": {"name": "run_command",
              "arguments": format!("{{\"command\":\"type {PATH}\"}}")}}]}
        ]);
        let before = msgs.to_string();
        let mut r = Redactor::new();
        r.tokenize_msgs(msgs.as_array_mut().unwrap());
        let sent = msgs.to_string();
        assert!(!sent.contains(EMAIL) && !sent.contains("dusti"), "leak: {sent}");
        r.detokenize_json(&mut msgs);
        assert_eq!(msgs.to_string(), before);
    }

    #[test]
    fn a_screenshot_part_is_skipped_and_its_text_part_is_not() {
        let mut msgs = serde_json::json!([{"role": "user", "content": [
            {"type": "text", "text": EMAIL},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{CARD}")}}
        ]}]);
        let mut r = Redactor::new();
        r.tokenize_msgs(msgs.as_array_mut().unwrap());
        let sent = msgs.to_string();
        assert!(sent.contains(CARD), "the base64 payload was rewritten: {sent}");
        assert!(!sent.contains(EMAIL), "the text part leaked: {sent}");
    }

    #[test]
    fn prose_without_secrets_is_untouched() {
        let source = "The GL account 4400 has 3 rows. See `fn main()` and v1.2.3 \
                      plus 2026-10-10 and a 30-day window.";
        let mut r = Redactor::new();
        assert_eq!(r.tokenize(source), source);
    }

    #[test]
    fn a_stray_handle_is_reported_by_argument_key() {
        let stray = serde_json::json!({ "command": "dir ⟦USER_PATH_9⟧", "shell": "cmd" });
        assert_eq!(stray_argument(&stray).as_deref(), Some("command"));
        let nested = serde_json::json!({ "ops": [{ "path": "C:\\⟦EMAIL_1⟧" }] });
        assert_eq!(stray_argument(&nested).as_deref(), Some("path"));
        let clean = serde_json::json!({ "path": "C:\\Users\\me\\notes.md" });
        assert_eq!(stray_argument(&clean), None);
        let prose = serde_json::json!({ "content": "a lone ⟦ bracket" });
        assert_eq!(stray_argument(&prose), None);
    }

    #[test]
    fn an_approved_message_never_returns_to_the_scorer() {
        let message = "My therapist cancelled our session twice this month and I am out of money.";
        let mut msgs = serde_json::json!([{"role": "user", "content": message}]);
        let mut r = Redactor::new();
        assert_eq!(
            r.pending(msgs.as_array_mut().unwrap().as_mut_slice()).len(),
            1
        );
        r.record(message);
        assert!(r
            .pending(msgs.as_array_mut().unwrap().as_mut_slice())
            .is_empty());
        r.tokenize_msgs(msgs.as_array_mut().unwrap());
        assert_eq!(
            msgs[0]["content"], message,
            "an approved message leaves as written"
        );
    }

    #[test]
    fn a_row_an_earlier_run_sent_never_reaches_the_scorer() {
        let sent = "The lease names my landlord's account number and my date of birth.";
        let mut r = Redactor::new();
        r.note_sent(std::iter::once(sent.to_string()));
        let mut msgs = serde_json::json!([{"role": "user", "content": sent}]);
        assert!(r
            .pending(msgs.as_array_mut().unwrap().as_mut_slice())
            .is_empty());
    }

    #[test]
    fn a_value_the_agent_model_already_holds_is_reported() {
        let mut r = Redactor::new();
        r.note_sent(std::iter::once(
            "I paid the card 4111 1111 1111 1111 last week.".to_string(),
        ));
        let sealed = r.tokenize("Charge the card 4111 1111 1111 1111 again.");
        assert!(sealed.contains("⟦CARD_1⟧"), "the repeat value is replaced");
        assert_eq!(r.leaks(), vec!["CARD"]);
    }

    #[test]
    fn a_value_that_never_left_the_pc_is_not_reported() {
        let mut r = Redactor::new();
        r.note_sent(std::iter::once("nothing sensitive here".to_string()));
        r.tokenize("My card is 4111 1111 1111 1111.");
        assert!(r.leaks().is_empty());
    }

    #[test]
    fn the_gate_scores_only_text_that_leaves_the_pc() {
        let private = "I told my doctor I stopped taking the pills the pharmacy gave me last August.";
        let mut msgs = serde_json::json!([
            {"role": "system", "content": private},
            {"role": "assistant", "content": private,
             "tool_calls": [{"function": {"arguments": private}}]},
            {"role": "tool", "content": "the pill bottle says twice daily"},
            {"role": "user", "content": "I stopped taking the pills"}
        ]);
        let r = Redactor::new();
        assert_eq!(
            r.pending(msgs.as_array_mut().unwrap().as_mut_slice()),
            vec![
                "I stopped taking the pills".to_string(),
                "the pill bottle says twice daily".to_string()
            ],
            "the instruction set and the provider's own text never reach the scorer"
        );
    }

    /// The scorer sees only the text the model reads, never the strings that
    /// hold a message together.
    #[test]
    fn only_the_text_the_model_reads_reaches_the_scorer() {
        let content = "My lease ends this month and the landlord kept my deposit.";
        let args = r#"{"path":"C:\\Users\\me\\notes.md"}"#;
        let mut msgs = serde_json::json!([
            {"role": "user", "content": [
                {"type": "text", "text": content},
                {"type": "image_url",
                 "image_url": {"url": format!("data:image/png;base64,{CARD}")}}
            ]},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_7", "type": "function",
                 "function": {"name": "run_command", "arguments": args}}
            ]}
        ]);
        let r = Redactor::new();
        assert_eq!(
            r.pending(msgs.as_array_mut().unwrap().as_mut_slice()),
            vec![content.to_string()],
            "a structural string or the provider's own text reached the scorer"
        );
    }

    /// A transcript row from an earlier run holds the marker, so a later run
    /// must resolve it.
    #[test]
    fn a_token_from_an_earlier_run_resolves_from_the_sealed_file() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        let dir = std::env::temp_dir().join("fm-token-map");
        std::fs::create_dir_all(&dir).unwrap();
        crate::crypto::init(dir.clone());
        let path = dir.join(crate::crypto::TOKEN_MAP);
        let _ = std::fs::remove_file(&path);
        let token = {
            let mut first = Redactor::with_store(Store::file(path.clone()));
            first.tokenize(&format!("pay {CARD} now"))
        };
        assert!(token.contains("CARD_1"), "{token}");
        let second = Redactor::with_store(Store::file(path.clone()));
        assert_eq!(second.detokenize(&token), format!("pay {CARD} now"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_stored_table_holds_no_plaintext_value() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        let dir = std::env::temp_dir().join("fm-token-map-plain");
        std::fs::create_dir_all(&dir).unwrap();
        crate::crypto::init(dir.clone());
        let path = dir.join(crate::crypto::TOKEN_MAP);
        let _ = std::fs::remove_file(&path);
        let mut red = Redactor::with_store(Store::file(path.clone()));
        red.tokenize(&format!("pay {CARD} now"));
        let bytes = std::fs::read(&path).unwrap();
        assert!(crate::crypto::looks_sealed(&bytes), "the table must be sealed");
        assert!(!String::from_utf8_lossy(&bytes).contains("4111"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_scan_sees_every_occurrence_and_writes_no_token() {
        let mut r = Redactor::new();
        let found = r.scan(&format!("pay {CARD} now, then pay {CARD} again"));
        assert_eq!(found.len(), 2, "a scan judges each occurrence");
        assert_eq!(found[0].class, "CARD");
        assert_eq!(found[0].value, CARD);
        assert!(!found[0].left, "the value never left this machine");
        assert_eq!(r.redacted_count(), 0, "a scan issued a token");
        assert!(r.breakdown().is_empty(), "a scan moved the chip count");
        r.note_sent(std::iter::once(format!("I paid {CARD} last week")));
        let again = r.scan(&format!("pay {CARD} now"));
        assert!(again[0].left, "a value the Agent Model holds has left");
        assert_eq!(r.redacted_count(), 0, "a scan issued a token");
    }

    #[test]
    fn a_scan_counts_characters_and_not_bytes() {
        let r = Redactor::new();
        let found = r.scan(&format!("Zahlungskarte ✓ {CARD}"));
        assert_eq!(found[0].start, 16, "the offset counts chars");
        assert_eq!(found[0].end, 35, "the offset ends one past the value");
    }

}
