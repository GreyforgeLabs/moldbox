//! Formatting helpers that reproduce the observable text conventions of the
//! original Python implementation (`str()`/`repr()` of paths and OS errors,
//! float `repr`, `{:,}` grouping, `json.dumps` escaping and `pathlib`
//! normalisation), so that output stays byte-for-byte compatible.
//!
//! Paths are handled as raw bytes. Undecodable bytes are mapped the way Python
//! maps them through the `surrogateescape` error handler (U+DC80..U+DCFF).

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::printable::is_printable;

/// Decode bytes into Python-style code points (`surrogateescape`).
pub fn py_chars(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.extend(chunk.valid().chars().map(u32::from));
        out.extend(chunk.invalid().iter().map(|&b| 0xDC00 + u32::from(b)));
    }
    out
}

fn push_code_point(out: &mut String, cp: u32) {
    match char::from_u32(cp) {
        Some(c) => out.push(c),
        // A lone surrogate cannot be encoded; Python's `backslashreplace`
        // handler (used for stderr) renders it as `\udcXX`.
        None => out.push_str(&format!("\\u{cp:04x}")),
    }
}

/// Render bytes the way Python writes the decoded `str` to stderr
/// (UTF-8 with `backslashreplace`).
pub fn display_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for cp in py_chars(bytes) {
        push_code_point(&mut out, cp);
    }
    out
}

