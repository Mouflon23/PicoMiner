//! Wi-Fi association and the Stratum client.

use core::fmt::Write;

use cyw43::{Control, JoinOptions};
use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use log::{info, warn};
use picominer_core::settings::Settings;
use picominer_core::sha256::Work;
use picominer_core::stratum::{self, Job, LineBuffer, Message, Session};
use picominer_core::target::meets_target;
use static_cell::ConstStaticCell;

use crate::config;
use crate::miner;
use crate::stats::{self, Status};

const ID_SUBSCRIBE: u32 = 1;
const ID_AUTHORIZE: u32 = 2;
const ID_SUGGEST: u32 = 3;
const ID_FIRST_SUBMIT: u32 = 4;

const KEEPALIVE: Duration = Duration::from_secs(60);
const JOB_TIMEOUT: Duration = Duration::from_secs(600);
const RSSI_PERIOD: Duration = Duration::from_secs(30);

static RX: ConstStaticCell<[u8; 4096]> = ConstStaticCell::new([0; 4096]);
static TX: ConstStaticCell<[u8; 1024]> = ConstStaticCell::new([0; 1024]);
static LINES: ConstStaticCell<LineBuffer<4096>> = ConstStaticCell::new(LineBuffer::new());

type Request = String<320>;

async fn send(socket: &mut TcpSocket<'_>, request: &Request) -> Result<(), &'static str> {
    let mut rest = request.as_bytes();
    while !rest.is_empty() {
        let written = socket.write(rest).await.map_err(|_| "write failed")?;
        rest = &rest[written..];
    }
    Ok(())
}

async fn join(control: &mut Control<'static>, settings: &Settings) {
    stats::set_status(Status::Joining);
    loop {
        let options = if settings.wifi_password.is_empty() {
            JoinOptions::new_open()
        } else {
            JoinOptions::new(settings.wifi_password.as_bytes())
        };
        match control.join(&settings.wifi_ssid, options).await {
            Ok(()) => {
                info!("joined {}", settings.wifi_ssid.as_str());
                return;
            }
            Err(error) => {
                warn!("Wi-Fi join failed: {:?}", error);
                Timer::after_secs(5).await;
            }
        }
    }
}

async fn resolve(stack: Stack<'static>, host: &str) -> Result<IpAddress, &'static str> {
    let addresses = stack
        .dns_query(host, DnsQueryType::A)
        .await
        .map_err(|_| "DNS failed")?;
    addresses.first().copied().ok_or("no DNS answer")
}

/// One connection to the pool, from subscribe to the first error.
async fn session(
    stack: Stack<'static>,
    control: &mut Control<'static>,
    settings: &Settings,
    rx: &mut [u8],
    tx: &mut [u8],
    lines: &mut LineBuffer<4096>,
) -> Result<(), &'static str> {
    let address = resolve(stack, &settings.pool_host).await?;
    let port = settings.pool_port;
    if port == 0 {
        return Err("no pool port");
    }
    info!("connecting to {}:{}", address, port);

    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(180)));
    socket.set_keep_alive(Some(Duration::from_secs(30)));
    socket.connect((address, port)).await.map_err(|_| "connect failed")?;

    let mut user: String<128> = String::new();
    if settings.worker.is_empty() {
        write!(user, "{}", settings.btc_address)
    } else {
        write!(user, "{}.{}", settings.btc_address, settings.worker)
    }
    .map_err(|_| "user too long")?;

    let mut request = Request::new();
    stratum::write_subscribe(&mut request, ID_SUBSCRIBE, config::AGENT).map_err(|_| "subscribe")?;
    send(&mut socket, &request).await?;

    lines.clear();
    let mut session: Option<Session> = None;
    let mut job: Option<(u32, Job)> = None;
    let mut difficulty = 1.0;
    let mut next_id = ID_FIRST_SUBMIT;
    let mut last_send = Instant::now();
    let mut last_job = Instant::now();
    let mut last_rssi = Instant::from_ticks(0);
    let mut led_off = None;

    loop {
        match select(socket.read(lines.space()), Timer::after_millis(100)).await {
            Either::First(Ok(0)) => return Err("pool closed"),
            Either::First(Ok(count)) => lines.filled(count),
            Either::First(Err(_)) => return Err("read failed"),
            Either::Second(()) => {}
        }

        while let Some(len) = lines.line_len() {
            match stratum::parse_line(lines.line(len)) {
                Ok(Message::Notify(notify)) => {
                    last_job = Instant::now();
                    if let Some(current) = &session {
                        match Job::build(&notify, current) {
                            Ok(built) => {
                                let generation = if cfg!(feature = "stress") {
                                    0
                                } else {
                                    miner::publish(Work::from_header(&built.header))
                                };
                                if let Some(height) = built.height {
                                    stats::update(|stats| stats.height = height);
                                }
                                info!("job {} block {:?}", built.job_id.as_str(), built.height);
                                job = Some((generation, built));
                                stats::set_status(Status::Mining);
                            }
                            Err(error) => warn!("bad job: {:?}", error),
                        }
                    }
                }
                Ok(Message::SetDifficulty(value)) => {
                    info!("pool difficulty {}", value);
                    difficulty = value;
                    miner::set_difficulty(value);
                    stats::update(|stats| stats.pool_difficulty = value);
                }
                Ok(Message::Reply { id: ID_SUBSCRIBE, result, error }) => {
                    if let Some(reason) = error {
                        warn!("subscribe refused: {}", reason);
                        return Err("subscribe refused");
                    }
                    let subscribed = stratum::parse_subscribe(&result).map_err(|_| "subscribe reply")?;
                    info!(
                        "subscribed, extranonce1 {} extranonce2 {} bytes",
                        subscribed.extranonce1.as_str(),
                        subscribed.extranonce2_size,
                    );
                    session = Some(subscribed);
                    request.clear();
                    stratum::write_authorize(&mut request, ID_AUTHORIZE, &user, "x").map_err(|_| "authorize")?;
                    stratum::write_suggest_difficulty(&mut request, ID_SUGGEST, config::SUGGESTED_DIFFICULTY)
                        .map_err(|_| "suggest")?;
                    send(&mut socket, &request).await?;
                    last_send = Instant::now();
                }
                Ok(Message::Reply { id: ID_AUTHORIZE, result, error }) => {
                    if error.is_some() || result.as_bool() == Some(false) {
                        warn!("authorize refused: {:?}", error);
                        return Err("authorize refused");
                    }
                    info!("authorized as {}", user.as_str());
                }
                Ok(Message::Reply { id, result, error }) if id >= ID_FIRST_SUBMIT => {
                    let accepted = error.is_none() && result.as_bool() != Some(false);
                    if accepted {
                        info!("share {} accepted", id);
                        control.gpio_set(0, true).await;
                        led_off = Some(Instant::now() + Duration::from_millis(200));
                    } else {
                        warn!("share {} rejected: {:?}", id, error);
                    }
                    stats::update(|stats| {
                        if accepted {
                            stats.accepted += 1;
                        } else {
                            stats.rejected += 1;
                        }
                    });
                }
                Ok(_) => {}
                Err(error) => warn!("bad line: {:?}", error),
            }
            lines.consume(len);
        }

        while let Some(found) = miner::take_found() {
            let Some((generation, current)) = &job else { continue };
            if found.generation != *generation || found.difficulty < difficulty {
                continue;
            }
            if meets_target(&found.hash, &current.target) {
                warn!("BLOCK FOUND: nonce {:08x}", found.nonce);
                stats::update(|stats| stats.blocks += 1);
            }
            info!("submitting diff {:.6} as {}", found.difficulty, next_id);
            request.clear();
            stratum::write_submit(&mut request, next_id, &user, current, found.nonce).map_err(|_| "submit")?;
            send(&mut socket, &request).await?;
            next_id += 1;
            last_send = Instant::now();
        }

        let now = Instant::now();
        if led_off.is_some_and(|at| now >= at) {
            led_off = None;
            control.gpio_set(0, false).await;
        }
        if now - last_rssi >= RSSI_PERIOD {
            last_rssi = now;
            let rssi = control.get_rssi().await;
            stats::update(|stats| stats.rssi = rssi);
        }
        if now - last_send >= KEEPALIVE {
            request.clear();
            stratum::write_suggest_difficulty(&mut request, ID_SUGGEST, config::SUGGESTED_DIFFICULTY)
                .map_err(|_| "suggest")?;
            send(&mut socket, &request).await?;
            last_send = now;
        }
        if now - last_job >= JOB_TIMEOUT {
            return Err("no job for 10 minutes");
        }
    }
}

#[embassy_executor::task]
pub async fn run(stack: Stack<'static>, mut control: Control<'static>, settings: &'static Settings) -> ! {
    let (rx, tx, lines) = (RX.take(), TX.take(), LINES.take());
    loop {
        if !stack.is_link_up() {
            join(&mut control, settings).await;
        }
        stats::set_status(Status::Dhcp);
        stack.wait_config_up().await;
        if let Some(config) = stack.config_v4() {
            info!("address {}", config.address);
            let octets = config.address.address().octets();
            stats::update(|stats| stats.ip = octets);
        }

        stats::set_status(Status::Connecting);
        let started = Instant::now();
        if let Err(reason) = session(stack, &mut control, settings, rx, tx, lines).await {
            warn!(
                "pool session ended after {} s, {} s since boot, link {}, address {}: {}",
                started.elapsed().as_secs(),
                Instant::now().as_secs(),
                stack.is_link_up(),
                stack.is_config_up(),
                reason,
            );
        }
        miner::stop();
        stats::set_status(Status::PoolError);
        Timer::after_secs(3).await;
    }
}
