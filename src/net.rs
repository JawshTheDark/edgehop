//! Encrypted transport (Noise XX over TCP) and LAN discovery (UDP broadcast).
//!
//! Every install has a long-term X25519 identity key. The Noise XX handshake
//! mutually authenticates both static keys; the server only accepts keys it has
//! paired with, and the client pins the server key it first paired with.
//! Both sides derive a 6-digit pairing code from the handshake hash so a human
//! can confirm they are talking to each other (a MITM would produce different codes).

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use snow::{Builder, HandshakeState, StatelessTransportState};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 24800;
pub const DISCOVERY_PORT: u16 = 24801;
const PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const MAX_NOISE_MSG: usize = 65535;
const CHUNK: usize = 60_000;
const MAX_APP_MSG: usize = 16 * 1024 * 1024;
const DISCOVER_MAGIC: &[u8] = b"KVMLINK-DISCOVER-1";

fn err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Short human-readable fingerprint of a public key (hex string).
pub fn fingerprint(key_hex: &str) -> String {
    let k = key_hex.to_uppercase();
    let k = &k[..k.len().min(16)];
    k.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn generate_keypair() -> (Vec<u8>, Vec<u8>) {
    let kp = Builder::new(PARAMS.parse().expect("noise params"))
        .generate_keypair()
        .expect("keypair");
    (kp.private, kp.public)
}

fn write_raw(s: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(2 + data.len());
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    s.write_all(&buf)
}

fn read_raw(s: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut l = [0u8; 2];
    s.read_exact(&mut l)?;
    let mut b = vec![0u8; u16::from_be_bytes(l) as usize];
    s.read_exact(&mut b)?;
    Ok(b)
}

pub struct FrameSender {
    stream: TcpStream,
    t: Arc<Mutex<StatelessTransportState>>,
    nonce: u64,
    buf: Vec<u8>,
}

pub struct FrameReceiver {
    stream: TcpStream,
    t: Arc<Mutex<StatelessTransportState>>,
    nonce: u64,
    pending: Vec<u8>,
    buf: Vec<u8>,
}

pub struct Handshaked {
    pub tx: FrameSender,
    pub rx: FrameReceiver,
    pub remote_key: String,
    pub code: String,
}

impl FrameSender {
    pub fn send<T: Serialize>(&mut self, msg: &T) -> io::Result<()> {
        let body = serde_json::to_vec(msg).map_err(err)?;
        let mut plain = Vec::with_capacity(4 + body.len());
        plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
        plain.extend_from_slice(&body);
        let mut out = Vec::with_capacity(plain.len() + 64);
        {
            let t = self.t.lock().unwrap_or_else(|e| e.into_inner());
            for chunk in plain.chunks(CHUNK) {
                let n = t
                    .write_message(self.nonce, chunk, &mut self.buf)
                    .map_err(err)?;
                self.nonce += 1;
                out.extend_from_slice(&(n as u16).to_be_bytes());
                out.extend_from_slice(&self.buf[..n]);
            }
        }
        self.stream.write_all(&out)
    }
}

impl FrameReceiver {
    pub fn recv<T: DeserializeOwned>(&mut self) -> io::Result<T> {
        loop {
            if self.pending.len() >= 4 {
                let n = u32::from_be_bytes(self.pending[..4].try_into().unwrap()) as usize;
                if n > MAX_APP_MSG {
                    return Err(err("message too large"));
                }
                if self.pending.len() >= 4 + n {
                    let msg = serde_json::from_slice(&self.pending[4..4 + n]).map_err(err);
                    self.pending.drain(..4 + n);
                    return msg;
                }
            }
            let ct = read_raw(&mut self.stream)?;
            let n = {
                let t = self.t.lock().unwrap_or_else(|e| e.into_inner());
                t.read_message(self.nonce, &ct, &mut self.buf).map_err(err)?
            };
            self.nonce += 1;
            self.pending.extend_from_slice(&self.buf[..n]);
        }
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }
}

fn finish(s: TcpStream, hs: HandshakeState) -> io::Result<Handshaked> {
    let remote_key = hex(hs.get_remote_static().ok_or_else(|| err("peer sent no key"))?);
    let h = hs.get_handshake_hash();
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    let code = format!("{:03} {:03}", n / 1000, n % 1000);
    let t = Arc::new(Mutex::new(hs.into_stateless_transport_mode().map_err(err)?));
    let s2 = s.try_clone()?;
    Ok(Handshaked {
        tx: FrameSender { stream: s, t: t.clone(), nonce: 0, buf: vec![0; MAX_NOISE_MSG] },
        rx: FrameReceiver { stream: s2, t, nonce: 0, pending: Vec::new(), buf: vec![0; MAX_NOISE_MSG] },
        remote_key,
        code,
    })
}

pub fn connect_handshake(mut s: TcpStream, private_key: &[u8]) -> io::Result<Handshaked> {
    s.set_nodelay(true)?;
    let mut hs = Builder::new(PARAMS.parse().unwrap())
        .local_private_key(private_key)
        .build_initiator()
        .map_err(err)?;
    let mut buf = vec![0u8; MAX_NOISE_MSG];
    let n = hs.write_message(&[], &mut buf).map_err(err)?;
    write_raw(&mut s, &buf[..n])?;
    let m = read_raw(&mut s)?;
    hs.read_message(&m, &mut buf).map_err(err)?;
    let n = hs.write_message(&[], &mut buf).map_err(err)?;
    write_raw(&mut s, &buf[..n])?;
    finish(s, hs)
}

pub fn accept_handshake(mut s: TcpStream, private_key: &[u8]) -> io::Result<Handshaked> {
    s.set_nodelay(true)?;
    let mut hs = Builder::new(PARAMS.parse().unwrap())
        .local_private_key(private_key)
        .build_responder()
        .map_err(err)?;
    let mut buf = vec![0u8; MAX_NOISE_MSG];
    let m = read_raw(&mut s)?;
    hs.read_message(&m, &mut buf).map_err(err)?;
    let n = hs.write_message(&[], &mut buf).map_err(err)?;
    write_raw(&mut s, &buf[..n])?;
    let m = read_raw(&mut s)?;
    hs.read_message(&m, &mut buf).map_err(err)?;
    finish(s, hs)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Announce {
    pub name: String,
    pub port: u16,
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct Discovered {
    pub addr: SocketAddr,
    pub info: Announce,
}

/// Server side: answer discovery broadcasts until `stop` is set.
pub fn run_discovery_responder(stop: Arc<AtomicBool>, info: Announce) {
    let mut warned = false;
    let sock = loop {
        match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
            Ok(s) => break s,
            Err(e) => {
                if !warned {
                    log::warn!("discovery paused: can't use UDP port {DISCOVERY_PORT} ({e}); retrying");
                    warned = true;
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
    let reply = serde_json::to_vec(&info).unwrap_or_default();
    let mut buf = [0u8; 256];
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, src)) = sock.recv_from(&mut buf) {
            if &buf[..n] == DISCOVER_MAGIC {
                let _ = sock.send_to(&reply, src);
            }
        }
    }
}

/// Client side: broadcast and collect replies for `wait`.
pub fn discover(wait: Duration) -> Vec<Discovered> {
    let mut found: Vec<Discovered> = Vec::new();
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else { return found };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = sock.send_to(DISCOVER_MAGIC, ("255.255.255.255", DISCOVERY_PORT));
    let start = Instant::now();
    let mut buf = [0u8; 2048];
    let mut resent = false;
    while start.elapsed() < wait {
        if !resent && start.elapsed() > wait / 2 {
            let _ = sock.send_to(DISCOVER_MAGIC, ("255.255.255.255", DISCOVERY_PORT));
            resent = true;
        }
        if let Ok((n, src)) = sock.recv_from(&mut buf) {
            if let Ok(info) = serde_json::from_slice::<Announce>(&buf[..n]) {
                let addr = SocketAddr::new(src.ip(), info.port);
                if !found.iter().any(|d| d.info.key == info.key) {
                    found.push(Discovered { addr, info });
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{ToClient, ToServer};
    use std::net::TcpListener;

    #[test]
    fn handshake_pairing_code_and_large_messages() {
        let (s_priv, s_pub) = generate_keypair();
        let (c_priv, c_pub) = generate_keypair();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let big = "x".repeat(200_000);
        let big2 = big.clone();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut h = accept_handshake(s, &s_priv).unwrap();
            let hello: ToServer = h.rx.recv().unwrap();
            assert!(matches!(hello, ToServer::Hello { ref name, .. } if name == "surface"));
            h.tx.send(&ToClient::Key { code: 30, down: true }).unwrap();
            h.tx.send(&ToClient::Clipboard { text: big2 }).unwrap();
            let back: ToServer = h.rx.recv().unwrap();
            (h.remote_key, h.code, back)
        });
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut h = connect_handshake(s, &c_priv).unwrap();
        h.tx.send(&ToServer::Hello { name: "surface".into(), width: 1, height: 1, os: "linux".into(), version: 1 }).unwrap();
        assert!(matches!(h.rx.recv::<ToClient>().unwrap(), ToClient::Key { code: 30, down: true }));
        match h.rx.recv::<ToClient>().unwrap() {
            ToClient::Clipboard { text } => assert_eq!(text, big),
            m => panic!("{m:?}"),
        }
        h.tx.send(&ToServer::Pong).unwrap();
        let (seen_client_key, server_code, back) = server.join().unwrap();
        assert_eq!(seen_client_key, hex(&c_pub), "server sees the client's identity");
        assert_eq!(h.remote_key, hex(&s_pub), "client sees the server's identity");
        assert_eq!(server_code, h.code, "both sides show the same pairing code");
        assert!(matches!(back, ToServer::Pong));
    }

    #[test]
    fn hex_round_trip() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(unhex(&hex(&b)).unwrap(), b);
    }
}
