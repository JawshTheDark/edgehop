//! Fast file transfer: several parallel encrypted connections, each pulling
//! 8 MB pieces from a shared queue and writing them straight into place.
//!
//! The sender publishes a `Manifest` (file list) and keeps the real paths in
//! its `Outbox`. The receiver opens N data connections and requests pieces:
//!
//!   request  (16 bytes): [u32 file index][u64 offset][u32 length]   (length 0 = done)
//!   response (length bytes of file data)
//!
//! Everything travels inside the same Noise encryption as the control channel.
use crate::files;
use crate::net::{FrameReceiver, FrameSender};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Parallel connections per transfer (configurable).
pub const DEFAULT_CONNS: u8 = 4;
/// Size of one request.
const PIECE: u64 = 8 * 1024 * 1024;
/// Responses are sent in slices of this size (bounds memory per write).
const SLICE: usize = 1024 * 1024;

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    /// '/'-separated, starting with the top-level item's name.
    pub rel: String,
    pub size: u64,
    pub dir: bool,
    #[serde(default)]
    pub mtime: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: u64,
    /// Name of the computer the files are on.
    pub from: String,
    pub total: u64,
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// Same files (names, sizes, modification times) from the same computer.
    pub fn signature(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.from.hash(&mut h);
        self.entries.hash(&mut h);
        h.finish()
    }

    pub fn top_level(&self) -> Vec<&str> {
        let mut tops: Vec<&str> = Vec::new();
        for e in &self.entries {
            let t = e.rel.split('/').next().unwrap_or("");
            if !tops.contains(&t) {
                tops.push(t);
            }
        }
        tops
    }

    pub fn describe(&self) -> String {
        let what = match self.top_level().as_slice() {
            [one] => format!("“{one}”"),
            many => format!("{} items", many.len()),
        };
        format!("{what} ({})", files::human(self.total))
    }
}

pub fn new_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    t ^ (N.fetch_add(1, Ordering::Relaxed) << 48) ^ (std::process::id() as u64) << 32
}

/// Files this computer has offered, by manifest id.
pub struct Offered {
    pub manifest: Manifest,
    paths: Vec<PathBuf>,
}

#[derive(Default)]
pub struct Outbox {
    offers: Mutex<Vec<Arc<Offered>>>,
}

impl Outbox {
    /// Offer files and folders; reuses the previous offer for unchanged files.
    pub fn offer(&self, from: &str, roots: &[PathBuf]) -> io::Result<Manifest> {
        let mut found = Vec::new();
        for r in roots {
            let name = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
            files::collect(r, name, &mut found)?;
        }
        let mut entries = Vec::with_capacity(found.len());
        let mut paths = Vec::with_capacity(found.len());
        for (abs, rel, dir, size) in found {
            let mtime = fs::metadata(&abs)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            entries.push(Entry { rel, size, dir, mtime });
            paths.push(abs);
        }
        let total = entries.iter().map(|e| e.size).sum();
        let mut manifest = Manifest { id: 0, from: from.to_string(), total, entries };
        let sig = manifest.signature();
        let mut offers = lk(&self.offers);
        if let Some(o) = offers.iter().find(|o| o.manifest.signature() == sig) {
            return Ok(o.manifest.clone());
        }
        manifest.id = new_id();
        offers.push(Arc::new(Offered { manifest: manifest.clone(), paths }));
        while offers.len() > 16 {
            offers.remove(0);
        }
        Ok(manifest)
    }

    pub fn get(&self, id: u64) -> Option<Arc<Offered>> {
        lk(&self.offers).iter().find(|o| o.manifest.id == id).cloned()
    }
}

/// Process-wide outbox (both roles use it).
pub fn outbox() -> &'static Outbox {
    static O: OnceLock<Outbox> = OnceLock::new();
    O.get_or_init(Outbox::default)
}

/// Answer piece requests on one data connection until the receiver is done.
pub fn serve(tx: &mut FrameSender, rx: &mut FrameReceiver, offered: &Offered) -> io::Result<u64> {
    let mut open: HashMap<u32, File> = HashMap::new();
    let mut req = Vec::with_capacity(16);
    let mut buf = Vec::new();
    let mut sent = 0u64;
    loop {
        rx.recv_data(&mut req, 16)?;
        let fi = u32::from_be_bytes(req[0..4].try_into().unwrap());
        let off = u64::from_be_bytes(req[4..12].try_into().unwrap());
        let len = u32::from_be_bytes(req[12..16].try_into().unwrap()) as usize;
        if len == 0 {
            return Ok(sent);
        }
        let entry = offered.manifest.entries.get(fi as usize).ok_or_else(|| io::Error::other("bad file index"))?;
        if entry.dir || off.saturating_add(len as u64) > entry.size {
            return Err(io::Error::other("bad piece request"));
        }
        if !open.contains_key(&fi) {
            open.insert(fi, File::open(&offered.paths[fi as usize])?);
        }
        let f = open.get_mut(&fi).unwrap();
        f.seek(SeekFrom::Start(off))?;
        buf.resize(len, 0);
        f.read_exact(&mut buf)?;
        for part in buf.chunks(SLICE) {
            tx.send_data(part)?;
        }
        sent += len as u64;
    }
}

