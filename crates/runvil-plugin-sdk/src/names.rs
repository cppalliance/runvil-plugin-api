//! The rules the Harness applies to the names and texts a Plugin hands
//! it, under the contract's limits on them. The rules are `const fn`s, so
//! [`entry`](crate::entry) can check a Plugin's constants when it is
//! compiled.

use runvil_plugin_sys::{RV_MAX_LINE, RV_MAX_NAME};

/// Whether `name` is one name segment the Harness accepts: 1 to 64 bytes
/// of lowercase ASCII letters, digits, `-`, `_`, and `.`.
pub(crate) const fn is_valid_name(name: &str) -> bool {
    is_segment(name.as_bytes(), 0, name.len())
}

/// Whether `name` is a Pack name the Harness accepts: a vendor and a Pack
/// id joined by one `/`, each one name segment.
pub(crate) const fn is_valid_pack_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut slash = 0;
    while slash < bytes.len() && bytes[slash] != b'/' {
        slash += 1;
    }
    slash < bytes.len() && is_segment(bytes, 0, slash) && is_segment(bytes, slash + 1, bytes.len())
}

/// Whether `bytes[from..to]` is one name segment.
const fn is_segment(bytes: &[u8], from: usize, to: usize) -> bool {
    if to <= from || to - from > RV_MAX_NAME {
        return false;
    }
    let mut at = from;
    while at < to {
        if !matches!(bytes[at], b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.') {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether `name` is a wire name the Harness accepts: 1 to 64 bytes of
/// ASCII letters, digits, `_`, and `-`.
#[cfg(any(test, feature = "test-support"))]
pub(crate) const fn is_valid_wire_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > RV_MAX_NAME {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if !matches!(bytes[at], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-') {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether `version` is a version the Harness accepts: 1 to 64 bytes with
/// no control character.
pub(crate) const fn is_valid_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= RV_MAX_NAME && !has_control(version)
}

/// Whether `description` is a Pack description the Harness accepts: at
/// most 1024 bytes with no control character. It may be empty.
pub(crate) const fn is_valid_description(description: &str) -> bool {
    description.len() <= RV_MAX_LINE && !has_control(description)
}

/// Whether `text` holds a control character, which `char::is_control`
/// defines as U+0000 to U+001F and U+007F to U+009F. In UTF-8 those are
/// the bytes below 0x20, 0x7F, and 0xC2 followed by 0x80 to 0x9F.
pub(crate) const fn has_control(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte < 0x20 || byte == 0x7f {
            return true;
        }
        if byte == 0xc2 && at + 1 < bytes.len() && matches!(bytes[at + 1], 0x80..=0x9f) {
            return true;
        }
        at += 1;
    }
    false
}

/// `text` cut to at most `max` bytes, at a character boundary.
pub(crate) fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether `a` and `b` hold the same bytes, as a `const fn`.
pub(crate) const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use runvil_plugin_sys::{RV_MAX_LINE, RV_MAX_NAME};

    use super::{
        cut, has_control, is_valid_description, is_valid_name, is_valid_pack_name,
        is_valid_version, is_valid_wire_name, same,
    };

    #[test]
    fn a_text_is_cut_at_a_character_boundary() {
        assert_eq!(cut("short", 1024), "short");
        assert_eq!(cut("abcdef", 3), "abc");
        // 'é' is two bytes, so byte 2 falls inside one.
        assert_eq!(cut("aéé", 2), "a");
        assert_eq!(cut("aéé", 3), "aé");
    }

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
        assert!(is_valid_name(&"a".repeat(RV_MAX_NAME)));
        assert!(!is_valid_name(&"a".repeat(RV_MAX_NAME + 1)));
    }

    #[test]
    fn a_pack_name_is_a_vendor_and_a_pack_id_joined_by_one_slash() {
        for name in ["acme/url", "runvil/std", "a.b/c-d_e", "0/0"] {
            assert!(is_valid_pack_name(name), "{name}");
        }
        for name in [
            "",
            "url",
            "/url",
            "acme/",
            "/",
            "acme/url/x",
            "Acme/url",
            "acme/Url",
            "acme url",
        ] {
            assert!(!is_valid_pack_name(name), "{name:?}");
        }
    }

    #[test]
    fn each_segment_of_a_pack_name_is_at_most_64_bytes() {
        let long = "a".repeat(RV_MAX_NAME);
        assert!(is_valid_pack_name(&format!("{long}/{long}")));
        assert!(!is_valid_pack_name(&format!("{long}a/{long}")));
        assert!(!is_valid_pack_name(&format!("{long}/{long}a")));
    }

    #[test]
    fn a_wire_name_is_ascii_letters_digits_underscores_and_hyphens() {
        for name in ["Read", "StrReplace", "web_fetch", "a-b", "0"] {
            assert!(is_valid_wire_name(name), "{name}");
        }
        for name in ["", "web/fetch", "a.b", "a b", "café"] {
            assert!(!is_valid_wire_name(name), "{name:?}");
        }
        assert!(is_valid_wire_name(&"a".repeat(RV_MAX_NAME)));
        assert!(!is_valid_wire_name(&"a".repeat(RV_MAX_NAME + 1)));
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
    fn the_control_rule_matches_char_is_control() {
        for code in 0..=0x2ff_u32 {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let text = format!("1{c}");
            assert_eq!(has_control(&text), c.is_control(), "U+{code:04X}");
        }
    }

    #[test]
    fn a_version_is_at_most_64_bytes() {
        assert!(is_valid_version(&"1".repeat(RV_MAX_NAME)));
        assert!(!is_valid_version(&"1".repeat(RV_MAX_NAME + 1)));
    }

    #[test]
    fn a_description_is_one_line_of_at_most_1024_bytes_and_may_be_empty() {
        assert!(is_valid_description(""));
        assert!(is_valid_description("Greets a person by name"));
        assert!(is_valid_description(&"a".repeat(RV_MAX_LINE)));
        assert!(!is_valid_description(&"a".repeat(RV_MAX_LINE + 1)));
        assert!(!is_valid_description("two\nlines"));
    }

    #[test]
    fn two_texts_are_the_same_when_every_byte_is() {
        assert!(same("acme/url", "acme/url"));
        assert!(same("", ""));
        assert!(!same("acme/url", "acme/urn"));
        assert!(!same("acme/url", "acme/ur"));
    }
}
