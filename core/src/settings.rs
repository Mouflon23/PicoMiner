//! Settings entered in the setup portal: how they are stored in flash, and how
//! the submitted form updates them.

use core::fmt::{self, Display, Formatter};

use heapless::{String, Vec};

/// Bytes a stored record can take: two flash pages.
pub const RECORD_SIZE: usize = 512;

const MAGIC: [u8; 4] = *b"PMS1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub wifi_ssid: String<32>,
    pub wifi_password: String<64>,
    pub btc_address: String<90>,
    pub pool_host: String<64>,
    pub pool_port: u16,
    pub worker: String<32>,
}

impl Settings {
    /// Whether there is enough to join a network and mine.
    pub fn is_complete(&self) -> bool {
        !self.wifi_ssid.is_empty() && !self.btc_address.is_empty()
    }

    /// Writes the record: magic, CRC-32 of the rest, the port, then each text
    /// field with a length byte. The rest of `out` is left as erased flash.
    pub fn encode(&self, out: &mut [u8; RECORD_SIZE]) {
        out.fill(0xff);
        out[..4].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.pool_port.to_le_bytes());
        let mut at = 10;
        for field in self.fields() {
            out[at] = field.len() as u8;
            out[at + 1..at + 1 + field.len()].copy_from_slice(field.as_bytes());
            at += 1 + field.len();
        }
        let crc = crc32(&out[8..at]);
        out[4..8].copy_from_slice(&crc.to_le_bytes());
    }

    /// Reads a record written by [`encode`](Self::encode), or `None` if the
    /// flash holds anything else.
    pub fn decode(record: &[u8; RECORD_SIZE]) -> Option<Self> {
        if record[..4] != MAGIC {
            return None;
        }
        let mut settings = Settings {
            pool_port: u16::from_le_bytes([record[8], record[9]]),
            ..Settings::default()
        };
        let mut at = 10;
        let next = |at: &mut usize| -> Option<&str> {
            let len = *record.get(*at)? as usize;
            let text = record.get(*at + 1..*at + 1 + len)?;
            *at += 1 + len;
            core::str::from_utf8(text).ok()
        };
        settings.wifi_ssid = next(&mut at)?.try_into().ok()?;
        settings.wifi_password = next(&mut at)?.try_into().ok()?;
        settings.btc_address = next(&mut at)?.try_into().ok()?;
        settings.pool_host = next(&mut at)?.try_into().ok()?;
        settings.worker = next(&mut at)?.try_into().ok()?;
        let crc = u32::from_le_bytes([record[4], record[5], record[6], record[7]]);
        (crc32(&record[8..at]) == crc).then_some(settings)
    }

    fn fields(&self) -> [&str; 5] {
        [&self.wifi_ssid, &self.wifi_password, &self.btc_address, &self.pool_host, &self.worker]
    }

    /// Returns these settings updated from an `application/x-www-form-urlencoded`
    /// body. A blank password keeps the stored one when the network is the same,
    /// so the form never has to send the password back to the browser.
    pub fn with_form(&self, body: &[u8]) -> Result<Settings, &'static str> {
        let mut updated = self.clone();
        let mut password = None;
        let mut port = "";
        for pair in body.split(|&byte| byte == b'&') {
            let (key, value) = match pair.iter().position(|&byte| byte == b'=') {
                Some(at) => (&pair[..at], &pair[at + 1..]),
                None => (pair, &[][..]),
            };
            match key {
                b"ssid" => updated.wifi_ssid = decode_field(value)?,
                b"password" => password = Some(decode_field::<64>(value)?),
                b"address" => updated.btc_address = decode_field(value)?,
                b"host" => updated.pool_host = decode_field(value)?,
                b"port" => port = core::str::from_utf8(value).map_err(|_| "bad port")?,
                b"worker" => updated.worker = decode_field(value)?,
                _ => {}
            }
        }

        updated.wifi_ssid = trimmed(&updated.wifi_ssid);
        updated.btc_address = trimmed(&updated.btc_address);
        updated.pool_host = trimmed(&updated.pool_host);
        updated.worker = trimmed(&updated.worker);
        if updated.wifi_ssid.is_empty() {
            return Err("Wi-Fi network is missing");
        }
        if updated.btc_address.is_empty() {
            return Err("BTC address is missing");
        }
        if updated.pool_host.is_empty() {
            return Err("Pool host is missing");
        }
        updated.pool_port = match port.trim().parse::<u16>() {
            Ok(0) | Err(_) => return Err("Pool port must be between 1 and 65535"),
            Ok(port) => port,
        };
        match password {
            Some(password) if !password.is_empty() => updated.wifi_password = password,
            _ if updated.wifi_ssid != self.wifi_ssid => updated.wifi_password.clear(),
            _ => {}
        }
        if !updated.wifi_password.is_empty() && updated.wifi_password.len() < 8 {
            return Err("Wi-Fi password must have at least 8 characters");
        }
        Ok(updated)
    }
}

