//! Sensitive-info redaction. `docs/roadmap/REDACTION.md` is the design.
//! The outbound pass replaces each detected value with a `⟦CLASS_N⟧` token,
//! and detokenize runs at the harness boundaries.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// A token opens with this character. A model that mangles a token still
/// copies the pair, so the repair pass keys on it.
const OPEN: char = '⟦';
const CLOSE: char = '⟧';

/// A held fragment longer than this is prose, not a token. Without the cap, a
/// model that emits one bare `⟦` holds the rest of its answer in the carry.
const MAX_TOKEN_CHARS: usize = 48;

/// A span shorter than this holds no fact for a decision server to judge, and
/// the measured audit used the same floor.
const MIN_SCORE_CHARS: usize = 60;

/// One request scores at most this many spans, so a first turn over a large
/// file cannot spend its whole latency budget on scoring.
const MAX_SPANS_PER_REQUEST: usize = 16;

/// One `⟦…⟧` span, so a stray handle in tool arguments can be named.
static RE_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"⟦[^⟧]{1,48}⟧").unwrap());

/// The key of the argument whose value carries a token, deepest first. The
/// refusal names the key, because copying the handle back into the transcript
/// gives the model another handle to reuse.
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

/// A detected class. The console breakdown names this value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    /// A value the registry supplied, because no shape identifies it. A US
    /// account number is 8 to 17 digits and is indistinguishable from an
    /// order id.
    Entity,
    /// A span a decision server flagged. The whole span leaves as one token,
    /// because a probability carries no offset.
    Span,
}

impl Class {
    /// The label inside a token: `IBAN` in `⟦IBAN_2⟧`.
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
            Class::Span => "SPAN",
        }
    }

    /// Detection order, lowest first. A span that two detectors claim goes to
    /// the earlier class: an IBAN holds an 18-digit run that passes Luhn as an
    /// Amex, and a JWT holds a payload that can hold an email.
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
            Class::Span => 12,
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
        Class::Span,
    ];
}

/// ISO 3166-1 alpha-2. A BIC's country must be one of these, which is what
/// separates `DEUTDEFF` from the word `CORRESPONDS`.
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

/// IBAN lengths per country, from ISO 13616. An unlisted country has no
/// length to check, so layer 1 refuses the match rather than guess.
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
    // let the tail of the next word join the match, and the length check would
    // then reject a real IBAN.
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
/// A path under the user-profile root. `docs/roadmap/REDACTION.md` names this
/// root, because it holds mail stores, browser profiles, and credential files.
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
/// number, so layer 1 leaves it in the clear.
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
/// values that no issued card uses.
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

/// BIP 173 `polymod`. The five generators are the spec's, and a wrong one
/// rejects every real address.
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

/// ISO 9362 allows 8 or 11 characters. A 9- or 10-character all-caps run is
/// not a BIC, which is what keeps most all-caps English words out of this
/// class.
fn swift_ok(token: &str) -> bool {
    (token.len() == 8 || token.len() == 11) && is_country(&token[4..6])
}

struct Hit {
    start: usize,
    end: usize,
    class: Class,
}

/// The token map for one run. Nothing here is persisted: a map on disk is a
/// key that opens every token in every earlier conversation.
#[derive(Default)]
pub(crate) struct Redactor {
    /// `⟦EMAIL_1⟧` → the real value.
    values: HashMap<String, String>,
    /// The real value → its token, so one value keeps one token across a run.
    tokens: HashMap<String, String>,
    /// A normalized token interior → its token, for the repair pass.
    repairs: HashMap<String, String>,
    counters: HashMap<Class, usize>,
    registry: Vec<String>,
    /// Spans the decision server already scored. The agent loop re-sends the
    /// whole window on every tool turn, and a rescore repeats one audit.
    scored: HashSet<String>,
}

