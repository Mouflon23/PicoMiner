//! Zero-copy JSON reader. Stratum `params` arrays mix strings, numbers, booleans
//! and nested arrays, which a typed deserializer handles poorly without an
//! allocator, so values are walked lazily over the received line instead.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    /// Raw string contents; escapes are kept as received.
    String(&'a str),
    /// Raw text between the brackets.
    Array(&'a str),
    /// Raw text between the braces.
    Object(&'a str),
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches([' ', '\t', '\r', '\n'])
}

/// Byte index just past the string opening at `s[0]`.
fn string_end(s: &[u8]) -> Option<usize> {
    let mut i = 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Byte index just past the bracket closing the one at `s[0]`.
fn container_end(s: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'"' => {
                i += string_end(&s[i..])?;
                continue;
            }
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parses the value at the start of `s`, returning it and the remaining text.
pub fn parse(s: &str) -> Option<(Value<'_>, &str)> {
    let s = skip_ws(s);
    match *s.as_bytes().first()? {
        b'"' => {
            let end = string_end(s.as_bytes())?;
            Some((Value::String(s.get(1..end - 1)?), s.get(end..)?))
        }
        b'[' => {
            let end = container_end(s.as_bytes())?;
            Some((Value::Array(s.get(1..end - 1)?), s.get(end..)?))
        }
        b'{' => {
            let end = container_end(s.as_bytes())?;
            Some((Value::Object(s.get(1..end - 1)?), s.get(end..)?))
        }
        b't' => Some((Value::Bool(true), s.strip_prefix("true")?)),
        b'f' => Some((Value::Bool(false), s.strip_prefix("false")?)),
        b'n' => Some((Value::Null, s.strip_prefix("null")?)),
        _ => {
            let end = s
                .find(|c: char| !matches!(c, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'))
                .unwrap_or(s.len());
            if end == 0 {
                return None;
            }
            Some((Value::Number(&s[..end]), &s[end..]))
        }
    }
}

/// Iterates the elements of an [`Value::Array`].
#[derive(Clone)]
pub struct Elements<'a>(&'a str);

impl<'a> Iterator for Elements<'a> {
    type Item = Value<'a>;

    fn next(&mut self) -> Option<Value<'a>> {
        let (value, rest) = parse(self.0)?;
        let rest = skip_ws(rest);
        self.0 = rest.strip_prefix(',').unwrap_or(rest);
        Some(value)
    }
}

impl<'a> Value<'a> {
    pub fn elements(&self) -> Option<Elements<'a>> {
        match self {
            Value::Array(inner) => Some(Elements(inner)),
            _ => None,
        }
    }

    pub fn at(&self, index: usize) -> Option<Value<'a>> {
        self.elements()?.nth(index)
    }

    pub fn get(&self, key: &str) -> Option<Value<'a>> {
        let Value::Object(mut rest) = *self else { return None };
        loop {
            let (name, after) = parse(rest)?;
            let after = skip_ws(after).strip_prefix(':')?;
            let (value, after) = parse(after)?;
            if name == Value::String(key) {
                return Some(value);
            }
            let after = skip_ws(after);
            rest = after.strip_prefix(',')?;
        }
    }

    pub fn as_str(&self) -> Option<&'a str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => n.parse().ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_nested_values() {
        let (root, rest) = parse(r#" {"id":1,"error":null,"result":[[["mining.notify","ab"]],"77677db4",8]}"#).unwrap();
        assert_eq!(rest, "");
        assert_eq!(root.get("id").unwrap().as_u32(), Some(1));
        assert_eq!(root.get("error"), Some(Value::Null));
        assert_eq!(root.get("missing"), None);
        let result = root.get("result").unwrap();
        assert_eq!(result.at(1).unwrap().as_str(), Some("77677db4"));
        assert_eq!(result.at(2).unwrap().as_u32(), Some(8));
        assert_eq!(result.at(3), None);
        assert_eq!(result.at(0).unwrap().at(0).unwrap().at(1).unwrap().as_str(), Some("ab"));
    }

    #[test]
    fn brackets_inside_strings_do_not_count() {
        let (root, _) = parse(r#"["a]b", "c\"]", {"k": "}"}, 1.5e-4, true]"#).unwrap();
        let mut items = root.elements().unwrap();
        assert_eq!(items.next(), Some(Value::String("a]b")));
        assert_eq!(items.next(), Some(Value::String(r#"c\"]"#)));
        assert_eq!(items.next().unwrap().get("k").unwrap().as_str(), Some("}"));
        assert_eq!(items.next().unwrap().as_f64(), Some(0.00015));
        assert_eq!(items.next().unwrap().as_bool(), Some(true));
        assert_eq!(items.next(), None);
    }

    #[test]
    fn truncated_input_is_rejected_not_panicking() {
        for line in [r#"{"id":1"#, r#"["a"#, "[", "{", "\"", "tru", "", "-", "]", "}"] {
            let _ = parse(line);
        }
        assert!(parse(r#"{"id":1"#).is_none());
        assert!(parse(r#"["a"#).is_none());
    }
}