fn trimmed<const N: usize>(text: &String<N>) -> String<N> {
    text.trim().try_into().unwrap_or_default()
}

/// Undoes form encoding: `+` is a space and `%XX` a byte.
fn decode_field<const N: usize>(value: &[u8]) -> Result<String<N>, &'static str> {
    let mut bytes: Vec<u8, N> = Vec::new();
    let mut i = 0;
    while i < value.len() {
        let byte = match value[i] {
            b'+' => b' ',
            b'%' => {
                let hex = value.get(i + 1..i + 3).ok_or("bad form encoding")?;
                let hex = core::str::from_utf8(hex).map_err(|_| "bad form encoding")?;
                i += 2;
                u8::from_str_radix(hex, 16).map_err(|_| "bad form encoding")?
            }
            byte => byte,
        };
        bytes.push(byte).map_err(|_| "a field is too long")?;
        i += 1;
    }
    String::from_utf8(bytes).map_err(|_| "a field is not valid UTF-8")
}

/// CRC-32 as used by zip and Ethernet.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Prints text with the characters HTML gives a meaning to escaped.
pub struct Html<'a>(pub &'a str);

impl Display for Html<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut rest = self.0;
        while let Some(at) = rest.find(['&', '<', '>', '"', '\'']) {
            f.write_str(&rest[..at])?;
            f.write_str(match rest.as_bytes()[at] {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                b'"' => "&quot;",
                _ => "&#39;",
            })?;
            rest = &rest[at + 1..];
        }
        f.write_str(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            wifi_ssid: "Maison".try_into().unwrap(),
            wifi_password: "secret123".try_into().unwrap(),
            btc_address: "bc1qexample".try_into().unwrap(),
            pool_host: "pool.solomining.de".try_into().unwrap(),
            pool_port: 3333,
            worker: "picominer".try_into().unwrap(),
        }
    }

    #[test]
    fn record_round_trip() {
        let mut record = [0; RECORD_SIZE];
        sample().encode(&mut record);
        assert_eq!(Settings::decode(&record), Some(sample()));

        let mut full = sample();
        full.wifi_ssid = "s".repeat(32).as_str().try_into().unwrap();
        full.wifi_password = "p".repeat(64).as_str().try_into().unwrap();
        full.btc_address = "b".repeat(90).as_str().try_into().unwrap();
        full.pool_host = "h".repeat(64).as_str().try_into().unwrap();
        full.worker = "w".repeat(32).as_str().try_into().unwrap();
        full.encode(&mut record);
        assert_eq!(Settings::decode(&record), Some(full));
    }

    #[test]
    fn rejects_erased_and_corrupt_records() {
        assert_eq!(Settings::decode(&[0xff; RECORD_SIZE]), None);
        let mut record = [0; RECORD_SIZE];
        sample().encode(&mut record);
        record[20] ^= 1;
        assert_eq!(Settings::decode(&record), None);
    }

    #[test]
    fn applies_form() {
        let form = b"ssid=Box+5G&password=p%40ss+word&address=bc1qnew&host=public-pool.io&port=21496&worker=desk";
        let updated = sample().with_form(form).unwrap();
        assert_eq!(updated.wifi_ssid, "Box 5G");
        assert_eq!(updated.wifi_password, "p@ss word");
        assert_eq!(updated.btc_address, "bc1qnew");
        assert_eq!(updated.pool_host, "public-pool.io");
        assert_eq!(updated.pool_port, 21496);
        assert_eq!(updated.worker, "desk");
    }

    #[test]
    fn blank_password_keeps_it_on_the_same_network_only() {
        let same = b"ssid=Maison&password=&address=bc1q&host=h&port=1&worker=";
        assert_eq!(sample().with_form(same).unwrap().wifi_password, "secret123");
        let other = b"ssid=Other&password=&address=bc1q&host=h&port=1&worker=";
        assert_eq!(sample().with_form(other).unwrap().wifi_password, "");
    }

    #[test]
    fn rejects_bad_forms() {
        let base = Settings::default();
        assert!(base.with_form(b"ssid=&address=a&host=h&port=1").is_err());
        assert!(base.with_form(b"ssid=s&address=&host=h&port=1").is_err());
        assert!(base.with_form(b"ssid=s&address=a&host=h&port=0").is_err());
        assert!(base.with_form(b"ssid=s&address=a&host=h&port=70000").is_err());
        assert!(base.with_form(b"ssid=s&password=short&address=a&host=h&port=1").is_err());
        assert!(base.with_form(b"ssid=%zz&address=a&host=h&port=1").is_err());
        let long = format!("ssid={}&address=a&host=h&port=1", "x".repeat(33));
        assert!(base.with_form(long.as_bytes()).is_err());
    }

    #[test]
    fn escapes_html() {
        assert_eq!(format!("{}", Html("a<b>&\"c'")), "a&lt;b&gt;&amp;&quot;c&#39;");
        assert_eq!(format!("{}", Html("plain")), "plain");
    }
}
