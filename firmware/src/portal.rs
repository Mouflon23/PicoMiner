//! The setup portal, after the one of NerdMiner v2: the Pico opens its own Wi-Fi
//! network, PicoMinerAP, and serves a form for the network to join, the BTC
//! address and the pool. Saving reboots into mining. It runs when nothing is
//! configured, or when KEY0 is held at power-up.

use core::fmt::Write;

use cyw43::{Control, ScanOptions};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_time::{Duration, Timer};
use heapless::{String, Vec};
use log::{info, warn};
use picominer_core::dhcp::{self, Leases, REPLY_SIZE};
use picominer_core::http::{self, Parsed, Request};
use picominer_core::settings::{Html, Settings};

use crate::{boot, settings};

pub const AP_SSID: &str = "PicoMinerAP";
pub const AP_PASSWORD: &str = "MineYourCoins";
pub const ADDRESS: [u8; 4] = [192, 168, 4, 1];
const CHANNEL: u8 = 6;

pub type Networks = Vec<String<32>, 12>;

pub fn net_config() -> embassy_net::Config {
    let [a, b, c, d] = ADDRESS;
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(a, b, c, d), 24),
        gateway: None,
        dns_servers: Vec::new(),
    })
}

/// Names of the networks in range, strongest first, for the form to suggest.
/// Done before the access point starts, while the radio is still a station.
pub async fn scan(control: &mut Control<'static>) -> Networks {
    let mut found: Vec<(i16, String<32>), 32> = Vec::new();
    let mut scanner = control.scan(ScanOptions::default()).await;
    loop {
        let bss = match select(scanner.next(), Timer::after_secs(10)).await {
            Either::First(Some(bss)) => bss,
            _ => break,
        };
        let (ssid, rssi) = (bss.ssid, bss.rssi);
        let Ok(name) = core::str::from_utf8(&ssid[..(bss.ssid_len as usize).min(32)]) else { continue };
        if name.is_empty() {
            continue;
        }
        match found.iter_mut().find(|(_, known)| known == name) {
            Some(entry) => entry.0 = entry.0.max(rssi),
            None => {
                let _ = found.push((rssi, name.try_into().unwrap_or_default()));
            }
        }
    }
    found.sort_unstable_by_key(|&(rssi, _)| -rssi);
    found.into_iter().map(|(_, name)| name).take(12).collect()
}

pub async fn start_access_point(control: &mut Control<'static>) {
    control.start_ap_wpa2(AP_SSID, AP_PASSWORD, CHANNEL).await;
    info!("setup portal: join {} with password {}, then open http://192.168.4.1", AP_SSID, AP_PASSWORD);
}

pub fn spawn(
    spawner: Spawner,
    stack: Stack<'static>,
    control: Control<'static>,
    current: &'static Settings,
    networks: &'static Networks,
) {
    spawner.spawn(dhcp_server(stack).unwrap());
    spawner.spawn(http_server(stack, current, networks).unwrap());
    spawner.spawn(http_server(stack, current, networks).unwrap());
    spawner.spawn(blink(control).unwrap());
}

/// A slow blink tells the portal apart from mining, where the LED flashes on shares.
#[embassy_executor::task]
async fn blink(mut control: Control<'static>) -> ! {
    loop {
        control.gpio_set(0, true).await;
        Timer::after_millis(500).await;
        control.gpio_set(0, false).await;
        Timer::after_millis(1500).await;
    }
}

#[embassy_executor::task]
async fn dhcp_server(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx = [0; 1536];
    let mut tx = [0; 1024];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    socket.bind(67).unwrap();

    let mut leases = Leases::<8>::default();
    let mut packet = [0; 768];
    let mut reply = [0; REPLY_SIZE];
    let broadcast = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68);
    loop {
        let Ok((len, _)) = socket.recv_from(&mut packet).await else { continue };
        if let Some(len) = dhcp::respond(&packet[..len], ADDRESS, &mut leases, &mut reply)
            && socket.send_to(&reply[..len], broadcast).await.is_err()
        {
            warn!("DHCP reply not sent");
        }
    }
}

type Page = String<6144>;

#[embassy_executor::task(pool_size = 2)]
async fn http_server(stack: Stack<'static>, current: &'static Settings, networks: &'static Networks) -> ! {
    let mut rx = [0; 1024];
    let mut tx = [0; 1024];
    let mut request = [0; 2048];
    let mut page = Page::new();
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(Duration::from_secs(10)));
        if socket.accept(80).await.is_err() {
            continue;
        }
        let saved = serve(&mut socket, &mut request, &mut page, current, networks).await;
        let _ = socket.flush().await;
        socket.close();
        Timer::after_millis(200).await;
        socket.abort();
        let _ = socket.flush().await;

        if let Some(saved) = saved {
            info!("settings saved, joining {} after a restart", saved.wifi_ssid.as_str());
            settings::stage(&saved);
            Timer::after_secs(1).await;
            boot::retry();
        }
    }
}

