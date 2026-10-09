//! A tiny JSON writer for the command-line output (see `cli`), so Petal needs no
//! serialisation crates. Objects keep their fields in the order they were added.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// Sizes and counts: always whole numbers.
    Int(u64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(&'static str, Json)>),
}

impl Json {
    /// A string from anything that may not be valid UTF-8 (a path, say). Invalid bytes
    /// become U+FFFD, the replacement character.
    pub fn lossy(s: &std::ffi::OsStr) -> Json {
        Json::Str(s.to_string_lossy().into_owned())
    }

    /// Pretty-printed, two spaces per level, ending with a newline.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, level: usize) {
        let indent = |out: &mut String, level: usize| {
            for _ in 0..level {
                out.push_str("  ");
            }
        };
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Str(s) => write_str(out, s),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Array(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    indent(out, level + 1);
                    item.write(out, level + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                indent(out, level);
                out.push(']');
            }
            Json::Object(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Object(fields) => {
                out.push_str("{\n");
                for (i, (key, value)) in fields.iter().enumerate() {
                    indent(out, level + 1);
                    write_str(out, key);
                    out.push_str(": ");
                    value.write(out, level + 1);
                    out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
                }
                indent(out, level);
                out.push('}');
            }
        }
    }
}

/// Reading values back, for checking output in tests.
#[cfg(test)]
impl Json {
    /// The field `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// A JSON string literal: quotes, backslashes and control characters escaped; everything
/// else written as-is (JSON text is UTF-8).
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(s: &str) -> String {
        let mut out = String::new();
        write_str(&mut out, s);
        out
    }

    #[test]
    fn escapes_strings() {
        assert_eq!(string("plain"), r#""plain""#);
        assert_eq!(string(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(string(r"C:\dir\"), r#""C:\\dir\\""#);
        assert_eq!(string("a\nb\tc\r"), r#""a\nb\tc\r""#);
        assert_eq!(string("\u{0}\u{1}\u{8}\u{c}\u{1f}\u{7f}"), r#""\u0000\u0001\b\f\u001f\u007f""#);
        // Anything else is written as UTF-8, including characters outside the BMP.
        assert_eq!(string("Café 🌸 /"), "\"Café 🌸 /\"");
    }

    #[test]
    fn invalid_utf8_becomes_the_replacement_character() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"bad\xffname");
        assert_eq!(Json::lossy(name), Json::Str("bad\u{fffd}name".into()));
        assert_eq!(Json::lossy(name).render(), "\"bad\u{fffd}name\"\n");
    }

    #[test]
    fn renders_nested_values() {
        let value = Json::Object(vec![
            ("schema_version", Json::Int(1)),
            ("ok", Json::Bool(true)),
            ("none", Json::Null),
            ("empty", Json::Array(Vec::new())),
            ("list", Json::Array(vec![Json::Int(1), Json::Object(vec![("k", Json::Str("v".into()))])])),
        ]);
        let expected = "{\n  \"schema_version\": 1,\n  \"ok\": true,\n  \"none\": null,\n  \"empty\": [],\n  \"list\": [\n    1,\n    {\n      \"k\": \"v\"\n    }\n  ]\n}\n";
        assert_eq!(value.render(), expected);
        assert_eq!(value.get("schema_version").and_then(Json::as_u64), Some(1));
        assert_eq!(Json::Int(u64::MAX).render(), "18446744073709551615\n");
    }
}
