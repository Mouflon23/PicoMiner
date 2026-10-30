//! Stratum v1: parsing pool messages, writing requests, turning a job into a header.

use core::fmt::{self, Write};

use crate::hex::{self, InvalidHex};
use crate::json::{self, Value};
use crate::sha256::{Sha256, sha256};
use crate::target::nbits_target;

pub const MAX_MERKLE_BRANCHES: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed,
    Hex,
    TooLong,
}

impl From<InvalidHex> for Error {
    fn from(_: InvalidHex) -> Self {
        Error::Hex
    }
}

impl From<fmt::Error> for Error {
    fn from(_: fmt::Error) -> Self {
        Error::TooLong
    }
}

/// Short string copied out of the receive buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

impl<const N: usize> Text<N> {
    pub const fn empty() -> Self {
        Self { bytes: [0; N], len: 0 }
    }

    pub fn new(text: &str) -> Result<Self, Error> {
        if text.len() > N || text.len() > u8::MAX as usize {
            return Err(Error::TooLong);
        }
        let mut bytes = [0; N];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Ok(Self { bytes, len: text.len() as u8 })
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// `mining.notify` parameters, borrowed from the receive buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Notify<'a> {
    pub job_id: &'a str,
    pub prevhash: &'a str,
    pub coinb1: &'a str,
    pub coinb2: &'a str,
    pub merkle: Value<'a>,
    pub version: &'a str,
    pub nbits: &'a str,
    pub ntime: &'a str,
    pub clean: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message<'a> {
    Notify(Notify<'a>),
    SetDifficulty(f64),
    /// Reply to a request of ours. `error` holds the reason when it was refused.
    Reply { id: u32, result: Value<'a>, error: Option<&'a str> },
    /// A valid message this miner has no use for.
    Ignored,
}

fn string_at<'a>(params: &Value<'a>, index: usize) -> Result<&'a str, Error> {
    params.at(index).and_then(|v| v.as_str()).ok_or(Error::Malformed)
}

fn error_reason<'a>(error: Option<Value<'a>>) -> Option<&'a str> {
    match error? {
        Value::Null => None,
        Value::String(reason) => Some(reason),
        // Pools send [code, "reason", traceback].
        array @ Value::Array(_) => Some(array.at(1).and_then(|v| v.as_str()).unwrap_or("error")),
        _ => Some("error"),
    }
}

pub fn parse_line(line: &str) -> Result<Message<'_>, Error> {
    let (root, _) = json::parse(line).ok_or(Error::Malformed)?;
    if let Some(method) = root.get("method").and_then(|m| m.as_str()) {
        let params = root.get("params").ok_or(Error::Malformed)?;
        return match method {
            "mining.notify" => Ok(Message::Notify(Notify {
                job_id: string_at(&params, 0)?,
                prevhash: string_at(&params, 1)?,
                coinb1: string_at(&params, 2)?,
                coinb2: string_at(&params, 3)?,
                merkle: params.at(4).filter(|v| v.elements().is_some()).ok_or(Error::Malformed)?,
                version: string_at(&params, 5)?,
                nbits: string_at(&params, 6)?,
                ntime: string_at(&params, 7)?,
                clean: params.at(8).and_then(|v| v.as_bool()).unwrap_or(false),
            })),
            "mining.set_difficulty" => {
                let difficulty = params.at(0).and_then(|v| v.as_f64()).ok_or(Error::Malformed)?;
                if difficulty > 0.0 {
                    Ok(Message::SetDifficulty(difficulty))
                } else {
                    Err(Error::Malformed)
                }
            }
            _ => Ok(Message::Ignored),
        };
    }
    let Some(id) = root.get("id").and_then(|v| v.as_u32()) else {
        return Ok(Message::Ignored);
    };
    Ok(Message::Reply {
        id,
        result: root.get("result").unwrap_or(Value::Null),
        error: error_reason(root.get("error")),
    })
}