/// Download everything in `m` into `dest` over the given connections.
/// Top-level items get unique names (never overwrite). Returns their paths.
pub fn fetch(
    m: &Manifest,
    conns: Vec<(FrameSender, FrameReceiver)>,
    dest: &Path,
    done: &AtomicU64,
) -> io::Result<Vec<PathBuf>> {
    if conns.is_empty() {
        return Err(io::Error::other("no transfer connections"));
    }
    fs::create_dir_all(dest)?;
    // Lay out every directory and pre-size every file.
    let mut tops: Vec<PathBuf> = Vec::new();
    let mut renamed: HashMap<String, PathBuf> = HashMap::new();
    let mut targets: Vec<PathBuf> = Vec::with_capacity(m.entries.len());
    for e in &m.entries {
        let rel = files::safe_rel(&e.rel).ok_or_else(|| io::Error::other(format!("unsafe path {:?}", e.rel)))?;
        let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        let top = renamed
            .entry(first.clone())
            .or_insert_with(|| {
                let p = files::unique_path(&dest.join(&first));
                tops.push(p.clone());
                p
            })
            .clone();
        let path = match rel.strip_prefix(&first) {
            Ok(rest) if !rest.as_os_str().is_empty() => top.join(rest),
            _ => top,
        };
        if e.dir {
            fs::create_dir_all(&path)?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            File::create(&path)?.set_len(e.size)?;
        }
        targets.push(path);
    }

    let mut queue = VecDeque::new();
    for (i, e) in m.entries.iter().enumerate() {
        let mut off = 0;
        while !e.dir && off < e.size {
            let len = PIECE.min(e.size - off);
            queue.push_back((i as u32, off, len as u32));
            off += len;
        }
    }
    let queue = Mutex::new(queue);
    let last_err: Mutex<Option<io::Error>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for (mut tx, mut rx) in conns {
            let (queue, targets, last_err) = (&queue, &targets, &last_err);
            scope.spawn(move || {
                let mut open: HashMap<u32, File> = HashMap::new();
                let mut buf = Vec::new();
                loop {
                    let Some(job) = lk(queue).pop_front() else {
                        let _ = tx.send_data(&[0u8; 16]);
                        return;
                    };
                    let (fi, off, len) = job;
                    let r = (|| -> io::Result<()> {
                        let mut req = [0u8; 16];
                        req[0..4].copy_from_slice(&fi.to_be_bytes());
                        req[4..12].copy_from_slice(&off.to_be_bytes());
                        req[12..16].copy_from_slice(&len.to_be_bytes());
                        tx.send_data(&req)?;
                        rx.recv_data(&mut buf, len as usize)?;
                        if !open.contains_key(&fi) {
                            open.insert(fi, OpenOptions::new().write(true).open(&targets[fi as usize])?);
                        }
                        let f = open.get_mut(&fi).unwrap();
                        f.seek(SeekFrom::Start(off))?;
                        f.write_all(&buf)?;
                        Ok(())
                    })();
                    match r {
                        Ok(()) => {
                            done.fetch_add(len as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            // Hand the piece to the remaining connections and drop out.
                            lk(queue).push_front(job);
                            *lk(last_err) = Some(e);
                            return;
                        }
                    }
                }
            });
        }
    });

    if !lk(&queue).is_empty() {
        let why = lk(&last_err).take().map(|e| e.to_string()).unwrap_or_default();
        return Err(io::Error::other(format!("transfer incomplete: {why}")));
    }
    Ok(tops)
}

/// Files already received this session (signature -> top-level paths), so
/// crossing back and forth doesn't transfer the same files again.
pub fn cached(m: &Manifest) -> Option<Vec<PathBuf>> {
    let map = lk(cache());
    map.get(&m.signature()).filter(|ps| ps.iter().all(|p| p.exists())).cloned()
}

pub fn remember(m: &Manifest, paths: &[PathBuf]) {
    lk(cache()).insert(m.signature(), paths.to_vec());
}

