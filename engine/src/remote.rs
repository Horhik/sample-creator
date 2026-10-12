//! Sharing a folder with another Sample Creator over iroh: an encrypted QUIC
//! connection dialled by public key, direct where possible (same network,
//! hole punching) and through iroh's public relays otherwise. No server of
//! our own.
//!
//! The sharing side runs a [`Share`]; its ticket (shown as a QR code) carries
//! the endpoint address and a random access token. The other side
//! [`connect`]s with the ticket and reads the folder read-only: list a
//! folder, look at a file, read a piece of a file. Each request goes on its
//! own stream: a JSON line, then for reads the bytes.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use serde::{Deserialize, Serialize};

use crate::lang::tr;
use crate::store::Library;
use crate::vfs::{Meta, REMOTE_ROOT, VFile};

const ALPN: &[u8] = b"sample-creator/share/1";
const TICKET_PREFIX: &str = "sc1";
/// Bytes fetched per request when reading a remote file.
const BLOCK: u64 = 512 * 1024;
/// Blocks kept per open remote file.
const BLOCKS_KEPT: usize = 32;
/// Blocks fetched after a missing one, in parallel with it: streaming then
/// waits one round trip per two megabytes, not per block.
const READ_AHEAD: u64 = 3;
const MAX_READ: u32 = 4 * 1024 * 1024;

/// One runtime for all iroh work; the engine itself is synchronous.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("iroh")
            .build()
            .expect("tokio runtime")
    })
}

// ---- Protocol ----

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    op: Op,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Op {
    List { path: String },
    Stat { path: String },
    Read { path: String, offset: u64, len: u32 },
}

#[derive(Serialize, Deserialize, Default)]
struct Reply {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// list: (name, is_dir)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<(String, bool)>,
    /// stat: size, modification time (s since the epoch), folder?, name
    #[serde(default)]
    len: u64,
    #[serde(default)]
    mtime: Option<u64>,
    #[serde(default)]
    dir: bool,
    #[serde(default)]
    name: String,
    /// read: bytes that follow the line
    #[serde(default)]
    n: u32,
}

/// A shared folder's ticket: access token plus endpoint address.
pub fn make_ticket(token: &str, addr: iroh::EndpointAddr) -> String {
    format!("{TICKET_PREFIX}.{token}.{}", EndpointTicket::new(addr))
}

fn parse_ticket(ticket: &str) -> Result<(String, EndpointTicket)> {
    let bad = || anyhow!(tr("это не код Sample Creator", "not a Sample Creator code"));
    let ticket = ticket.trim();
    let mut parts = ticket.splitn(3, '.');
    if parts.next() != Some(TICKET_PREFIX) {
        return Err(bad());
    }
    let token = parts.next().ok_or_else(bad)?.to_owned();
    let endpoint = EndpointTicket::from_str(parts.next().ok_or_else(bad)?).map_err(|_| bad())?;
    Ok((token, endpoint))
}

