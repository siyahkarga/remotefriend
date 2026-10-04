//! Direct (peer-to-peer) path for a session: a WebRTC data channel (ICE + DTLS + SCTP, str0m).
//!
//! Every session starts through the relay. The viewer then offers a data channel inside the
//! end-to-end encrypted session, so the relay can neither read nor change the offer and its
//! DTLS fingerprint. When both sides can reach each other (same network, or NATs that allow
//! hole punching with the public addresses learnt from the relay's STUN), video, sound and
//! input flow directly; the session's own end-to-end encryption still applies, with separate
//! keys. If no direct path opens within a few seconds, the session simply stays on the relay.

use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

/// Fragment size on the data channel (every browser accepts messages this big).
const FRAGMENT: usize = 16 * 1024;
/// Don't take new messages while this much waits in the channel (backpressure).
const MAX_BUFFERED: usize = 1024 * 1024;
/// A reassembled message larger than this is refused.
const MAX_MESSAGE: usize = 32 * 1024 * 1024;
/// Give up when no direct path opened within this time.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// An open direct path carrying complete messages (end-to-end encrypted by the caller).
pub struct Link {
    pub out: mpsc::Sender<Vec<u8>>,
    pub inc: mpsc::Receiver<Vec<u8>>,
}

/// Sending side of a session on the direct path: the session's direct-path cipher + the link.
pub struct DirectOut {
    pub cipher: remote_friend_common::e2e::Cipher,
    pub out: mpsc::Sender<Vec<u8>>,
}

impl DirectOut {
    /// Encrypt and send; false when the direct path is gone (use the relay again).
    pub async fn send(&mut self, plain: &[u8]) -> bool {
        match self.cipher.encrypt(plain) {
            Ok(ct) => self.out.send(ct).await.is_ok(),
            Err(_) => false,
        }
    }
}

/// Direct connections allowed on this computer? (Settings; on by default.)
pub fn enabled() -> bool {
    !remote_friend_common::identity::load_host_settings().direct_off
}

/// The relay's STUN service (same host as the relay, UDP 3478), if a relay is configured.
pub async fn stun_server() -> Option<SocketAddr> {
    let server = std::env::var("RF_RV_SERVER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| remote_friend_common::identity::load_rv_config().server);
    let host = server.trim().rsplit_once(':').map(|(h, _)| h).unwrap_or(server.trim()).trim_matches(['[', ']']).to_string();
    if host.is_empty() {
        return None;
    }
    let port = std::env::var("RF_STUN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(remote_friend_common::stun::PORT);
    let mut addrs = tokio::net::lookup_host((host.as_str(), port)).await.ok()?;
    addrs.find(|a| a.is_ipv4())
}

fn crypto() -> Arc<str0m::crypto::CryptoProvider> {
    static P: OnceLock<Arc<str0m::crypto::CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| Arc::new(str0m::crypto::from_feature_flags())).clone()
}

fn new_rtc() -> Rtc {
    RtcConfig::new().set_crypto_provider(crypto()).build(Instant::now())
}

/// A UDP socket on the local network address, offered as a host candidate plus, if the relay's
/// STUN answers, the public (server reflexive) address of the same socket.
async fn prepare(rtc: &mut Rtc, stun: Option<SocketAddr>) -> Result<UdpSocket> {
    let ip = local_ip_address::local_ip().context("no local network address")?;
    let sock = UdpSocket::bind(SocketAddr::new(ip, 0)).await?;
    let local = sock.local_addr()?;
    rtc.add_local_candidate(Candidate::host(local, "udp")?);
    if let Some(server) = stun {
        match stun_query(&sock, server).await {
            Some(public) if public != local => {
                if let Ok(c) = Candidate::server_reflexive(public, local, "udp") {
                    rtc.add_local_candidate(c);
                }
            }
            Some(_) => {}
            None => tracing::debug!("STUN {server} did not answer"),
        }
    }
    Ok(sock)
}

/// Ask the STUN server for the public address of `sock`.
async fn stun_query(sock: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    use remote_friend_common::stun;
    let txid: [u8; 12] = rand::random();
    let req = stun::binding_request(&txid);
    let mut buf = [0u8; 600];
    for _ in 0..3 {
        let _ = sock.send_to(&req, server).await;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
            if from == server {
                if let Some(addr) = stun::parse_response(&buf[..n], &txid) {
                    return Some(addr);
                }
            }
        }
    }
    None
}

