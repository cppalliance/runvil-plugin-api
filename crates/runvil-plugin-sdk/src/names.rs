//! The rules the Harness applies to the name and version a Plugin
//! describes itself with, as `const fn`s, so [`entry`](crate::entry) can
//! check a Plugin's constants when it is compiled.

/// The contract's bound on a name and on a version, in bytes.
pub(crate) const MAX_LEN: usize = 64;

/// Whether `name` is one Pack id segment the Harness accepts: 1 to 64
/// bytes of lowercase ASCII letters, digits, `-`, `_`, and `.`.
pub(crate) const fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_LEN {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if !matches!(bytes[at], b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.') {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether `version` is a version the Harness accepts: 1 to 64 bytes with
/// no control character, which `char::is_control` defines as U+0000 to
/// U+001F and U+007F to U+009F. In UTF-8 those are the bytes below 0x20,
/// 0x7F, and 0xC2 followed by 0x80 to 0x9F.
pub(crate) const fn is_valid_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_LEN {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte < 0x20 || byte == 0x7f {
            return false;
        }
        if byte == 0xc2 && at + 1 < bytes.len() && matches!(bytes[at + 1], 0x80..=0x9f) {
            return false;
        }
        at += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{MAX_LEN, is_valid_name, is_valid_version};

    #[test]
    fn a_name_is_one_lowercase_segment() {
        for name in ["hello", "acme-url", "web_2", "a.b", "0"] {
            assert!(is_valid_name(name), "{name}");
        }
        for name in ["", "Hello", "acme/url", "a b", "café", "a\n"] {
            assert!(!is_valid_name(name), "{name:?}");
        }
    }

    #[test]
    fn a_name_is_at_most_64_bytes() {
        assert!(is_valid_name(&"a".repeat(MAX_LEN)));
        assert!(!is_valid_name(&"a".repeat(MAX_LEN + 1)));
    }

    #[test]
    fn a_version_is_any_text_without_control_characters() {
        for version in ["1.4.0", "0.1.0-beta+3", "2026-11-06", "v1 (café)"] {
            assert!(is_valid_version(version), "{version}");
        }
        for version in ["", "1.0\n", "1\t0", "1\u{7f}", "1\u{85}"] {
            assert!(!is_valid_version(version), "{version:?}");
        }
    }

    #[test]
    fn the_version_rule_matches_char_is_control() {
        for code in 0..=0x2ff_u32 {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let version = format!("1{c}");
            assert_eq!(is_valid_version(&version), !c.is_control(), "U+{code:04X}");
        }
    }

    #[test]
    fn a_version_is_at_most_64_bytes() {
        assert!(is_valid_version(&"1".repeat(MAX_LEN)));
        assert!(!is_valid_version(&"1".repeat(MAX_LEN + 1)));
    }
}