/// Random hex, from the key generator iroh already uses.
pub fn random_token() -> String {
    SecretKey::generate().to_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// This computer's iroh key (so the address stays the same) and the folder's
/// access token, made once and kept: a QR code scanned before keeps working.
pub fn share_keys(library: &Library, folder: &Path) -> ([u8; 32], String) {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let secret = library
        .setting("share.secret")
        .and_then(|h| {
            let bytes: Vec<u8> = (0..h.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
                .collect();
            bytes.try_into().ok()
        })
        .unwrap_or_else(|| {
            let key = iroh_key();
            library.set_setting("share.secret", &hex(&key)).ok();
            key
        });
    let key = format!("share.token:{}", folder.display());
    let token = library.setting(&key).unwrap_or_else(|| {
        let t = random_token();
        library.set_setting(&key, &t).ok();
        t
    });
    (secret, token)
}

fn iroh_key() -> [u8; 32] {
    let t = random_token() + &random_token();
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    key
}

// ---- Sharing side ----

/// A folder open to whoever has the ticket, while this value lives.
pub struct Share {
    endpoint: Endpoint,
    pub folder: PathBuf,
    ticket: String,
}

impl Share {
    /// Starts sharing `folder`. `secret` keeps the address the same across
    /// restarts and `token` is what a client must show; together they make a
    /// ticket that stays valid.
    pub fn start(folder: PathBuf, secret: [u8; 32], token: String) -> Result<Share> {
        let root = folder
            .canonicalize()
            .with_context(|| format!("{}", folder.display()))?;
        runtime().block_on(async move {
            let endpoint = Endpoint::builder(presets::N0)
                .secret_key(SecretKey::from_bytes(&secret))
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
                .map_err(|e| anyhow!("{e}"))?;
            // Wait a little for a relay, so the ticket works from anywhere.
            let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
            let ticket = make_ticket(&token, endpoint.addr());
            let server = endpoint.clone();
            let (root2, token2) = (root.clone(), token.clone());
            tokio::spawn(async move {
                while let Some(incoming) = server.accept().await {
                    let (root, token) = (root2.clone(), token2.clone());
                    tokio::spawn(async move {
                        if let Ok(conn) = incoming.await {
                            serve(conn, root, token).await;
                        }
                    });
                }
            });
            Ok(Share {
                endpoint,
                folder: root,
                ticket,
            })
        })
    }

    pub fn ticket(&self) -> &str {
        &self.ticket
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        let endpoint = self.endpoint.clone();
        runtime().spawn(async move { endpoint.close().await });
    }
}

async fn serve(conn: Connection, root: PathBuf, token: String) {
    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
        let (root, token) = (root.clone(), token.clone());
        tokio::spawn(async move {
            let Ok(raw) = recv.read_to_end(64 * 1024).await else {
                return;
            };
            let (reply, bytes) = match serde_json::from_slice::<Request>(&raw) {
                Ok(req) if req.token == token => answer(&root, req.op),
                Ok(_) => (fail("bad token"), Vec::new()),
                Err(e) => (fail(&e.to_string()), Vec::new()),
            };
            let mut line = serde_json::to_vec(&reply).unwrap_or_default();
            line.push(b'\n');
            if send.write_all(&line).await.is_ok() && !bytes.is_empty() {
                let _ = send.write_all(&bytes).await;
            }
            let _ = send.finish();
        });
    }
}

fn fail(msg: &str) -> Reply {
    Reply {
        ok: false,
        error: Some(msg.to_owned()),
        ..Reply::default()
    }
}

/// `inner` inside `root`, refusing anything that would step outside it.
fn resolve(root: &Path, inner: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for c in Path::new(inner).components() {
        match c {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => bail!("path outside the shared folder"),
        }
    }
    let real = path.canonicalize()?;
    if !real.starts_with(root) {
        bail!("path outside the shared folder");
    }
    Ok(real)
}

fn answer(root: &Path, op: Op) -> (Reply, Vec<u8>) {
    let result = (|| -> Result<(Reply, Vec<u8>)> {
        match op {
            Op::List { path } => {
                let dir = resolve(root, &path)?;
                let mut entries = Vec::new();
                for item in std::fs::read_dir(dir)? {
                    let item = item?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    entries.push((name, item.path().is_dir()));
                }
                Ok((
                    Reply {
                        ok: true,
                        entries,
                        ..Reply::default()
                    },
                    Vec::new(),
                ))
            }
            Op::Stat { path } => {
                let p = resolve(root, &path)?;
                let m = std::fs::metadata(&p)?;
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                Ok((
                    Reply {
                        ok: true,
                        len: m.len(),
                        mtime,
                        dir: m.is_dir(),
                        name: p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        ..Reply::default()
                    },
                    Vec::new(),
                ))
            }
            Op::Read { path, offset, len } => {
                let p = resolve(root, &path)?;
                let mut f = std::fs::File::open(p)?;
                f.seek(SeekFrom::Start(offset))?;
                let mut buf = Vec::with_capacity(len.min(MAX_READ) as usize);
                f.take(u64::from(len.min(MAX_READ))).read_to_end(&mut buf)?;
                Ok((
                    Reply {
                        ok: true,
                        n: buf.len() as u32,
                        ..Reply::default()
                    },
                    buf,
                ))
            }
        }
    })();
    result.unwrap_or_else(|e| (fail(&format!("{e:#}")), Vec::new()))
}

// ---- Connecting side ----

struct Client {
    token: String,
    ticket: EndpointTicket,
    endpoint: Endpoint,
    conn: Mutex<Option<Connection>>,
    /// The shared folder's own name, for showing.
    name: String,
}

static CLIENTS: Mutex<Option<HashMap<String, Arc<Client>>>> = Mutex::new(None);

fn client(id: &str) -> io::Result<Arc<Client>> {
    CLIENTS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(id).cloned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "remote share not connected"))
}