/// Reassembles newline-delimited messages from TCP reads. A message longer than
/// the buffer is dropped rather than parsed truncated.
pub struct LineBuffer<const N: usize> {
    buf: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> LineBuffer<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0, overflowed: false }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.overflowed = false;
    }

    /// Free space to read into. Always non-empty.
    pub fn space(&mut self) -> &mut [u8] {
        if self.len == N {
            self.len = 0;
            self.overflowed = true;
        }
        &mut self.buf[self.len..]
    }

    pub fn filled(&mut self, count: usize) {
        self.len += count;
    }

    /// Length of the first complete line, newline excluded.
    pub fn line_len(&mut self) -> Option<usize> {
        loop {
            let end = self.buf[..self.len].iter().position(|&b| b == b'\n')?;
            if !self.overflowed {
                return Some(end);
            }
            self.overflowed = false;
            self.consume(end);
        }
    }

    pub fn line(&self, len: usize) -> &str {
        core::str::from_utf8(&self.buf[..len]).unwrap_or("")
    }

    /// Drops the line of length `len` and its newline.
    pub fn consume(&mut self, len: usize) {
        self.buf.copy_within(len + 1..self.len, 0);
        self.len -= len + 1;
    }
}

impl<const N: usize> Default for LineBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub extranonce1: Text<32>,
    pub extranonce2_size: usize,
}

/// Reads the reply to `mining.subscribe`.
pub fn parse_subscribe(result: &Value<'_>) -> Result<Session, Error> {
    let extranonce1 = result.at(1).and_then(|v| v.as_str()).ok_or(Error::Malformed)?;
    let size = result.at(2).and_then(|v| v.as_u32()).ok_or(Error::Malformed)? as usize;
    if extranonce1.len() % 2 != 0 || !(1..=8).contains(&size) {
        return Err(Error::Malformed);
    }
    Ok(Session { extranonce1: Text::new(extranonce1)?, extranonce2_size: size })
}

pub fn write_subscribe(out: &mut impl Write, id: u32, agent: &str) -> Result<(), Error> {
    writeln!(out, "{{\"id\":{id},\"method\":\"mining.subscribe\",\"params\":[\"{agent}\"]}}")?;
    Ok(())
}

pub fn write_authorize(out: &mut impl Write, id: u32, user: &str, password: &str) -> Result<(), Error> {
    writeln!(out, "{{\"id\":{id},\"method\":\"mining.authorize\",\"params\":[\"{user}\",\"{password}\"]}}")?;
    Ok(())
}

pub fn write_suggest_difficulty(out: &mut impl Write, id: u32, difficulty: f64) -> Result<(), Error> {
    writeln!(out, "{{\"id\":{id},\"method\":\"mining.suggest_difficulty\",\"params\":[{difficulty}]}}")?;
    Ok(())
}

pub fn write_submit(
    out: &mut impl Write,
    id: u32,
    user: &str,
    job: &Job,
    nonce: u32,
) -> Result<(), Error> {
    writeln!(
        out,
        "{{\"id\":{id},\"method\":\"mining.submit\",\"params\":[\"{user}\",\"{}\",\"{}\",\"{}\",\"{nonce:08x}\"]}}",
        job.job_id.as_str(),
        job.extranonce2.as_str(),
        job.ntime.as_str(),
    )?;
    Ok(())
}

/// A job ready to mine, plus what `mining.submit` needs to refer back to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub job_id: Text<32>,
    pub extranonce2: Text<16>,
    pub ntime: Text<8>,
    pub header: [u8; 80],
    /// Little-endian block target, from `nbits`.
    pub target: [u8; 32],
    pub height: Option<u32>,
}

fn swap_word(bytes: &mut [u8]) {
    bytes.reverse();
}

/// Each miner keeps a fixed extranonce2: the 32-bit nonce space outlasts any job
/// at this hashrate by hours, and the pool makes extranonce1 unique per client.
fn write_extranonce2(size: usize) -> Result<Text<16>, Error> {
    let mut text = [0u8; 16];
    text[..size * 2].fill(b'0');
    text[size * 2 - 1] = b'1';
    Text::new(core::str::from_utf8(&text[..size * 2]).map_err(|_| Error::Malformed)?)
}

/// Block height from the BIP34 push at the start of the coinbase script.
fn parse_height(coinb1: &str) -> Option<u32> {
    // version(4) + input count(1) + prevout(36) + script length(1)
    let push = coinb1.get(84..86)?;
    let mut len = [0u8; 1];
    hex::decode_into(push, &mut len).ok()?;
    let len = len[0] as usize;
    if !(1..=4).contains(&len) {
        return None;
    }
    let mut bytes = [0u8; 4];
    hex::decode_into(coinb1.get(86..86 + len * 2)?, &mut bytes[..len]).ok()?;
    Some(u32::from_le_bytes(bytes))
}