/// Python `repr()` of the `str` decoded from `bytes`.
pub fn py_repr(bytes: &[u8]) -> String {
    let chars = py_chars(bytes);
    let has_single = chars.contains(&u32::from('\''));
    let has_double = chars.contains(&u32::from('"'));
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(chars.len() + 2);
    out.push(quote);
    for cp in chars {
        match cp {
            0x5C => out.push_str("\\\\"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            _ if cp == u32::from(quote) => {
                out.push('\\');
                out.push(quote);
            }
            _ if is_printable(cp) => push_code_point(&mut out, cp),
            _ if cp < 0x100 => out.push_str(&format!("\\x{cp:02x}")),
            _ if cp < 0x10000 => out.push_str(&format!("\\u{cp:04x}")),
            _ => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out.push(quote);
    out
}

/// `json.dumps()` of a string with the default `ensure_ascii=True`.
pub fn json_str(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for cp in py_chars(bytes) {
        match cp {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x20..=0x7E => out.push(char::from(cp as u8)),
            0x7F => out.push('\x7f'),
            0x10000.. => {
                let v = cp - 0x10000;
                let hi = 0xD800 | (v >> 10);
                let lo = 0xDC00 | (v & 0x3FF);
                out.push_str(&format!("\\u{hi:04x}\\u{lo:04x}"));
            }
            _ => out.push_str(&format!("\\u{cp:04x}")),
        }
    }
    out.push('"');
    out
}

/// Python `repr(float)` (shortest round-trip, `.0` suffix, exponent outside
/// `1e-4 <= |x| < 1e16`).
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let sci = format!("{x:e}");
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent digits");
    let (negative, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let point = exp as usize + 1;
            if digits.len() <= point {
                out.push_str(&digits);
                for _ in digits.len()..point {
                    out.push('0');
                }
                out.push_str(".0");
            } else {
                out.push_str(&digits[..point]);
                out.push('.');
                out.push_str(&digits[point..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let sign = if exp < 0 { '-' } else { '+' };
        out.push_str(&format!("e{sign}{:02}", exp.abs()));
    }
    out
}

/// Python `round(x, 2)` (correctly rounded decimal result).
pub fn round2(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.2}").parse().unwrap_or(x)
}

/// Python `format(n, ",")`.
pub fn thousands(n: i128) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `str(pathlib.PurePosixPath(arg))`: collapse repeated separators, drop `.`
/// components and trailing separators, keep `..` and a POSIX `//` root.
pub fn pathlib_str(arg: &OsStr) -> OsString {
    let bytes = arg.as_bytes();
    let (root, rel): (&[u8], &[u8]) = if !bytes.starts_with(b"/") {
        (b"", bytes)
    } else if bytes.get(1) != Some(&b'/') || bytes.get(2) == Some(&b'/') {
        (b"/", &bytes[1..])
    } else {
        (b"//", &bytes[2..])
    };
    let parts: Vec<&[u8]> = rel
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty() && *p != b".")
        .collect();
    let mut out = root.to_vec();
    out.extend_from_slice(&parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    OsString::from_vec(out)
}

/// `pathlib` `.name` and `.parent` of an already normalised path.
pub fn pathlib_split(normalized: &OsStr) -> (OsString, OsString) {
    let bytes = normalized.as_bytes();
    let root_len = if bytes.starts_with(b"//") && !bytes.starts_with(b"///") {
        2
    } else if bytes.starts_with(b"/") {
        1
    } else {
        0
    };
    let rel = &bytes[root_len..];
    if rel.is_empty() || rel == b"." {
        return (OsString::new(), normalized.to_os_string());
    }
    match rel.iter().rposition(|b| *b == b'/') {
        Some(i) => (
            OsString::from_vec(rel[i + 1..].to_vec()),
            OsString::from_vec(bytes[..root_len + i].to_vec()),
        ),
        None => {
            let parent = if root_len == 0 {
                b".".to_vec()
            } else {
                bytes[..root_len].to_vec()
            };
            (OsString::from_vec(rel.to_vec()), OsString::from_vec(parent))
        }
    }
}

/// `os.path.normpath` for POSIX.
pub fn normpath(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let initial = if path.starts_with(b"//") && !path.starts_with(b"///") {
        2
    } else if path.starts_with(b"/") {
        1
    } else {
        0
    };
    let mut comps: Vec<&[u8]> = Vec::new();
    for comp in path.split(|b| *b == b'/') {
        if comp.is_empty() || comp == b"." {
            continue;
        }
        if comp != b".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&&b".."[..])
        {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let mut out = vec![b'/'; initial];
    out.extend_from_slice(&comps.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

/// `os.path.abspath` for POSIX.
pub fn abspath(path: &[u8]) -> io::Result<Vec<u8>> {
    if path.starts_with(b"/") {
        return Ok(normpath(path));
    }
    let mut joined = std::env::current_dir()?.into_os_string().into_vec();
    if !joined.ends_with(b"/") {
        joined.push(b'/');
    }
    joined.extend_from_slice(path);
    Ok(normpath(&joined))
}

/// The C `strerror()` text for an OS error number.
pub fn strerror(errno: i32) -> String {
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// `str(OSError)` with one or two file names.
pub fn os_error_message(
    err: &io::Error,
    filename: Option<&OsStr>,
    filename2: Option<&OsStr>,
) -> String {
    let Some(errno) = err.raw_os_error() else {
        return err.to_string();
    };
    let mut out = format!("[Errno {errno}] {}", strerror(errno));
    if let Some(f1) = filename {
        out.push_str(": ");
        out.push_str(&py_repr(f1.as_bytes()));
        if let Some(f2) = filename2 {
            out.push_str(" -> ");
            out.push_str(&py_repr(f2.as_bytes()));
        }
    }
    out
}

/// First code point of every block of ten Unicode decimal digits (category
/// Nd, Unicode 16.0), so `int()`/`float()` accept non-ASCII digits as Python
/// does.
const DECIMAL_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0,
    0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50, 0x11D50,
    0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE, 0x1D7D8,
    0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// `_PyUnicode_TransformDecimalAndSpaceToASCII`: map Unicode decimal digits to
/// ASCII digits and Unicode whitespace to a space. Returns `None` for input
/// that is not valid UTF-8 or contains other non-ASCII characters (Python
/// rejects those as numbers anyway).
fn transform_decimal_and_space(text: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(text).ok()?;
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let cp = u32::from(ch);
        if ch.is_ascii() {
            out.push(cp as u8);
        } else if matches!(
            cp,
            0x85 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
        ) {
            out.push(b' ');
        } else {
            let zero = DECIMAL_ZEROS
                .iter()
                .rev()
                .find(|z| **z <= cp && cp < **z + 10)?;
            out.push(b'0' + (cp - zero) as u8);
        }
    }
    Some(out)
}

/// Python `int(text)` (base 10). Values outside the `i128`
/// range saturate (they are only ever compared against small bounds).
pub fn parse_py_int(text: &[u8]) -> Option<i128> {
    let text = transform_decimal_and_space(text)?;
    let s = trim_ascii_ws(&text);
    let (negative, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if !valid_digit_underscores(digits) {
        return None;
    }
    let mut value: i128 = 0;
    for &b in digits.iter().filter(|b| **b != b'_') {
        value = value
            .saturating_mul(10)
            .saturating_add(i128::from(b - b'0'));
    }
    Some(if negative { -value } else { value })
}

/// Python `float(text)`.
pub fn parse_py_float(text: &[u8]) -> Option<f64> {
    let text = transform_decimal_and_space(text)?;
    let s = trim_ascii_ws(&text);
    if s.is_empty() {
        return None;
    }
    let mut cleaned = Vec::with_capacity(s.len());
    for (i, &b) in s.iter().enumerate() {
        if b == b'_' {
            let before = i > 0 && s[i - 1].is_ascii_digit();
            let after = s.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !(before && after) {
                return None;
            }
        } else {
            cleaned.push(b);
        }
    }
    let cleaned = std::str::from_utf8(&cleaned).ok()?;
    let lower = cleaned.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if lower.len() - unsigned.len() > 1 {
        return None;
    }
    let negative = lower.starts_with('-');
    let special = match unsigned {
        "inf" | "infinity" => Some(f64::INFINITY),
        "nan" => Some(f64::NAN),
        _ => None,
    };
    if let Some(v) = special {
        return Some(if negative { -v } else { v });
    }
    if !unsigned
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-'))
    {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn trim_ascii_ws(text: &[u8]) -> &[u8] {
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C);
    let start = text.iter().position(|b| !is_ws(b)).unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|b| !is_ws(b))
        .map_or(start, |i| i + 1);
    &text[start..end]
}

fn valid_digit_underscores(digits: &[u8]) -> bool {
    if digits.is_empty()
        || !digits[0].is_ascii_digit()
        || !digits[digits.len() - 1].is_ascii_digit()
    {
        return false;
    }
    let mut prev_underscore = false;
    for &b in digits {
        match b {
            b'_' if prev_underscore => return false,
            b'_' => prev_underscore = true,
            b'0'..=b'9' => prev_underscore = false,
            _ => return false,
        }
    }
    true
}

/// Percent-encode like `urllib.parse.quote(path, safe="/")`.
pub fn quote_path(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (0.56, "0.56"),
            (12.35, "12.35"),
            (1234.5, "1234.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (2.5e20, "2.5e+20"),
            (f64::INFINITY, "inf"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(value), expected, "{value:e}");
        }
    }

    #[test]
    fn round2_is_decimal_rounding() {
        assert_eq!(round2(1.234_9), 1.23);
        assert_eq!(round2(1.235_1), 1.24);
        assert_eq!(round2(0.004), 0.0);
        assert_eq!(float_repr(round2(0.1 + 0.2)), "0.3");
        // Exact binary ties round half-to-even, like Python's round().
        assert_eq!(round2(0.125), 0.12);
        assert_eq!(round2(0.375), 0.38);
        assert_eq!(round2(2.675), 2.67);
        assert_eq!(round2(1.005), 1.0);
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(8192), "8,192");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(-1_234), "-1,234");
    }

    #[test]
    fn repr_and_json_escaping() {
        assert_eq!(py_repr(b"plain"), "'plain'");
        assert_eq!(py_repr(b"it's"), "\"it's\"");
        assert_eq!(py_repr(b"both'\""), "'both\\'\"'");
        assert_eq!(py_repr(b"a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(py_repr(b"\x01\x7f"), "'\\x01\\x7f'");
        assert_eq!(py_repr("caf\u{e9}".as_bytes()), "'caf\u{e9}'");
        assert_eq!(py_repr(b"bad\xff"), "'bad\\udcff'");
        assert_eq!(json_str(b"a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(json_str("caf\u{e9}".as_bytes()), "\"caf\\u00e9\"");
        assert_eq!(json_str("\u{1F600}".as_bytes()), "\"\\ud83d\\ude00\"");
        assert_eq!(json_str(b"\x01\xff"), "\"\\u0001\\udcff\"");
        assert_eq!(display_bytes(b"x\xffy"), "x\\udcffy");
    }

    #[test]
    fn pathlib_normalisation() {
        let norm = |s: &str| pathlib_str(OsStr::new(s)).into_string().unwrap();
        assert_eq!(norm(""), ".");
        assert_eq!(norm("."), ".");
        assert_eq!(norm("./a//b/./"), "a/b");
        assert_eq!(norm("a/../b"), "a/../b");
        assert_eq!(norm("/x/"), "/x");
        assert_eq!(norm("//x"), "//x");
        assert_eq!(norm("///x"), "/x");
        let split = |s: &str| {
            let (n, p) = pathlib_split(OsStr::new(s));
            (n.into_string().unwrap(), p.into_string().unwrap())
        };
        assert_eq!(split("b.db"), ("b.db".into(), ".".into()));
        assert_eq!(split("/b.db"), ("b.db".into(), "/".into()));
        assert_eq!(split("a/b/c.db"), ("c.db".into(), "a/b".into()));
        assert_eq!(split("."), ("".into(), ".".into()));
        assert_eq!(split("/"), ("".into(), "/".into()));
    }

    #[test]
    fn normpath_matches_posixpath() {
        assert_eq!(normpath(b""), b".");
        assert_eq!(normpath(b"/a/../b/./c/"), b"/b/c");
        assert_eq!(normpath(b"../x"), b"../x");
        assert_eq!(normpath(b"/.."), b"/");
        assert_eq!(normpath(b"//a"), b"//a");
    }

    #[test]
    fn number_parsing_matches_python() {
        assert_eq!(parse_py_int(b"17"), Some(17));
        assert_eq!(parse_py_int(b" +1_000 "), Some(1000));
        assert_eq!(parse_py_int(b"-5"), Some(-5));
        assert_eq!(parse_py_int(b"007"), Some(7));
        assert_eq!(parse_py_int(b"1.5"), None);
        assert_eq!(parse_py_int(b"1__0"), None);
        assert_eq!(parse_py_int(b"_1"), None);
        assert_eq!(parse_py_int(b""), None);
        assert_eq!(
            parse_py_int(b"99999999999999999999999999999999999999999"),
            Some(i128::MAX)
        );
        assert_eq!(parse_py_float(b"2.5"), Some(2.5));
        assert_eq!(parse_py_float(b" 1_0.5 "), Some(10.5));
        assert_eq!(parse_py_float(b"1e3"), Some(1000.0));
        assert_eq!(parse_py_float(b".5"), Some(0.5));
        assert_eq!(parse_py_float(b"5."), Some(5.0));
        assert_eq!(parse_py_float(b"-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_py_float(b"nan").unwrap().is_nan());
        assert_eq!(parse_py_float(b"1e999"), Some(f64::INFINITY));
        assert_eq!(parse_py_float(b"x"), None);
        assert_eq!(parse_py_float(b"1_"), None);
        assert_eq!(parse_py_float(b"0x10"), None);
        assert_eq!(parse_py_float(b"--1"), None);
        assert_eq!(parse_py_int("\u{663}".as_bytes()), Some(3));
        assert_eq!(
            parse_py_int("\u{ff11}\u{ff12}\u{3000}".as_bytes()),
            Some(12)
        );
        assert_eq!(parse_py_float("\u{967}.\u{96B}".as_bytes()), Some(1.5));
        assert_eq!(parse_py_int(b"\xff"), None);
        assert_eq!(parse_py_int(b"5\x1c"), None);
        assert_eq!(parse_py_int("5\u{85}".as_bytes()), Some(5));
        assert_eq!(parse_py_int("\u{b2}".as_bytes()), None);
    }

    #[test]
    fn quote_matches_urllib() {
        assert_eq!(quote_path(b"/tmp/a b/%x?#.db"), "/tmp/a%20b/%25x%3F%23.db");
        assert_eq!(quote_path("/\u{e9}".as_bytes()), "/%C3%A9");
        assert_eq!(quote_path(b"/~_-."), "/~_-.");
    }

    #[test]
    fn os_error_formatting() {
        let err = io::Error::from_raw_os_error(2);
        assert_eq!(
            os_error_message(&err, Some(OsStr::new("/x/y")), None),
            "[Errno 2] No such file or directory: '/x/y'"
        );
        let err = io::Error::from_raw_os_error(17);
        assert_eq!(
            os_error_message(&err, Some(OsStr::new("a")), Some(OsStr::new("b"))),
            "[Errno 17] File exists: 'a' -> 'b'"
        );
    }
}