/// Short stable id of a ticket's share, used in its paths.
fn share_id(ticket: &EndpointTicket, token: &str) -> String {
    let h = xxhash_rust::xxh3::xxh3_64(format!("{}{token}", ticket.endpoint_addr().id).as_bytes());
    format!("{:08x}", h as u32)
}

/// A connected share: where it is mounted and what it is called.
#[derive(Debug, Clone)]
pub struct Mounted {
    pub root: PathBuf,
    pub name: String,
}

/// Connects to a shared folder; it then reads like a local one under the
/// returned root. Connecting again with the same ticket reuses it.
pub fn connect(ticket: &str) -> Result<Mounted> {
    let (token, endpoint_ticket) = parse_ticket(ticket)?;
    let id = share_id(&endpoint_ticket, &token);
    let root = Path::new(REMOTE_ROOT).join(&id);
    if let Ok(c) = client(&id) {
        return Ok(Mounted {
            root,
            name: c.name.clone(),
        });
    }
    let endpoint = runtime()
        .block_on(Endpoint::bind(presets::N0))
        .map_err(|e| anyhow!("{e}"))?;
    let mut c = Client {
        token,
        ticket: endpoint_ticket,
        endpoint,
        conn: Mutex::new(None),
        name: String::new(),
    };
    let top = c.request(&Op::Stat { path: String::new() }).map(|(r, _)| r)?;
    c.name = top.name;
    let name = c.name.clone();
    CLIENTS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(id, Arc::new(c));
    Ok(Mounted { root, name })
}

pub fn disconnect(root: &Path) {
    if let Some((id, _)) = crate::vfs::split_remote(root)
        && let Some(map) = CLIENTS.lock().unwrap().as_mut()
        && let Some(c) = map.remove(id)
    {
        let endpoint = c.endpoint.clone();
        runtime().spawn(async move { endpoint.close().await });
    }
}

impl Client {
    async fn connection(&self) -> Result<Connection> {
        if let Some(c) = self.conn.lock().unwrap().clone()
            && c.close_reason().is_none()
        {
            return Ok(c);
        }
        let c = tokio::time::timeout(
            Duration::from_secs(20),
            self.endpoint
                .connect(self.ticket.endpoint_addr().clone(), ALPN),
        )
        .await
        .map_err(|_| anyhow!(tr("компьютер не отвечает", "the computer does not answer")))?
        .map_err(|e| anyhow!("{e}"))?;
        *self.conn.lock().unwrap() = Some(c.clone());
        Ok(c)
    }

    fn request(&self, op: &Op) -> Result<(Reply, Vec<u8>)> {
        let req = serde_json::to_vec(&Request {
            token: self.token.clone(),
            op: match op {
                Op::List { path } => Op::List { path: path.clone() },
                Op::Stat { path } => Op::Stat { path: path.clone() },
                Op::Read { path, offset, len } => Op::Read {
                    path: path.clone(),
                    offset: *offset,
                    len: *len,
                },
            },
        })?;
        runtime().block_on(self.send(&req))
    }

    /// Sends a serialised request; reconnects once if the connection dropped.
    async fn send(&self, req: &[u8]) -> Result<(Reply, Vec<u8>)> {
        let mut last = None;
        for _ in 0..2 {
            match self.round_trip(req).await {
                Ok(r) => return Ok(r),
                Err(e) => {
                    *self.conn.lock().unwrap() = None;
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("request failed")))
    }

    async fn round_trip(&self, req: &[u8]) -> Result<(Reply, Vec<u8>)> {
        let conn = self.connection().await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(req).await?;
        send.finish()?;
        let raw = recv.read_to_end(MAX_READ as usize + 64 * 1024).await?;
        let nl = raw
            .iter()
            .position(|&b| b == b'\n')
            .context("bad reply")?;
        let reply: Reply = serde_json::from_slice(&raw[..nl])?;
        if !reply.ok {
            bail!("{}", reply.error.unwrap_or_default());
        }
        Ok((reply, raw[nl + 1..].to_vec()))
    }
}

fn io_err(e: anyhow::Error) -> io::Error {
    io::Error::other(format!("{e:#}"))
}

pub fn list(id: &str, inner: &str) -> io::Result<Vec<(String, bool)>> {
    let c = client(id)?;
    let (r, _) = c
        .request(&Op::List {
            path: inner.to_owned(),
        })
        .map_err(io_err)?;
    Ok(r.entries)
}

pub fn stat(id: &str, inner: &str) -> io::Result<Meta> {
    let c = client(id)?;
    let (r, _) = c
        .request(&Op::Stat {
            path: inner.to_owned(),
        })
        .map_err(io_err)?;
    Ok(Meta {
        len: r.len,
        modified: r
            .mtime
            .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s)),
        is_dir: r.dir,
    })
}