impl Job {
    pub fn build(notify: &Notify<'_>, session: &Session) -> Result<Self, Error> {
        let extranonce2 = write_extranonce2(session.extranonce2_size)?;

        let mut coinbase = Sha256::new();
        hex::hash_hex(&mut coinbase, notify.coinb1)?;
        hex::hash_hex(&mut coinbase, session.extranonce1.as_str())?;
        hex::hash_hex(&mut coinbase, extranonce2.as_str())?;
        hex::hash_hex(&mut coinbase, notify.coinb2)?;
        let mut root = sha256(&coinbase.finalize());

        let branches = notify.merkle.elements().ok_or(Error::Malformed)?;
        for branch in branches.take(MAX_MERKLE_BRANCHES) {
            let mut sibling = [0u8; 32];
            hex::decode_into(branch.as_str().ok_or(Error::Malformed)?, &mut sibling)?;
            let mut pair = [0u8; 64];
            pair[..32].copy_from_slice(&root);
            pair[32..].copy_from_slice(&sibling);
            root = crate::sha256::sha256d(&pair);
        }

        let mut header = [0u8; 80];
        hex::decode_into(notify.version, &mut header[..4])?;
        swap_word(&mut header[..4]);
        hex::decode_into(notify.prevhash, &mut header[4..36])?;
        for word in header[4..36].chunks_exact_mut(4) {
            swap_word(word);
        }
        header[36..68].copy_from_slice(&root);
        hex::decode_into(notify.ntime, &mut header[68..72])?;
        swap_word(&mut header[68..72]);
        hex::decode_into(notify.nbits, &mut header[72..76])?;
        swap_word(&mut header[72..76]);

        Ok(Self {
            job_id: Text::new(notify.job_id)?,
            extranonce2,
            ntime: Text::new(notify.ntime)?,
            header,
            target: nbits_target(hex::parse_u32(notify.nbits)?),
            height: parse_height(notify.coinb1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::{Work, sha256d};
    use crate::target::meets_target;

    /// A buffer-backed writer, as the firmware's `heapless::String` would be.
    struct Line(std::string::String);
    impl Write for Line {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.0.push_str(s);
            Ok(())
        }
    }

    const GENESIS_COINB1: &str = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4d04ffff001d";
    const GENESIS_COINB2_TAIL: &str = "455468652054696d65732030332f4a616e2f32303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e64206261696c6f757420666f722062616e6b73ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000";

    #[test]
    fn genesis_block_end_to_end() {
        let line = format!(
            r#"{{"id":null,"method":"mining.notify","params":["1","{}","{}","{}",[],"00000001","1d00ffff","495fab29",true]}}"#,
            "0".repeat(64),
            GENESIS_COINB1,
            GENESIS_COINB2_TAIL,
        );
        let Message::Notify(notify) = parse_line(&line).unwrap() else { panic!("not a notify") };
        assert!(notify.clean);

        let session = Session { extranonce1: Text::new("01").unwrap(), extranonce2_size: 1 };
        let coinb2 = format!("04{GENESIS_COINB2_TAIL}");
        let notify = Notify { coinb2: &coinb2, ..notify };
        let mut job = Job::build(&notify, &session).unwrap();

        // Rebuild the expected header independently of the byte shuffling above.
        let mut expected = [0u8; 80];
        hex::decode_into(
            "0100000000000000000000000000000000000000000000000000000000000000\
             000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa\
             4b1e5e4a29ab5f49ffff001d00000000",
            &mut expected,
        )
        .unwrap();
        assert_eq!(job.header[..36], expected[..36]);
        assert_eq!(job.header[68..76], expected[68..76]);

        // extranonce1 01 + extranonce2 01 is not the real `01 04`, so the merkle
        // root differs: swap in the true one to check the mined result.
        job.header[36..68].copy_from_slice(&expected[36..68]);
        let work = Work::from_header(&job.header);
        let nonce = 0x7c2b_ac1d;
        assert_eq!(work.scan(nonce - 10, 20, 0), Some(nonce));
        assert!(meets_target(&work.hash(nonce), &job.target));
    }

    #[test]
    fn coinbase_and_merkle_hashing() {
        // With no extranonce bytes in play the merkle root is the coinbase txid.
        let tx = format!("{GENESIS_COINB1}0104{GENESIS_COINB2_TAIL}");
        let mut raw = vec![0u8; tx.len() / 2];
        hex::decode_into(&tx, &mut raw).unwrap();
        let mut txid = sha256d(&raw);
        txid.reverse();
        let mut expected = [0u8; 32];
        hex::decode_into(
            "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            &mut expected,
        )
        .unwrap();
        assert_eq!(txid, expected);

        // Through Job::build, the fixed extranonce2 of 01 lands on the real `01` byte.
        let (coinb1, rest) = tx.split_at(GENESIS_COINB1.len());
        assert_eq!(&rest[..4], "0104");
        let session = Session { extranonce1: Text::empty(), extranonce2_size: 1 };
        let notify = Notify {
            job_id: "1",
            prevhash: &"0".repeat(64),
            coinb1,
            coinb2: &rest[2..],
            merkle: json::parse("[]").unwrap().0,
            version: "00000001",
            nbits: "1d00ffff",
            ntime: "495fab29",
            clean: true,
        };
        let job = Job::build(&notify, &session).unwrap();
        let mut root = [0u8; 32];
        root.copy_from_slice(&job.header[36..68]);
        root.reverse();
        assert_eq!(root, expected);
    }

    #[test]
    fn merkle_branches_fold_from_the_left() {
        let session = Session { extranonce1: Text::empty(), extranonce2_size: 1 };
        let merkle = format!("[\"{0}\",\"{0}\"]", "11".repeat(32));
        let notify = Notify {
            job_id: "1",
            prevhash: &"0".repeat(64),
            coinb1: "aa",
            coinb2: "bb",
            merkle: json::parse(&merkle).unwrap().0,
            version: "20000000",
            nbits: "1701e5ac",
            ntime: "6ae512a0",
            clean: false,
        };
        let job = Job::build(&notify, &session).unwrap();

        let mut node = sha256d(&[0xaa, 0x01, 0xbb]);
        for _ in 0..2 {
            let mut pair = [0x11u8; 64];
            pair[..32].copy_from_slice(&node);
            node = sha256d(&pair);
        }
        assert_eq!(job.header[36..68], node);
        assert_eq!(job.header[..4], [0, 0, 0, 0x20]);
    }

    #[test]
    fn real_public_pool_notify() {
        // Captured from public-pool.io; the branch list is shortened.
        let line = r#"{"id":null,"method":"mining.notify","params":["6571280","b8bcecce08252fdee9dd2075af692bb6a00c7749000048370000000000000000","02000000010000000000000000000000000000000000000000000000000000000000000000ffffffff1b033edc0e5075626c69632d506f6f6c","ffffffff025e15b012000000001976a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac0000000000000000266a24aa21a9edfc456e44fc1c5b84fcae4efde757c8b0c9544c764922a59b80c8f6bafc213e7500000000",["c352ecf14ceef2c69494d9dead3d583902f464981c1addb66f35eb46039a79bd","1de57a226987bfe1596ee4689eef27186ba2af6ea6799007b3a418f96f3c06c1"],"20000000","1701e5ac","6ae512a0",false]}"#;
        let Message::Notify(notify) = parse_line(line).unwrap() else { panic!("not a notify") };
        assert_eq!(notify.job_id, "6571280");
        assert!(!notify.clean);
        let session = Session { extranonce1: Text::new("dbc158f8").unwrap(), extranonce2_size: 8 };
        let job = Job::build(&notify, &session).unwrap();
        assert_eq!(job.height, Some(0x0edc3e));
        assert_eq!(job.extranonce2.as_str(), "0000000000000001");
        assert_eq!(job.header[68..72], [0xa0, 0x12, 0xe5, 0x6a]);
        assert_eq!(job.header[72..76], [0xac, 0xe5, 0x01, 0x17]);
        // prevhash arrives word-swapped: the last word holds the leading zeros.
        assert_eq!(job.header[4..8], [0xce, 0xec, 0xbc, 0xb8]);
        assert_eq!(job.header[32..36], [0, 0, 0, 0]);
    }

    #[test]
    fn replies_and_errors() {
        let Message::Reply { id, result, error } =
            parse_line(r#"{"id":1,"error":null,"result":[[["mining.notify","dbc158f8"]],"dbc158f8",8]}"#).unwrap()
        else {
            panic!("not a reply")
        };
        assert_eq!((id, error), (1, None));
        let session = parse_subscribe(&result).unwrap();
        assert_eq!(session.extranonce1.as_str(), "dbc158f8");
        assert_eq!(session.extranonce2_size, 8);

        assert_eq!(
            parse_line(r#"{"id":7,"result":null,"error":[23,"Difficulty too low",""]}"#).unwrap(),
            Message::Reply { id: 7, result: Value::Null, error: Some("Difficulty too low") }
        );
        assert_eq!(
            parse_line(r#"{"id":null,"method":"mining.set_difficulty","params":[0.00015]}"#).unwrap(),
            Message::SetDifficulty(0.00015)
        );
        assert_eq!(parse_line(r#"{"id":null,"method":"client.show_message","params":["hi"]}"#).unwrap(), Message::Ignored);
        assert!(parse_line(r#"{"id":null,"method":"mining.set_difficulty","params":[0]}"#).is_err());
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn request_shapes() {
        let mut line = Line(std::string::String::new());
        write_subscribe(&mut line, 1, "PicoMiner/0.1").unwrap();
        write_authorize(&mut line, 2, "bc1qaddr.pico", "x").unwrap();
        write_suggest_difficulty(&mut line, 3, 0.00015).unwrap();
        assert_eq!(
            line.0,
            "{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"PicoMiner/0.1\"]}\n\
             {\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qaddr.pico\",\"x\"]}\n\
             {\"id\":3,\"method\":\"mining.suggest_difficulty\",\"params\":[0.00015]}\n"
        );

        let job = Job {
            job_id: Text::new("6571280").unwrap(),
            extranonce2: Text::new("0000000000000001").unwrap(),
            ntime: Text::new("6ae512a0").unwrap(),
            header: [0; 80],
            target: [0; 32],
            height: None,
        };
        let mut line = Line(std::string::String::new());
        write_submit(&mut line, 9, "bc1qaddr.pico", &job, 0xab).unwrap();
        assert_eq!(
            line.0,
            "{\"id\":9,\"method\":\"mining.submit\",\"params\":[\"bc1qaddr.pico\",\"6571280\",\"0000000000000001\",\"6ae512a0\",\"000000ab\"]}\n"
        );
    }

    fn feed<const N: usize>(lines: &mut LineBuffer<N>, data: &[u8]) {
        let space = lines.space();
        space[..data.len()].copy_from_slice(data);
        lines.filled(data.len());
    }

    #[test]
    fn line_buffer_reassembles_and_splits() {
        let mut lines = LineBuffer::<32>::new();
        feed(&mut lines, b"{\"a\"");
        assert_eq!(lines.line_len(), None);
        feed(&mut lines, b":1}\n{\"b\":2}\npart");
        let first = lines.line_len().unwrap();
        assert_eq!(lines.line(first), "{\"a\":1}");
        lines.consume(first);
        let second = lines.line_len().unwrap();
        assert_eq!(lines.line(second), "{\"b\":2}");
        lines.consume(second);
        assert_eq!(lines.line_len(), None);
        feed(&mut lines, b"ial\n");
        let third = lines.line_len().unwrap();
        assert_eq!(lines.line(third), "partial");
    }

    #[test]
    fn line_buffer_drops_oversized_lines_whole() {
        let mut lines = LineBuffer::<8>::new();
        feed(&mut lines, b"12345678");
        assert_eq!(lines.line_len(), None);
        feed(&mut lines, b"90\nok\n");
        let len = lines.line_len().unwrap();
        assert_eq!(lines.line(len), "ok");
    }

    #[test]
    fn height_needs_a_sane_push() {
        assert_eq!(parse_height(""), None);
        assert_eq!(parse_height(&"0".repeat(84)), None);
    }

    #[test]
    fn text_limits() {
        assert_eq!(Text::<4>::new("abcd").unwrap().as_str(), "abcd");
        assert_eq!(Text::<4>::new("abcde"), Err(Error::TooLong));
    }
}