/// Answers one request. Returns the new settings once the form is accepted.
async fn serve(
    socket: &mut TcpSocket<'_>,
    buffer: &mut [u8],
    page: &mut Page,
    current: &Settings,
    networks: &Networks,
) -> Option<Settings> {
    let mut len = 0;
    loop {
        match socket.read(&mut buffer[len..]).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => len += count,
        }
        match http::parse(&buffer[..len]) {
            Parsed::Incomplete if len < buffer.len() => {}
            _ => break,
        }
    }

    page.clear();
    let mut saved = None;
    let status = match http::parse(&buffer[..len]) {
        Parsed::Done(Request { method: "POST", path: "/save", body }) => match current.with_form(body) {
            Ok(settings) => {
                let _ = saved_page(page, &settings);
                saved = Some(settings);
                "200 OK"
            }
            Err(reason) => {
                let _ = form_page(page, current, networks, Some(reason));
                "200 OK"
            }
        },
        Parsed::Done(Request { method: "GET", .. }) => {
            let _ = form_page(page, current, networks, None);
            "200 OK"
        }
        Parsed::Done(_) => "405 Method Not Allowed",
        _ => "400 Bad Request",
    };

    let mut head: String<192> = String::new();
    let _ = write!(
        head,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        page.len(),
    );
    for part in [head.as_bytes(), page.as_bytes()] {
        let mut rest = part;
        while !rest.is_empty() {
            match socket.write(rest).await {
                Ok(0) | Err(_) => return None,
                Ok(written) => rest = &rest[written..],
            }
        }
    }
    saved
}

const HEAD: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>PicoMiner setup</title><style>\
body{font-family:sans-serif;max-width:28em;margin:1em auto;padding:0 1em}\
label{display:block;margin-top:.8em}\
input{display:block;width:100%;box-sizing:border-box;padding:.4em;font-size:1em}\
button{margin-top:1.4em;padding:.6em;width:100%;font-size:1em}\
.error{color:#b00020}small{color:#666}\
</style></head><body><h1>PicoMiner</h1>";

fn form_page(page: &mut Page, current: &Settings, networks: &Networks, error: Option<&str>) -> core::fmt::Result {
    page.push_str(HEAD).map_err(|_| core::fmt::Error)?;
    if let Some(error) = error {
        write!(page, "<p class=\"error\">{}</p>", Html(error))?;
    }
    write!(
        page,
        "<form method=\"post\" action=\"/save\">\
         <label>Wi-Fi network<input name=\"ssid\" list=\"networks\" maxlength=\"32\" required value=\"{}\"></label>\
         <datalist id=\"networks\">",
        Html(&current.wifi_ssid),
    )?;
    for network in networks {
        write!(page, "<option value=\"{}\">", Html(network))?;
    }
    let password_hint = if current.wifi_password.is_empty() { "" } else { "Unchanged if left empty" };
    write!(
        page,
        "</datalist>\
         <label>Wi-Fi password<input name=\"password\" type=\"password\" maxlength=\"64\" placeholder=\"{password_hint}\"></label>\
         <label>BTC address<input name=\"address\" maxlength=\"90\" required value=\"{}\"></label>\
         <label>Pool host<input name=\"host\" maxlength=\"64\" required value=\"{}\"></label>\
         <label>Pool port<input name=\"port\" type=\"number\" min=\"1\" max=\"65535\" required value=\"{}\"></label>\
         <label>Worker name <small>optional</small><input name=\"worker\" maxlength=\"32\" value=\"{}\"></label>\
         <button>Save and restart</button></form>\
         <p><small>Pools that go below difficulty 1, such as pool.solomining.de:3333, show shares within minutes.</small></p>\
         </body></html>",
        Html(&current.btc_address),
        Html(&current.pool_host),
        current.pool_port,
        Html(&current.worker),
    )
}

fn saved_page(page: &mut Page, saved: &Settings) -> core::fmt::Result {
    page.push_str(HEAD).map_err(|_| core::fmt::Error)?;
    write!(
        page,
        "<p>Saved. PicoMiner restarts, joins <b>{}</b> and starts mining on {}:{}.</p>\
         <p>You can leave {AP_SSID} now. To come back here, hold KEY0 while plugging the Pico in.</p>\
         </body></html>",
        Html(&saved.wifi_ssid),
        Html(&saved.pool_host),
        saved.pool_port,
    )
}