/// Opens a remote file: its downloaded copy if there is one for this version
/// (or any copy while the computer is away), else reads it over the network.
pub fn open(id: &str, inner: &str) -> io::Result<VFile> {
    if let Some(copy) = known_copy(id, inner) {
        return Ok(VFile::Local(std::fs::File::open(copy)?));
    }
    let meta = match stat(id, inner) {
        Ok(meta) => meta,
        Err(e) => {
            return match offline_copy(id, inner) {
                Some((copy, _)) => Ok(VFile::Local(std::fs::File::open(copy)?)),
                None => Err(e),
            };
        }
    };
    if meta.is_dir {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, inner.to_owned()));
    }
    if let Some(copy) = matching_copy(id, inner, &meta) {
        return Ok(VFile::Local(std::fs::File::open(copy)?));
    }
    Ok(VFile::Remote(RemoteFile {
        client: client(id)?,
        path: inner.to_owned(),
        len: meta.len,
        pos: 0,
        blocks: Arc::new(Mutex::new(VecDeque::new())),
        part: copies_dir()
            .ok()
            .map(|d| d.join(format!("{}.part", copy_name(id, inner, &meta)))),
    }))
}

/// Size and time of a remote file, or of its copy while the computer is away.
pub fn metadata(id: &str, inner: &str) -> io::Result<Meta> {
    stat(id, inner).or_else(|e| offline_copy(id, inner).map(|(_, meta)| meta).ok_or(e))
}

// ---- Local copies ----
//
// A remote recording is downloaded whole into the app's cache before it is
// opened: decoding, the waveform and playback then read from local storage,
// and the file still opens while the computer is away. Until then its start
// grows in `<copy>.part`: what streaming reads from the beginning (a preview)
// lands there too, and a download goes on from where it ends.

/// Room for copies; the least recently used go first.
const COPIES_MAX: u64 = 4 * 1024 * 1024 * 1024;
/// Bytes per request while downloading, and requests in flight.
const CHUNK: u64 = 2 * 1024 * 1024;
const IN_FLIGHT: usize = 4;

/// Serialises appends to partial copies (a preview and a download may meet).
static PART_LOCK: Mutex<()> = Mutex::new(());

/// Appends `bytes` read at `offset` to a partial copy if they extend it;
/// anything else (a gap, or bytes it has already) is left out.
fn append_part(part: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let _guard = PART_LOCK.lock().unwrap();
    let have = std::fs::metadata(part).map_or(0, |m| m.len());
    let end = offset + bytes.len() as u64;
    if have < offset || have >= end {
        return Ok(());
    }
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(part)?
        .write_all(&bytes[(have - offset) as usize..])
}

/// Copies checked or fetched in this session: "id/inner" → file.
static COPIES: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

fn copies_dir() -> Result<PathBuf> {
    Ok(crate::audio::cache_dir().context("no cache folder")?.join("remote"))
}

/// Copies of one remote file share this prefix; the rest of the name is the
/// file's size and modification time, so a changed file is fetched again.
fn copy_prefix(id: &str, inner: &str) -> String {
    format!("{:016x}_", xxhash_rust::xxh3::xxh3_64(format!("{id}/{inner}").as_bytes()))
}

fn copy_name(id: &str, inner: &str, meta: &Meta) -> String {
    let mtime = meta
        .modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{}{}_{mtime}", copy_prefix(id, inner), meta.len)
}

fn remember(id: &str, inner: &str, copy: &Path) {
    COPIES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(format!("{id}/{inner}"), copy.to_owned());
}

fn known_copy(id: &str, inner: &str) -> Option<PathBuf> {
    COPIES
        .lock()
        .unwrap()
        .as_ref()?
        .get(&format!("{id}/{inner}"))
        .filter(|p| p.is_file())
        .cloned()
}

