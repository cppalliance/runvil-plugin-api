//! Whether a text is the JSON of an object, which is the rule a tool's
//! parameters follow. The Harness parses them with a JSON library; this
//! crate depends on none, so the fake Host checks the grammar itself.

/// The deepest nesting of arrays and objects a text may hold. The
/// Harness's parser refuses the 128th level.
const MAX_DEPTH: usize = 127;

/// Whether `text` is one JSON value that is an object, with nothing but
/// whitespace around it.
pub(super) fn is_object(text: &str) -> bool {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    parser.space();
    parser.peek() == Some(b'{') && parser.value(0) && {
        parser.space();
        parser.at == parser.bytes.len()
    }
}

/// A cursor over the text. Each method reads one piece of the grammar
/// from the cursor on, and returns whether it was there.
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    /// The byte at the cursor.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// Steps over `byte` when it is at the cursor.
    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    /// Steps over any whitespace.
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// One value, nested `depth` arrays and objects deep.
    fn value(&mut self, depth: usize) -> bool {
        match self.peek() {
            Some(b'{') => self.items(depth, b'}', |parser, depth| {
                parser.string()
                    && {
                        parser.space();
                        parser.eat(b':')
                    }
                    && {
                        parser.space();
                        parser.value(depth)
                    }
            }),
            Some(b'[') => self.items(depth, b']', Parser::value),
            Some(b'"') => self.string(),
            Some(b't') => self.word(b"true"),
            Some(b'f') => self.word(b"false"),
            Some(b'n') => self.word(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => false,
        }
    }

    /// An array or an object: its opening byte, then `item`s separated by
    /// commas, then `close`.
    fn items(&mut self, depth: usize, close: u8, item: fn(&mut Self, usize) -> bool) -> bool {
        if depth == MAX_DEPTH {
            return false;
        }
        self.at += 1;
        self.space();
        if self.eat(close) {
            return true;
        }
        loop {
            if !item(self, depth + 1) {
                return false;
            }
            self.space();
            if self.eat(close) {
                return true;
            }
            if !self.eat(b',') {
                return false;
            }
            self.space();
        }
    }

    /// One of the literal names.
    fn word(&mut self, word: &[u8]) -> bool {
        let found = self.bytes[self.at..].starts_with(word);
        if found {
            self.at += word.len();
        }
        found
    }

    /// A string: no raw control character, and only the escapes JSON
    /// names.
    fn string(&mut self) -> bool {
        if !self.eat(b'"') {
            return false;
        }
        loop {
            let Some(byte) = self.peek() else {
                return false;
            };
            self.at += 1;
            match byte {
                b'"' => return true,
                b'\\' if !self.escape() => return false,
                0x00..=0x1f => return false,
                _ => {}
            }
        }
    }

    /// What follows a backslash in a string.
    fn escape(&mut self) -> bool {
        let Some(byte) = self.peek() else {
            return false;
        };
        self.at += 1;
        match byte {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => true,
            b'u' => match self.hex4() {
                // A leading surrogate needs its trailing one.
                Some(0xd800..=0xdbff) => {
                    self.eat(b'\\')
                        && self.eat(b'u')
                        && matches!(self.hex4(), Some(0xdc00..=0xdfff))
                }
                // A trailing surrogate alone names no character.
                Some(0xdc00..=0xdfff) | None => false,
                Some(_) => true,
            },
            _ => false,
        }
    }

    /// Four hex digits, as the number they spell.
    fn hex4(&mut self) -> Option<u32> {
        let digits = self.bytes.get(self.at..self.at + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.at += 4;
        u32::from_str_radix(text, 16).ok()
    }

    /// A number, in JSON's grammar, whose value is finite.
    fn number(&mut self) -> bool {
        let start = self.at;
        self.eat(b'-');
        if !self.eat(b'0') && !self.digits() {
            return false;
        }
        if self.eat(b'.') && !self.digits() {
            return false;
        }
        if self.eat(b'e') || self.eat(b'E') {
            let _signed = self.eat(b'+') || self.eat(b'-');
            if !self.digits() {
                return false;
            }
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()
            .and_then(|text| text.parse::<f64>().ok())
            .is_some_and(f64::is_finite)
    }

    /// One or more digits.
    fn digits(&mut self) -> bool {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at > start
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, is_object};

    #[test]
    fn an_object_of_any_json_is_accepted() {
        for text in [
            "{}",
            " \t\r\n{ } \n",
            r#"{"type":"object","properties":{}}"#,
            r#"{ "a" : [ 1 , -2.5 , 3e10 , 4E-2 , 0 , true , false , null , "s" , [ ] , { } ] }"#,
            r#"{"escapes":"\" \\ \/ \b \f \n \r \t \u00e9 \ud83d\ude00"}"#,
            r#"{"a":1,"a":2}"#,
            r#"{"é":"ünïcödé"}"#,
        ] {
            assert!(is_object(text), "{text}");
        }
    }

    #[test]
    fn a_value_that_is_not_an_object_is_refused() {
        for text in ["", " ", "[]", "\"{}\"", "1", "true", "null"] {
            assert!(!is_object(text), "{text:?}");
        }
    }

    #[test]
    fn broken_json_is_refused() {
        for text in [
            "{",
            "{}}",
            "{} {}",
            "{,}",
            r#"{"a"}"#,
            r#"{"a":}"#,
            r#"{"a":1,}"#,
            "{a:1}",
            "{'a':1}",
            r#"{"a":[1,]}"#,
            r#"{"a":[1 2]}"#,
            r#"{"a":01}"#,
            r#"{"a":1.}"#,
            r#"{"a":.5}"#,
            r#"{"a":1e}"#,
            r#"{"a":+1}"#,
            r#"{"a":1e999}"#,
            r#"{"a":tru}"#,
            r#"{"a":nul}"#,
            r#"{"a":"unclosed}"#,
            r#"{"a":"\x"}"#,
            r#"{"a":"\u12"}"#,
            r#"{"a":"\u12g4"}"#,
            r#"{"a":"\ud83d"}"#,
            r#"{"a":"\ud83d\u0041"}"#,
            r#"{"a":"\ude00"}"#,
            "{\"a\":\"line\nbreak\"}",
        ] {
            assert!(!is_object(text), "{text}");
        }
    }

    #[test]
    fn nesting_is_accepted_up_to_the_harness_parsers_limit() {
        let nested =
            |depth: usize| format!("{}{}", "{\"a\":".repeat(depth), "1") + &"}".repeat(depth);
        assert!(is_object(&nested(MAX_DEPTH)));
        assert!(!is_object(&nested(MAX_DEPTH + 1)));
    }
}
