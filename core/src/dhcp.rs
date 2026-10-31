//! The smallest DHCP server that gets a phone or a laptop onto the setup access
//! point: it hands each client an address and points it at the Pico.

const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const OPTIONS: usize = 240;
/// Some clients ignore BOOTP replies shorter than this.
const MIN_REPLY: usize = 300;
const LEASE_SECONDS: u32 = 3600;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

const OPTION_PAD: u8 = 0;
const OPTION_SUBNET: u8 = 1;
const OPTION_ROUTER: u8 = 3;
const OPTION_DNS: u8 = 6;
const OPTION_REQUESTED_IP: u8 = 50;
const OPTION_LEASE: u8 = 51;
const OPTION_MESSAGE_TYPE: u8 = 53;
const OPTION_SERVER_ID: u8 = 54;
const OPTION_END: u8 = 255;

/// Bytes a reply can take.
pub const REPLY_SIZE: usize = 320;

/// Addresses `.10` and up of the server's /24, one per client hardware address.
pub struct Leases<const N: usize> {
    clients: [Option<[u8; 6]>; N],
    next: usize,
}

impl<const N: usize> Default for Leases<N> {
    fn default() -> Self {
        Self { clients: [None; N], next: 0 }
    }
}

impl<const N: usize> Leases<N> {
    fn address(&mut self, server: [u8; 4], client: [u8; 6]) -> [u8; 4] {
        let index = match self.clients.iter().position(|known| *known == Some(client)) {
            Some(index) => index,
            None => {
                // Past N clients the oldest lease is handed out again.
                let index = self.next;
                self.clients[index] = Some(client);
                self.next = (index + 1) % N;
                index
            }
        };
        [server[0], server[1], server[2], 10 + index as u8]
    }
}

fn find_option(options: &[u8], code: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i < options.len() {
        match options[i] {
            OPTION_PAD => i += 1,
            OPTION_END => return None,
            found => {
                let len = *options.get(i + 1)? as usize;
                let value = options.get(i + 2..i + 2 + len)?;
                if found == code {
                    return Some(value);
                }
                i += 2 + len;
            }
        }
    }
    None
}