/// Computer side: answer the viewer's offer. Returns the answer SDP; the link arrives once
/// the channel is open (the receiver fails if no direct path comes up).
pub async fn answer(offer: &str, stun: Option<SocketAddr>) -> Result<(String, oneshot::Receiver<Link>)> {
    let offer = SdpOffer::from_sdp_string(offer).context("invalid offer")?;
    let mut rtc = new_rtc();
    let sock = prepare(&mut rtc, stun).await?;
    let answer = rtc.sdp_api().accept_offer(offer).context("offer not accepted")?;
    let (open_tx, open_rx) = oneshot::channel();
    tokio::spawn(drive(rtc, sock, None, open_tx));
    Ok((answer.to_sdp_string(), open_rx))
}

/// Viewer side (app): an offer with one data channel. Send the computer's answer into the
/// returned sender; the link arrives once the channel is open.
pub async fn offer(stun: Option<SocketAddr>) -> Result<(String, oneshot::Sender<String>, oneshot::Receiver<Link>)> {
    let mut rtc = new_rtc();
    let sock = prepare(&mut rtc, stun).await?;
    let mut api = rtc.sdp_api();
    api.add_channel("rf".into());
    let (offer, pending) = api.apply().context("cannot create an offer")?;
    let (answer_tx, answer_rx) = oneshot::channel();
    let (open_tx, open_rx) = oneshot::channel();
    tokio::spawn(drive(rtc, sock, Some((pending, answer_rx)), open_tx));
    Ok((offer.to_sdp_string(), answer_tx, open_rx))
}

async fn drive(
    rtc: Rtc,
    sock: UdpSocket,
    pending: Option<(SdpPendingOffer, oneshot::Receiver<String>)>,
    open_tx: oneshot::Sender<Link>,
) {
    match run(rtc, sock, pending, open_tx).await {
        Ok(()) => tracing::debug!("direct path closed"),
        Err(e) => tracing::debug!("direct path ended: {e:#}"),
    }
}

async fn recv_opt<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

async fn answer_opt(rx: &mut Option<oneshot::Receiver<String>>) -> Option<String> {
    match rx {
        Some(r) => {
            let a = r.await.ok();
            if a.is_none() {
                *rx = None;
            }
            a
        }
        None => std::future::pending().await,
    }
}