fn cache() -> &'static Mutex<HashMap<u64, Vec<PathBuf>>> {
    static C: OnceLock<Mutex<HashMap<u64, Vec<PathBuf>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// A fresh folder for received files.
pub fn new_drop_dir() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    files::received_root().join(format!("{stamp}-{}", N.fetch_add(1, Ordering::Relaxed)))
}

fn received() -> &'static Mutex<Vec<PathBuf>> {
    static R: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// We just put these received files on the clipboard.
pub fn mark_received(paths: &[PathBuf]) {
    *lk(received()) = paths.to_vec();
}

/// Is the clipboard still holding files we received (so not something new
/// to send back where they came from)?
pub fn is_received(paths: &[PathBuf]) -> bool {
    let r = lk(received());
    !r.is_empty() && r.as_slice() == paths
}

fn in_flight() -> &'static Mutex<Vec<u64>> {
    static F: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// Claim a fetch of these files; false if it's already being fetched.
pub fn begin_fetch(m: &Manifest) -> bool {
    let mut f = lk(in_flight());
    let sig = m.signature();
    if f.contains(&sig) {
        return false;
    }
    f.push(sig);
    true
}

pub fn end_fetch(m: &Manifest) {
    let sig = m.signature();
    lk(in_flight()).retain(|s| *s != sig);
}

// ---------------------------------------------------------------------------
// Benchmark: `edgehop --bench-serve <path>` on one computer and
// `edgehop --bench-fetch <host> [connections]` on another.
// ---------------------------------------------------------------------------

const BENCH_PORT: u16 = 24890;

pub fn bench_serve(path: &Path) -> io::Result<()> {
    let (private, _) = crate::net::generate_keypair();
    let manifest = outbox().offer("bench", &[path.to_path_buf()])?;
    println!("serving {} on port {BENCH_PORT}; Ctrl+C to stop", manifest.describe());
    let listener = std::net::TcpListener::bind(("0.0.0.0", BENCH_PORT))?;
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let private = private.clone();
        let manifest = manifest.clone();
        std::thread::spawn(move || -> io::Result<()> {
            let mut h = crate::net::accept_handshake(stream, &private)?;
            let hello: serde_json::Value = h.rx.recv()?;
            if hello["t"] == "manifest" {
                h.tx.send(&manifest)?;
            } else if let Some(o) = outbox().get(manifest.id) {
                serve(&mut h.tx, &mut h.rx, &o)?;
            }
            Ok(())
        });
    }
    Ok(())
}