/// Answers a DISCOVER with an OFFER and a REQUEST with an ACK, or a NAK when the
/// client asks for an address it does not have here. Returns the reply length,
/// to broadcast to port 68, or `None` when there is nothing to send.
pub fn respond<const N: usize>(
    request: &[u8],
    server: [u8; 4],
    leases: &mut Leases<N>,
    reply: &mut [u8; REPLY_SIZE],
) -> Option<usize> {
    if request.len() < OPTIONS || request[0] != 1 || request[236..240] != MAGIC_COOKIE {
        return None;
    }
    let options = &request[OPTIONS..];
    let client: [u8; 6] = request[28..34].try_into().ok()?;
    let address = leases.address(server, client);
    let kind = match *find_option(options, OPTION_MESSAGE_TYPE)?.first()? {
        DISCOVER => OFFER,
        REQUEST => {
            if find_option(options, OPTION_SERVER_ID).is_some_and(|id| id != server) {
                // The client picked another server's offer.
                return None;
            }
            let ciaddr = &request[12..16];
            let wanted = find_option(options, OPTION_REQUESTED_IP).unwrap_or(ciaddr);
            if wanted == address || wanted == [0; 4] { ACK } else { NAK }
        }
        _ => return None,
    };

    reply.fill(0);
    reply[0] = 2;
    reply[1..3].copy_from_slice(&request[1..3]);
    reply[4..8].copy_from_slice(&request[4..8]);
    reply[10..12].copy_from_slice(&request[10..12]);
    reply[28..44].copy_from_slice(&request[28..44]);
    reply[236..240].copy_from_slice(&MAGIC_COOKIE);
    if kind != NAK {
        reply[16..20].copy_from_slice(&address);
        reply[20..24].copy_from_slice(&server);
    }
    let mut at = OPTIONS;
    let mut put = |code: u8, value: &[u8]| {
        reply[at] = code;
        reply[at + 1] = value.len() as u8;
        reply[at + 2..at + 2 + value.len()].copy_from_slice(value);
        at += 2 + value.len();
    };
    put(OPTION_MESSAGE_TYPE, &[kind]);
    put(OPTION_SERVER_ID, &server);
    if kind != NAK {
        put(OPTION_LEASE, &LEASE_SECONDS.to_be_bytes());
        put(OPTION_SUBNET, &[255, 255, 255, 0]);
        put(OPTION_ROUTER, &server);
        put(OPTION_DNS, &server);
    }
    put(OPTION_END, &[]);
    Some((at - 1).max(MIN_REPLY))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: [u8; 4] = [192, 168, 4, 1];
    const MAC: [u8; 6] = [2, 0, 0, 0, 0, 7];

    fn request(kind: u8, extra: &[u8]) -> std::vec::Vec<u8> {
        let mut packet = std::vec![0; OPTIONS];
        packet[0] = 1;
        packet[1] = 1;
        packet[2] = 6;
        packet[4..8].copy_from_slice(&[1, 2, 3, 4]);
        packet[28..34].copy_from_slice(&MAC);
        packet[236..240].copy_from_slice(&MAGIC_COOKIE);
        packet.extend_from_slice(&[OPTION_MESSAGE_TYPE, 1, kind]);
        packet.extend_from_slice(extra);
        packet.push(OPTION_END);
        packet
    }

    #[test]
    fn offers_then_acks() {
        let mut leases = Leases::<4>::default();
        let mut reply = [0; REPLY_SIZE];

        let len = respond(&request(DISCOVER, &[]), SERVER, &mut leases, &mut reply).unwrap();
        assert_eq!(len, MIN_REPLY);
        assert_eq!(reply[0], 2);
        assert_eq!(reply[4..8], [1, 2, 3, 4]);
        assert_eq!(reply[16..20], [192, 168, 4, 10]);
        assert_eq!(reply[28..34], MAC);
        assert_eq!(find_option(&reply[OPTIONS..], OPTION_MESSAGE_TYPE), Some(&[OFFER][..]));
        assert_eq!(find_option(&reply[OPTIONS..], OPTION_ROUTER), Some(&SERVER[..]));

        let wanted = [OPTION_REQUESTED_IP, 4, 192, 168, 4, 10, OPTION_SERVER_ID, 4, 192, 168, 4, 1];
        respond(&request(REQUEST, &wanted), SERVER, &mut leases, &mut reply).unwrap();
        assert_eq!(find_option(&reply[OPTIONS..], OPTION_MESSAGE_TYPE), Some(&[ACK][..]));
        assert_eq!(reply[16..20], [192, 168, 4, 10]);
    }

    #[test]
    fn naks_a_foreign_address_and_ignores_other_servers() {
        let mut leases = Leases::<4>::default();
        let mut reply = [0; REPLY_SIZE];
        let foreign = [OPTION_REQUESTED_IP, 4, 10, 0, 0, 5];
        respond(&request(REQUEST, &foreign), SERVER, &mut leases, &mut reply).unwrap();
        assert_eq!(find_option(&reply[OPTIONS..], OPTION_MESSAGE_TYPE), Some(&[NAK][..]));
        assert_eq!(reply[16..20], [0; 4]);

        let other = [OPTION_SERVER_ID, 4, 10, 0, 0, 1];
        assert_eq!(respond(&request(REQUEST, &other), SERVER, &mut leases, &mut reply), None);
    }

    #[test]
    fn keeps_one_address_per_client() {
        let mut leases = Leases::<2>::default();
        assert_eq!(leases.address(SERVER, [1; 6]), [192, 168, 4, 10]);
        assert_eq!(leases.address(SERVER, [2; 6]), [192, 168, 4, 11]);
        assert_eq!(leases.address(SERVER, [1; 6]), [192, 168, 4, 10]);
        assert_eq!(leases.address(SERVER, [3; 6]), [192, 168, 4, 10]);
    }

    #[test]
    fn ignores_garbage() {
        let mut leases = Leases::<2>::default();
        let mut reply = [0; REPLY_SIZE];
        assert_eq!(respond(&[1; 100], SERVER, &mut leases, &mut reply), None);
        let mut reply_packet = request(DISCOVER, &[]);
        reply_packet[0] = 2;
        assert_eq!(respond(&reply_packet, SERVER, &mut leases, &mut reply), None);
    }
}
