//! Just enough HTTP/1.x request parsing for the setup portal.

#[derive(Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub body: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// Read more and try again.
    Incomplete,
    Invalid,
    Done(Request<'a>),
}

/// Parses a request once its headers, and the body they announce, have arrived.
pub fn parse(buffer: &[u8]) -> Parsed<'_> {
    let Some(head_len) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Parsed::Incomplete;
    };
    let Ok(head) = core::str::from_utf8(&buffer[..head_len]) else {
        return Parsed::Invalid;
    };
    let mut lines = head.split("\r\n");
    let mut words = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path)) = (words.next(), words.next()) else {
        return Parsed::Invalid;
    };

    let mut length = 0;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        if name.trim().eq_ignore_ascii_case("content-length") {
            match value.trim().parse() {
                Ok(value) => length = value,
                Err(_) => return Parsed::Invalid,
            }
        }
    }
    let start = head_len + 4;
    match buffer.get(start..start + length) {
        Some(body) => Parsed::Done(Request { method, path, body }),
        None => Parsed::Incomplete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get() {
        let request = b"GET /?x=1 HTTP/1.1\r\nHost: 192.168.4.1\r\n\r\n";
        assert_eq!(parse(request), Parsed::Done(Request { method: "GET", path: "/?x=1", body: b"" }));
    }

    #[test]
    fn waits_for_the_body() {
        let request = b"POST /save HTTP/1.1\r\nContent-Length: 7\r\n\r\nssid=ab";
        for end in 0..request.len() {
            assert_eq!(parse(&request[..end]), Parsed::Incomplete);
        }
        assert_eq!(parse(request), Parsed::Done(Request { method: "POST", path: "/save", body: b"ssid=ab" }));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(b"GET\r\n\r\n"), Parsed::Invalid);
        assert_eq!(parse(b"POST / HTTP/1.1\r\ncontent-length: x\r\n\r\n"), Parsed::Invalid);
    }
}