/// Drives one peer connection: UDP in/out, timers, and the data channel. Messages are split
/// into fragments of up to 16 KiB, each prefixed with 1 (more follow) or 0 (last).
async fn run(
    mut rtc: Rtc,
    sock: UdpSocket,
    pending: Option<(SdpPendingOffer, oneshot::Receiver<String>)>,
    open_tx: oneshot::Sender<Link>,
) -> Result<()> {
    let local = sock.local_addr()?;
    let started = Instant::now();
    let (mut pending_offer, mut answer_rx) = match pending {
        Some((p, rx)) => (Some(p), Some(rx)),
        None => (None, None),
    };
    let mut open_tx = Some(open_tx);
    let mut channel: Option<ChannelId> = None;
    let mut out_rx: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut inc_tx: Option<mpsc::Sender<Vec<u8>>> = None;
    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut partial: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        // Hand queued fragments to the channel while it has room.
        if let Some(id) = channel {
            while let Some(frag) = queue.front() {
                let Some(mut ch) = rtc.channel(id) else { return Ok(()) };
                if ch.write(true, frag)? {
                    queue.pop_front();
                } else {
                    break;
                }
            }
        }
        // Send what is due and react to events.
        let deadline = loop {
            match rtc.poll_output()? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    let _ = sock.send_to(&t.contents, t.destination).await;
                }
                Output::Event(ev) => match ev {
                    Event::ChannelOpen(id, _) => {
                        channel = Some(id);
                        let (otx, orx) = mpsc::channel(64);
                        let (itx, irx) = mpsc::channel(256);
                        out_rx = Some(orx);
                        inc_tx = Some(itx);
                        if let Some(tx) = open_tx.take() {
                            if tx.send(Link { out: otx, inc: irx }).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Event::ChannelData(d) => {
                        let Some((&more, data)) = d.data.split_first() else { continue };
                        partial.extend_from_slice(data);
                        anyhow::ensure!(partial.len() <= MAX_MESSAGE, "message too large");
                        if more == 0 {
                            let msg = std::mem::take(&mut partial);
                            if let Some(tx) = &inc_tx {
                                if tx.send(msg).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    Event::ChannelClose(_) => return Ok(()),
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return Ok(()),
                    _ => {}
                },
            }
        };
        if channel.is_none() && started.elapsed() > CONNECT_TIMEOUT {
            anyhow::bail!("no direct path within {} s", CONNECT_TIMEOUT.as_secs());
        }
        let room = queue.is_empty()
            && channel.is_some_and(|id| rtc.channel(id).is_some_and(|mut c| c.buffered_amount() < MAX_BUFFERED));
        let wait = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let (n, source) = r?;
                if let Ok(contents) = buf[..n].try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents });
                    if rtc.accepts(&input) {
                        rtc.handle_input(input)?;
                    }
                }
            }
            _ = tokio::time::sleep(wait) => rtc.handle_input(Input::Timeout(Instant::now()))?,
            m = recv_opt(&mut out_rx), if room => match m {
                Some(msg) => {
                    let mut chunks = msg.chunks(FRAGMENT).peekable();
                    if chunks.peek().is_none() {
                        queue.push_back(vec![0]);
                    }
                    while let Some(chunk) = chunks.next() {
                        let mut f = Vec::with_capacity(chunk.len() + 1);
                        f.push(if chunks.peek().is_some() { 1 } else { 0 });
                        f.extend_from_slice(chunk);
                        queue.push_back(f);
                    }
                }
                None => return Ok(()), // the session no longer uses the link
            },
            a = answer_opt(&mut answer_rx) => {
                if let (Some(sdp), Some(p)) = (a, pending_offer.take()) {
                    let answer = SdpAnswer::from_sdp_string(&sdp).context("invalid answer")?;
                    rtc.sdp_api().accept_answer(p, answer).context("answer not accepted")?;
                    answer_rx = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two peers on this machine: offer, answer, open, and a large message both ways.
    #[tokio::test]
    async fn direct_link_carries_large_messages() {
        if local_ip_address::local_ip().is_err() {
            return; // no network interface (sandbox)
        }
        let (offer_sdp, answer_tx, viewer_open) = offer(None).await.unwrap();
        let (answer_sdp, host_open) = answer(&offer_sdp, None).await.unwrap();
        answer_tx.send(answer_sdp).unwrap();
        let wait = Duration::from_secs(10);
        let mut v = tokio::time::timeout(wait, viewer_open).await.unwrap().unwrap();
        let mut h = tokio::time::timeout(wait, host_open).await.unwrap().unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        v.out.send(big.clone()).await.unwrap();
        assert_eq!(tokio::time::timeout(wait, h.inc.recv()).await.unwrap().unwrap(), big);
        h.out.send(b"pong".to_vec()).await.unwrap();
        assert_eq!(tokio::time::timeout(wait, v.inc.recv()).await.unwrap().unwrap(), b"pong");
    }
}