impl Redactor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Layer 2: a value the user declared sensitive. It has no shape, so only
    /// this exact string matches.
    pub(crate) fn seed(&mut self, value: &str) {
        if value.chars().count() >= 4 && !self.registry.iter().any(|v| v == value) {
            self.registry.push(value.to_string());
        }
    }

    /// The strings one JSON request carries that no server has scored, newest
    /// message first.
    pub(crate) fn pending(&self, msgs: &mut [serde_json::Value]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for msg in msgs.iter_mut().rev() {
            walk_strings(msg, &mut |s: &mut String| found.push(s.clone()));
        }
        self.budget(found)
    }

    /// The same choice for a caller that holds plain strings, because the
    /// Anthropic and Gemini request is not JSON at this point.
    pub(crate) fn pending_from<'a>(&self, spans: impl Iterator<Item = &'a str>) -> Vec<String> {
        self.budget(spans.map(str::to_string).collect())
    }

    /// The shared choice: long enough to judge, never scored before, and at most
    /// `MAX_SPANS_PER_REQUEST`. Caller order is priority order, because the
    /// message the user just typed must not lose the budget to older history.
    fn budget(&self, found: Vec<String>) -> Vec<String> {
        let mut kept: Vec<String> = found
            .into_iter()
            .filter(|s| s.chars().count() >= MIN_SCORE_CHARS && !self.scored.contains(s))
            .collect();
        kept.truncate(MAX_SPANS_PER_REQUEST);
        kept
    }

    /// Record one answer. A span at or above `threshold` becomes one token, and a
    /// scored span is never scored again. The return says whether the span left.
    pub(crate) fn record_score(&mut self, span: &str, score: f64, threshold: f64) -> bool {
        self.scored.insert(span.to_string());
        if score >= threshold {
            self.token(Class::Span, span);
            return true;
        }
        false
    }

    pub(crate) fn tokenize(&mut self, text: &str) -> String {
        if let Some(token) = self.tokens.get(text) {
            return token.clone();
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for hit in self.hits(text) {
            out.push_str(&text[last..hit.start]);
            let value = &text[hit.start..hit.end];
            out.push_str(&self.token(hit.class, value));
            last = hit.end;
        }
        out.push_str(&text[last..]);
        out
    }

    /// Replace every token with its value. A token this map never issued stays
    /// as written, because inventing a value is worse than showing a token.
    pub(crate) fn detokenize(&self, text: &str) -> String {
        if !text.contains(OPEN) {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(OPEN) {
            out.push_str(&rest[..i]);
            let after = &rest[i + OPEN.len_utf8()..];
            match after.find(CLOSE) {
                Some(rel) => {
                    let end = i + OPEN.len_utf8() + rel + CLOSE.len_utf8();
                    let token = &rest[i..end];
                    out.push_str(self.resolve(token).unwrap_or(token));
                    rest = &rest[end..];
                }
                None => {
                    out.push_str(&rest[i..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Detokenize a stream piece. A token can straddle two SSE chunks, so an
    /// unterminated `⟦…` waits in `carry` for its close.
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

    /// Emit what a stream left held. A run that ends inside a token shows the
    /// token, because no value can be guessed for it.
    pub(crate) fn flush(&self, carry: &mut String) -> String {
        let out = self.detokenize(carry);
        carry.clear();
        out
    }

    /// The chip breakdown. It names classes and counts, and never a value.
    pub(crate) fn breakdown(&self) -> Vec<(&'static str, usize)> {
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

    pub(crate) fn redacted_count(&self) -> usize {
        self.counters.values().sum()
    }

    /// Tokenize every string in the `messages` array of one request: text
    /// parts, tool call arguments, and tool results.
    pub(crate) fn tokenize_msgs(&mut self, msgs: &mut [serde_json::Value]) {
        for msg in msgs.iter_mut() {
            walk_strings(msg, &mut |s: &mut String| *s = self.tokenize(s));
        }
    }

    /// Detokenize tool call arguments at execute, so the tool sees the real
    /// value while the provider never sent it.
    pub(crate) fn detokenize_json(&self, value: &mut serde_json::Value) {
        walk_strings(value, &mut |s: &mut String| *s = self.detokenize(s));
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
                // A screenshot part holds base64, which carries no user value
                // the detectors could find. Running 12 detectors over 600 KB
                // on every turn costs more than it protects.
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
    fn a_flagged_span_leaves_as_one_token_and_returns_on_display() {
        let span = "My therapist cancelled our session twice this month and I am out of money.";
        let mut msgs = serde_json::json!([{"role": "user", "content": span}]);
        let mut r = Redactor::new();
        assert_eq!(
            r.pending(msgs.as_array_mut().unwrap().as_mut_slice()).len(),
            1
        );
        r.record_score(span, 0.91, 0.5);
        r.tokenize_msgs(msgs.as_array_mut().unwrap());
        assert_eq!(msgs[0]["content"], "⟦SPAN_1⟧");
        assert_eq!(
            r.detokenize(msgs[0]["content"].as_str().unwrap()),
            span,
            "the console must show the real sentence"
        );
        assert!(r
            .pending(msgs.as_array_mut().unwrap().as_mut_slice())
            .is_empty());
    }

    #[test]
    fn a_span_below_the_threshold_leaves_unchanged() {
        let span = "The quarterly report lists 3 rows for the GL account and one note.";
        let mut r = Redactor::new();
        r.record_score(span, 0.4, 0.5);
        assert_eq!(r.tokenize(span), span);
    }

    #[test]
    fn a_short_span_is_never_offered_to_the_server() {
        let mut msgs = serde_json::json!([{"role": "user", "content": "call me at 555-867-5309"}]);
        let r = Redactor::new();
        assert!(r
            .pending(msgs.as_array_mut().unwrap().as_mut_slice())
            .is_empty());
    }

    #[test]
    fn one_request_offers_at_most_the_span_budget() {
        let items: Vec<serde_json::Value> = (0..MAX_SPANS_PER_REQUEST + 5)
            .map(|i| {
                serde_json::json!({"role": "user",
                                   "content": format!("row {i}: {}", "x".repeat(70))})
            })
            .collect();
        let mut msgs = serde_json::Value::Array(items);
        let r = Redactor::new();
        assert_eq!(
            r.pending(msgs.as_array_mut().unwrap().as_mut_slice())
                .len(),
            MAX_SPANS_PER_REQUEST
        );
    }

    #[test]
    fn the_newest_span_wins_the_span_budget() {
        let mut items: Vec<serde_json::Value> = (0..MAX_SPANS_PER_REQUEST)
            .map(|i| {
                serde_json::json!({"role": "user",
                                   "content": format!("old row {i}: {}", "x".repeat(400))})
            })
            .collect();
        items.push(serde_json::json!(
            {"role": "user", "content": "just typed: my lease ends this month and the landlord kept my deposit"}
        ));
        let mut msgs = serde_json::Value::Array(items);
        let r = Redactor::new();
        let offered = r.pending(msgs.as_array_mut().unwrap().as_mut_slice());
        assert_eq!(offered.len(), MAX_SPANS_PER_REQUEST);
        assert!(
            offered.iter().any(|s| s.starts_with("just typed:")),
            "the newest message lost the budget to older history"
        );
    }

}