/// The copy of exactly this version of the file, if it was downloaded.
fn matching_copy(id: &str, inner: &str, meta: &Meta) -> Option<PathBuf> {
    let copy = copies_dir().ok()?.join(copy_name(id, inner, meta));
    copy.is_file().then(|| {
        touch(&copy);
        remember(id, inner, &copy);
        copy
    })
}

/// The newest copy of a file whose computer is away, with the size and time
/// the file had on it.
fn offline_copy(id: &str, inner: &str) -> Option<(PathBuf, Meta)> {
    let prefix = copy_prefix(id, inner);
    std::fs::read_dir(copies_dir().ok()?)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (len, mtime) = name.strip_prefix(&prefix)?.split_once('_')?;
            let meta = Meta {
                len: len.parse().ok()?,
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime.parse().ok()?)),
                is_dir: false,
            };
            Some((e.path(), meta))
        })
        .max_by_key(|(_, meta)| meta.modified)
}

/// Marks a copy as just used, so it is the last to be evicted.
fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Whether a remote file is downloaded and can open without the network.
pub fn has_copy(path: &Path) -> bool {
    let Some((id, inner)) = crate::vfs::split_remote(path) else {
        return false;
    };
    known_copy(id, &inner).is_some()
        || match stat(id, &inner) {
            Ok(meta) => matching_copy(id, &inner, &meta).is_some(),
            Err(_) => offline_copy(id, &inner).is_some(),
        }
}

/// Downloads a remote file whole into the cache, reporting (bytes done,
/// total). Does nothing if this version is there already.
pub fn download(
    path: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let (id, inner) = crate::vfs::split_remote(path).context("not a shared file")?;
    let c = client(id)?;
    let meta = stat(id, &inner)?;
    if meta.is_dir {
        bail!("{} {}", inner, tr("— это папка", "is a folder"));
    }
    if let Some(copy) = matching_copy(id, &inner, &meta) {
        remember(id, &inner, &copy);
        progress(meta.len, meta.len);
        return Ok(());
    }
    let dir = copies_dir()?;
    std::fs::create_dir_all(&dir)?;
    let name = copy_name(id, &inner, &meta);
    let target = dir.join(&name);
    let part = dir.join(format!("{name}.part"));
    // A cancelled or broken download keeps its part and goes on next time.
    fetch(&c, &inner, meta.len, &part, progress, cancel)?;
    if std::fs::metadata(&part).map_or(0, |m| m.len()) != meta.len {
        let _ = std::fs::remove_file(&part);
        bail!("{}", tr("копия получилась неполной", "the copy came out incomplete"));
    }
    std::fs::rename(&part, &target)?;
    prune(&dir, &copy_prefix(id, &inner), &target);
    remember(id, &inner, &target);
    Ok(())
}

/// Fetches the rest of a partial copy: several requests in flight, appended
/// in order.
fn fetch(
    c: &Arc<Client>,
    inner: &str,
    len: u64,
    part: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    use std::sync::atomic::Ordering;

    // Whole blocks only: a part cut short mid-write is trimmed back.
    let start = {
        let _guard = PART_LOCK.lock().unwrap();
        let have = std::fs::metadata(part).map_or(0, |m| m.len());
        let start = if have >= len { len } else { have / BLOCK * BLOCK };
        if have > start {
            std::fs::File::options().write(true).open(part)?.set_len(start)?;
        }
        start
    };
    progress(start, len);
    runtime().block_on(async {
        let mut pending = VecDeque::new();
        let (mut next, mut done) = (start, start);
        while done < len {
            while pending.len() < IN_FLIGHT && next < len {
                let want = (len - next).min(CHUNK);
                let req = serde_json::to_vec(&Request {
                    token: c.token.clone(),
                    op: Op::Read {
                        path: inner.to_owned(),
                        offset: next,
                        len: want as u32,
                    },
                })?;
                let c = c.clone();
                pending.push_back((
                    want,
                    tokio::spawn(async move { c.send(&req).await.map(|(_, bytes)| bytes) }),
                ));
                next += want;
            }
            let (want, task) = pending.pop_front().context("download stalled")?;
            let bytes = task.await??;
            if cancel.load(Ordering::Relaxed) {
                pending.iter().for_each(|(_, t)| t.abort());
                bail!("{}", tr("загрузка отменена", "download cancelled"));
            }
            if bytes.len() as u64 != want {
                pending.iter().for_each(|(_, t)| t.abort());
                let _ = std::fs::remove_file(part);
                bail!("{}", tr("файл на компьютере изменился", "the file changed on the computer"));
            }
            append_part(part, done, &bytes)?;
            done += want;
            progress(done, len);
        }
        Ok::<_, anyhow::Error>(())
    })
}

