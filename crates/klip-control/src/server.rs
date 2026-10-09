//! Loopback TCP server and client for the JSON-lines control channel.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Auth, CommandError, Discovery, ErrorBody, Handler, MAX_LINE, Request, Response};

/// Most simultaneous client connections; more are refused.
const MAX_CONNECTIONS: usize = 8;
/// Malformed requests tolerated on one connection before it's closed.
const MAX_BAD_REQUESTS: u32 = 8;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Server start-up errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("no secure random source: {0}")]
    Random(String),
}

/// Client errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Klip recorder is not running")]
    NotRunning,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{} ({})", .0.message, .0.code)]
    Command(ErrorBody),
}

fn random_token() -> Result<String, ServerError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| ServerError::Random(e.to_string()))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read one `\n`-terminated line of at most `max` bytes. `Ok(None)` at clean EOF.
pub fn read_line_capped<R: BufRead>(r: &mut R, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    loop {
        let available = r.fill_buf()?;
        if available.is_empty() {
            return Ok(if buf.is_empty() { None } else { Some(buf) });
        }
        let (take, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i.saturating_add(1), true),
            None => (available.len(), false),
        };
        if buf.len().saturating_add(take) > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        buf.extend_from_slice(available.get(..take).unwrap_or(&[]));
        r.consume(take);
        if done {
            return Ok(Some(buf));
        }
    }
}

fn write_json(w: &mut impl Write, v: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(v).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

fn error_response(id: u64, e: &CommandError) -> Response {
    Response {
        id,
        ok: false,
        result: None,
        error: Some(ErrorBody {
            code: e.code.to_string(),
            message: e.message.clone(),
        }),
    }
}

/// A running control-channel server.
pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
    token: String,
    discovery: Option<PathBuf>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

fn write_discovery(path: &Path, d: &Discovery) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let body = serde_json::to_vec(d).map_err(io::Error::other)?;
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

impl Server {
    /// Listen on `127.0.0.1:<ephemeral>` and, if given, write the discovery file.
    pub fn start(
        handler: Arc<dyn Handler>,
        discovery: Option<&Path>,
    ) -> Result<Server, ServerError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = random_token()?;
        if let Some(p) = discovery {
            write_discovery(
                p,
                &Discovery {
                    port,
                    token: token.clone(),
                    pid: std::process::id(),
                },
            )?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let token = token.clone();
            std::thread::Builder::new()
                .name("klip-control".into())
                .spawn(move || accept_loop(listener, handler, token, stop))?
        };
        Ok(Server {
            stop,
            thread: Some(thread),
            port,
            token,
            discovery: discovery.map(Path::to_path_buf),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The auth token (for in-process clients and tests).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Stop accepting, and remove the discovery file if it is still ours.
    pub fn shutdown(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(p) = self.discovery.take()
            && let Ok(d) = crate::read_discovery(&p)
            && d.token == self.token
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn accept_loop(
    listener: TcpListener,
    handler: Arc<dyn Handler>,
    token: String,
    stop: Arc<AtomicBool>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    let token: Arc<str> = token.into();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, addr)) => {
                if !addr.ip().is_loopback() || active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    drop(stream);
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let (handler, token, active2) = (handler.clone(), token.clone(), active.clone());
                let spawned = std::thread::Builder::new()
                    .name("klip-control-conn".into())
                    .spawn(move || {
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            if let Err(e) = serve(stream, handler.as_ref(), &token) {
                                tracing::debug!(error = %e, "control connection closed");
                            }
                        }));
                        active2.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                tracing::warn!(error = %e, "control accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve(stream: TcpStream, handler: &dyn Handler, token: &str) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let Some(line) = read_line_capped(&mut reader, MAX_LINE)? else {
        return Ok(());
    };
    let authed = serde_json::from_slice::<Auth>(&line)
        .map(|a| constant_time_eq(a.auth.as_bytes(), token.as_bytes()))
        .unwrap_or(false);
    if !authed {
        let e = CommandError::new("unauthorized", "missing or wrong token");
        let _ = write_json(&mut writer, &error_response(0, &e));
        return Ok(());
    }
    write_json(
        &mut writer,
        &Response {
            id: 0,
            ok: true,
            result: Some(json!({ "protocol": 1 })),
            error: None,
        },
    )?;

    let mut bad = 0u32;
    while let Some(line) = read_line_capped(&mut reader, MAX_LINE)? {
        let req: Request = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                bad = bad.saturating_add(1);
                let err = CommandError::new("bad_request", format!("invalid request: {e}"));
                write_json(&mut writer, &error_response(0, &err))?;
                if bad >= MAX_BAD_REQUESTS {
                    return Ok(());
                }
                continue;
            }
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| handler.handle(&req.cmd, &req.args)));
        let resp = match outcome {
            Ok(Ok(result)) => Response {
                id: req.id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Ok(Err(e)) => error_response(req.id, &e),
            Err(_) => error_response(
                req.id,
                &CommandError::new("internal", "command failed unexpectedly"),
            ),
        };
        write_json(&mut writer, &resp)?;
    }
    Ok(())
}

/// A connected, authenticated client.
pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Connect using a discovery file.
    pub fn connect(discovery: &Path, timeout: Duration) -> Result<Client, ClientError> {
        let d = crate::read_discovery(discovery)?;
        Client::connect_to(d.port, &d.token, timeout)
    }

    /// Connect to a known port with a known token. `timeout` applies to connecting and to
    /// each response.
    pub fn connect_to(port: u16, token: &str, timeout: Duration) -> Result<Client, ClientError> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| {
            if e.kind() == io::ErrorKind::ConnectionRefused || e.kind() == io::ErrorKind::TimedOut {
                ClientError::NotRunning
            } else {
                ClientError::Io(e)
            }
        })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let _ = stream.set_nodelay(true);
        let mut client = Client {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
        };
        write_json(
            &mut client.writer,
            &Auth {
                auth: token.to_string(),
            },
        )?;
        let ack = client.read_response()?;
        if !ack.ok {
            return Err(ClientError::Command(ack.error.unwrap_or(ErrorBody {
                code: "unauthorized".into(),
                message: "rejected".into(),
            })));
        }
        Ok(client)
    }

    fn read_response(&mut self) -> Result<Response, ClientError> {
        let line = read_line_capped(&mut self.reader, MAX_LINE)?
            .ok_or_else(|| ClientError::Protocol("connection closed".into()))?;
        serde_json::from_slice(&line).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Run one command and return its result.
    pub fn call(&mut self, cmd: &str, args: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        write_json(
            &mut self.writer,
            &Request {
                id,
                cmd: cmd.to_string(),
                args,
            },
        )?;
        let resp = self.read_response()?;
        if resp.id != id {
            return Err(ClientError::Protocol(format!(
                "response id {} != {id}",
                resp.id
            )));
        }
        if resp.ok {
            Ok(resp.result.unwrap_or(Value::Null))
        } else {
            Err(ClientError::Command(resp.error.unwrap_or(ErrorBody {
                code: "failed".into(),
                message: "command failed".into(),
            })))
        }
    }
}