pub fn bench_fetch(host: &str, conns: usize) -> io::Result<()> {
    let (private, _) = crate::net::generate_keypair();
    let addr = format!("{host}:{BENCH_PORT}");
    let connect = |kind: &str| -> io::Result<(FrameSender, FrameReceiver)> {
        let s = std::net::TcpStream::connect(&addr)?;
        let mut h = crate::net::connect_handshake(s, &private)?;
        h.tx.send(&serde_json::json!({ "t": kind }))?;
        Ok((h.tx, h.rx))
    };
    let (_tx, mut rx) = connect("manifest")?;
    let m: Manifest = rx.recv()?;
    let dest = std::env::temp_dir().join(format!("edgehop-bench-{}", std::process::id()));
    let mut data = Vec::new();
    for _ in 0..conns {
        data.push(connect("data")?);
    }
    let done = AtomicU64::new(0);
    let start = std::time::Instant::now();
    let r = fetch(&m, data, &dest, &done);
    let secs = start.elapsed().as_secs_f64();
    let _ = fs::remove_dir_all(&dest);
    r?;
    let bytes = done.load(Ordering::Relaxed) as f64;
    println!(
        "{conns} connection(s): {} in {:.2} s = {:.1} MB/s ({:.0} Mbit/s)",
        files::human(bytes as u64),
        secs,
        bytes / secs / 1e6,
        bytes * 8.0 / secs / 1e6
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// N real encrypted loopback connections: (receiver side, sender side).
    fn pairs(n: usize) -> Vec<((FrameSender, FrameReceiver), (FrameSender, FrameReceiver))> {
        let (sk, _) = crate::net::generate_keypair();
        let (ck, _) = crate::net::generate_keypair();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        (0..n)
            .map(|_| {
                let sk = sk.clone();
                let accept = l.try_clone().unwrap();
                let srv = std::thread::spawn(move || {
                    let (s, _) = accept.accept().unwrap();
                    let h = crate::net::accept_handshake(s, &sk).unwrap();
                    (h.tx, h.rx)
                });
                let c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                let h = crate::net::connect_handshake(c, &ck).unwrap();
                ((h.tx, h.rx), srv.join().unwrap())
            })
            .collect()
    }

    fn tree(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("edgehop-xfer-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/movie/extras")).unwrap();
        // 20 MB + odd tail: several pieces, last one partial.
        let big: Vec<u8> = (0..(20 * 1024 * 1024 + 12345u32)).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        fs::write(base.join("src/movie/film.mkv"), &big).unwrap();
        fs::write(base.join("src/movie/extras/notes.txt"), b"hello").unwrap();
        fs::write(base.join("src/movie/empty.bin"), b"").unwrap();
        fs::write(base.join("src/photo.jpg"), vec![7u8; 3000]).unwrap();
        base
    }

    #[test]
    fn parallel_transfer_reassembles_everything() {
        let base = tree("par");
        let outbox = Outbox::default();
        let m = outbox.offer("pc", &[base.join("src/movie"), base.join("src/photo.jpg")]).unwrap();
        assert_eq!(m.top_level(), vec!["movie", "photo.jpg"]);
        let offered = outbox.get(m.id).unwrap();

        let ps = pairs(4);
        let mut receivers = Vec::new();
        let mut servers = Vec::new();
        for (recv_side, (mut stx, mut srx)) in ps {
            receivers.push(recv_side);
            let o = offered.clone();
            servers.push(std::thread::spawn(move || serve(&mut stx, &mut srx, &o).unwrap()));
        }
        let dest = base.join("dest");
        let done = AtomicU64::new(0);
        let tops = fetch(&m, receivers, &dest, &done).unwrap();
        let served: u64 = servers.into_iter().map(|t| t.join().unwrap()).sum();
        assert_eq!(done.load(Ordering::Relaxed), m.total);
        assert_eq!(served, m.total);
        assert!(served > 0);
        assert_eq!(tops, vec![dest.join("movie"), dest.join("photo.jpg")]);
        assert_eq!(fs::read(dest.join("movie/film.mkv")).unwrap(), fs::read(base.join("src/movie/film.mkv")).unwrap());
        assert_eq!(fs::read(dest.join("movie/extras/notes.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dest.join("movie/empty.bin")).unwrap(), b"");
        assert_eq!(fs::read(dest.join("photo.jpg")).unwrap(), vec![7u8; 3000]);

        // Same files again into the same folder: renamed, never overwritten.
        let ps = pairs(1);
        let ((rtx, rrx), (mut stx, mut srx)) = ps.into_iter().next().unwrap();
        let o = offered.clone();
        let t = std::thread::spawn(move || serve(&mut stx, &mut srx, &o).unwrap());
        let tops2 = fetch(&m, vec![(rtx, rrx)], &dest, &AtomicU64::new(0)).unwrap();
        t.join().unwrap();
        assert_eq!(tops2, vec![dest.join("movie (2)"), dest.join("photo (2).jpg")]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn offers_are_reused_for_unchanged_files() {
        let base = tree("reuse");
        let outbox = Outbox::default();
        let a = outbox.offer("pc", &[base.join("src/photo.jpg")]).unwrap();
        let b = outbox.offer("pc", &[base.join("src/photo.jpg")]).unwrap();
        assert_eq!(a.id, b.id, "same files: same offer");
        fs::write(base.join("src/photo.jpg"), vec![1u8; 10]).unwrap();
        let c = outbox.offer("pc", &[base.join("src/photo.jpg")]).unwrap();
        assert_ne!(a.id, c.id, "changed file: new offer");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failing_connection_hands_its_pieces_to_the_others() {
        let base = tree("fail");
        let outbox = Outbox::default();
        let m = outbox.offer("pc", &[base.join("src/movie")]).unwrap();
        let offered = outbox.get(m.id).unwrap();
        let mut ps = pairs(3);
        // One sender side dies immediately.
        let (dead_recv, dead_send) = ps.pop().unwrap();
        drop(dead_send);
        let mut receivers = vec![dead_recv];
        let mut servers = Vec::new();
        for (recv_side, (mut stx, mut srx)) in ps {
            receivers.push(recv_side);
            let o = offered.clone();
            servers.push(std::thread::spawn(move || serve(&mut stx, &mut srx, &o).unwrap()));
        }
        let dest = base.join("dest");
        let done = AtomicU64::new(0);
        fetch(&m, receivers, &dest, &done).unwrap();
        for s in servers {
            s.join().unwrap();
        }
        assert_eq!(fs::read(dest.join("movie/film.mkv")).unwrap(), fs::read(base.join("src/movie/film.mkv")).unwrap());
        let _ = fs::remove_dir_all(&base);
    }
}