/// Drops older versions of the file just fetched and, past [`COPIES_MAX`],
/// the least recently used copies.
fn prune(dir: &Path, prefix: &str, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut copies = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path == keep {
            continue;
        }
        if e.file_name().to_string_lossy().starts_with(prefix) {
            let _ = std::fs::remove_file(&path);
        } else if let Ok(m) = e.metadata() {
            copies.push((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), path));
        }
    }
    let mut total: u64 = copies.iter().map(|c| c.1).sum::<u64>()
        + std::fs::metadata(keep).map_or(0, |m| m.len());
    copies.sort();
    for (_, len, path) in copies {
        if total <= COPIES_MAX {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Recently read blocks, newest first: (block index, bytes).
type Blocks = VecDeque<(u64, Arc<Vec<u8>>)>;

/// A remote file read in blocks, the recent ones kept in memory.
pub struct RemoteFile {
    client: Arc<Client>,
    path: String,
    len: u64,
    pos: u64,
    blocks: Arc<Mutex<Blocks>>,
    /// The partial copy: blocks it has are read from it, and blocks read in
    /// order from its end extend it.
    part: Option<PathBuf>,
}

impl RemoteFile {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn cached(&self, index: u64) -> Option<Arc<Vec<u8>>> {
        let mut blocks = self.blocks.lock().unwrap();
        let i = blocks.iter().position(|(k, _)| *k == index)?;
        let b = blocks.remove(i).unwrap();
        let data = b.1.clone();
        blocks.push_front(b);
        Some(data)
    }

    fn keep(&self, index: u64, data: Arc<Vec<u8>>) {
        let mut blocks = self.blocks.lock().unwrap();
        blocks.push_front((index, data));
        blocks.truncate(BLOCKS_KEPT);
    }

    /// A block the partial copy has already.
    fn part_block(&self, index: u64) -> Option<Arc<Vec<u8>>> {
        let part = self.part.as_ref()?;
        let start = index * BLOCK;
        let end = (start + BLOCK).min(self.len);
        let mut f = std::fs::File::open(part).ok()?;
        if f.metadata().ok()?.len() < end {
            return None;
        }
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut data = vec![0; (end - start) as usize];
        f.read_exact(&mut data).ok()?;
        Some(Arc::new(data))
    }

    fn block(&self, index: u64) -> io::Result<Arc<Vec<u8>>> {
        if let Some(data) = self.cached(index) {
            return Ok(data);
        }
        if let Some(data) = self.part_block(index) {
            self.keep(index, data.clone());
            return Ok(data);
        }
        let last = self.len.saturating_sub(1) / BLOCK;
        let wanted: Vec<u64> = (index..=(index + READ_AHEAD).min(last))
            .filter(|&i| i == index || !self.blocks.lock().unwrap().iter().any(|(k, _)| *k == i))
            .collect();
        let fetched = runtime()
            .block_on(async {
                let tasks: Vec<_> = wanted
                    .iter()
                    .map(|&i| {
                        let req = serde_json::to_vec(&Request {
                            token: self.client.token.clone(),
                            op: Op::Read {
                                path: self.path.clone(),
                                offset: i * BLOCK,
                                len: BLOCK as u32,
                            },
                        });
                        let c = self.client.clone();
                        tokio::spawn(async move { c.send(&req?).await.map(|(_, b)| (i, b)) })
                    })
                    .collect();
                let mut out = Vec::new();
                for t in tasks {
                    out.push(t.await??);
                }
                Ok::<_, anyhow::Error>(out)
            })
            .map_err(io_err)?;
        let mut found = None;
        for (i, bytes) in fetched {
            if let Some(part) = &self.part {
                let _ = append_part(part, i * BLOCK, &bytes);
            }
            let data = Arc::new(bytes);
            if i == index {
                found = Some(data.clone());
            }
            self.keep(i, data);
        }
        found.ok_or_else(|| io::Error::other("block missing"))
    }
}

impl Read for RemoteFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let block = self.block(self.pos / BLOCK)?;
        let at = (self.pos % BLOCK) as usize;
        if at >= block.len() {
            return Ok(0);
        }
        let n = buf.len().min(block.len() - at);
        buf[..n].copy_from_slice(&block[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RemoteFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_round_trip_and_reject_junk() {
        assert!(parse_ticket("hello").is_err());
        assert!(parse_ticket("sc1.abc").is_err());
        let addr = iroh::EndpointAddr::new(SecretKey::generate().public());
        let t = make_ticket("feed", addr);
        let (token, _) = parse_ticket(&t).unwrap();
        assert_eq!(token, "feed");
    }

    #[test]
    fn paths_cannot_leave_the_shared_folder() {
        let dir = std::env::temp_dir().join(format!("sc-share-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::fs::write(dir.join("in/a.wav"), b"x").unwrap();
        let root = dir.join("in").canonicalize().unwrap();
        assert!(resolve(&root, "a.wav").is_ok());
        assert!(resolve(&root, "../in/a.wav").is_err());
        assert!(resolve(&root, "/etc/passwd").is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod loopback {
    use super::*;
    use crate::audio::{bwf::tests::recorder_like, decode};

    /// Shares a folder and reads a recording from it through the ticket, the
    /// way the phone does. Needs UDP on localhost.
    #[test]
    fn reads_a_recording_through_a_share() {
        let dir = std::env::temp_dir().join(format!("sc-loop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("MUSIC")).unwrap();
        crate::audio::set_cache_dir(dir.join("cache"));
        // Over 2 MB: downloaded in more than one request.
        let frames: Vec<[f32; 2]> = (0..300_000).map(|i| [i as f32 / 65_536.0, 0.0]).collect();
        std::fs::write(dir.join("MUSIC/take.wav"), recorder_like(&frames)).unwrap();

        let share = Share::start(dir.clone(), SecretKey::generate().to_bytes(), random_token()).unwrap();
        let mounted = connect(share.ticket()).unwrap();
        assert_eq!(mounted.name, dir.file_name().unwrap().to_string_lossy());

        let entries = crate::vfs::read_dir(&mounted.root.join("MUSIC")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "take.wav");

        let path = mounted.root.join("MUSIC/take.wav");
        let (info, mut src) = decode::open(&path).unwrap();
        assert_eq!(info.total_frames, 300_000);
        src.seek(40_000).unwrap();
        let mut out = Vec::new();
        src.read(&mut out, 10).unwrap();
        assert!((out[0] - 40_000.0 / 65_536.0).abs() < 1e-6);

        // Streaming from the start began the partial copy.
        assert!(!has_copy(&path));
        let meta = crate::vfs::metadata(&path).unwrap();
        let len = meta.len;
        let (id, inner) = crate::vfs::split_remote(&path).unwrap();
        let part = copies_dir()
            .unwrap()
            .join(format!("{}.part", copy_name(id, &inner, &meta)));
        let streamed = std::fs::metadata(&part).unwrap().len();
        assert_eq!(streamed, (READ_AHEAD + 1) * BLOCK);

        // A cancelled download keeps it; the next one goes on from its end.
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let mut first = None;
        assert!(download(&path, &mut |done, _| { first.get_or_insert(done); }, &cancel).is_err());
        assert_eq!(first, Some(streamed));
        assert_eq!(std::fs::metadata(&part).unwrap().len(), streamed);

        // Downloaded whole, it then opens from the cache, even offline.
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        let (mut seen, mut calls) = (0, 0);
        download(&path, &mut |done, _| (seen, calls) = (done, calls + 1), &cancel).unwrap();
        assert_eq!(seen, len);
        assert!(calls >= 2);
        assert!(!part.exists());
        assert!(has_copy(&path));
        assert!(matches!(crate::vfs::open(&path).unwrap(), VFile::Local(_)));
        let mut src = decode::open(&path).unwrap().1;
        src.seek(250_000).unwrap();
        let mut out = Vec::new();
        src.read(&mut out, 10).unwrap();
        assert!((out[0] - 250_000.0 / 65_536.0).abs() < 1e-6);

        // A wrong token is refused.
        let bad = share.ticket().replacen('.', ".00", 1);
        assert!(connect(&bad).is_err());
        disconnect(&mounted.root);
        COPIES.lock().unwrap().take();
        assert_eq!(crate::vfs::metadata(&path).unwrap().len, len);
        assert_eq!(decode::open(&path).unwrap().0.total_frames, 300_000);
        std::fs::remove_dir_all(dir).ok();
    }
}
