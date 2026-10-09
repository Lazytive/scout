use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::server::read_line_capped;
use super::*;

struct Echo;

impl Handler for Echo {
    fn handle(&self, cmd: &str, args: &Value) -> Result<Value, CommandError> {
        match cmd {
            commands::PING => Ok(json!({ "pong": true })),
            "echo" => Ok(args.clone()),
            "boom" => panic!("handler bug"),
            other => Err(CommandError::unknown(other)),
        }
    }
}

const T: Duration = Duration::from_secs(5);

fn start() -> (Server, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let disc = dir.path().join("sub").join("control.json");
    let s = Server::start(Arc::new(Echo), Some(&disc)).unwrap();
    (s, dir, disc)
}

#[test]
fn round_trip_via_discovery_file() {
    let (server, _dir, disc) = start();
    let d = read_discovery(&disc).unwrap();
    assert_eq!(d.port, server.port());
    assert_eq!(d.pid, std::process::id());
    let mut c = Client::connect(&disc, T).unwrap();
    assert_eq!(
        c.call(commands::PING, Value::Null).unwrap(),
        json!({"pong": true})
    );
    assert_eq!(
        c.call("echo", json!({"a": [1, 2]})).unwrap(),
        json!({"a": [1, 2]})
    );
    match c.call("nope", Value::Null) {
        Err(ClientError::Command(e)) => assert_eq!(e.code, "unknown_command"),
        other => panic!("{other:?}"),
    }
    // A panicking handler becomes an error response; the connection survives.
    match c.call("boom", Value::Null) {
        Err(ClientError::Command(e)) => assert_eq!(e.code, "internal"),
        other => panic!("{other:?}"),
    }
    assert!(c.call(commands::PING, Value::Null).is_ok());
    server.shutdown();
    assert!(!disc.exists(), "discovery file removed on shutdown");
}

#[test]
fn wrong_or_missing_token_is_rejected() {
    let (server, _dir, _disc) = start();
    let bad = "0".repeat(64);
    assert!(matches!(
        Client::connect_to(server.port(), &bad, T),
        Err(ClientError::Command(e)) if e.code == "unauthorized"
    ));
    // An HTTP request (e.g. from a web page) fails auth and gets nothing else.
    let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("unauthorized"), "{line}");
    line.clear();
    assert_eq!(
        r.read_line(&mut line).unwrap(),
        0,
        "connection closed after failed auth"
    );
}

#[test]
fn malformed_and_oversized_requests() {
    let (server, _dir, _disc) = start();
    let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    writeln!(s, "{}", json!({"auth": server.token()})).unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.contains("\"ok\":true"));
    for bad in [
        "not json",
        "{\"id\":1}",
        "{\"id\":1,\"cmd\":\"ping\",\"evil\":1}",
        "[]",
    ] {
        writeln!(s, "{bad}").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("bad_request"), "{bad} -> {line}");
    }
    // Oversized line: the server closes the connection instead of buffering it.
    let big = vec![b'a'; MAX_LINE + 10];
    let _ = s.write_all(&big);
    let _ = s.write_all(b"\n");
    line.clear();
    let n = r.read_line(&mut line).unwrap_or(0);
    assert_eq!(n, 0, "closed: {line}");
}

#[test]
fn line_reader_caps_and_handles_eof() {
    let mut c = Cursor::new(b"abc\ndef".to_vec());
    assert_eq!(read_line_capped(&mut c, 10).unwrap().unwrap(), b"abc\n");
    assert_eq!(read_line_capped(&mut c, 10).unwrap().unwrap(), b"def");
    assert!(read_line_capped(&mut c, 10).unwrap().is_none());
    let mut c = Cursor::new(vec![b'x'; 100]);
    assert!(read_line_capped(&mut c, 10).is_err());
}

#[test]
fn hostile_discovery_files() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("control.json");
    assert!(matches!(read_discovery(&p), Err(ClientError::NotRunning)));
    for body in [
        "garbage".to_string(),
        json!({"port": 0, "token": "a".repeat(64), "pid": 1}).to_string(),
        json!({"port": 5, "token": "short", "pid": 1}).to_string(),
        json!({"port": 5, "token": "z".repeat(64), "pid": 1}).to_string(),
        json!({"port": 5, "token": "a".repeat(64), "pid": 1, "x": 2}).to_string(),
        " ".repeat(5000),
    ] {
        std::fs::write(&p, &body).unwrap();
        assert!(
            matches!(read_discovery(&p), Err(ClientError::Protocol(_))),
            "{body}"
        );
    }
}

#[test]
fn client_reports_not_running() {
    // Bind and drop a listener to get a port that is very likely closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(matches!(
        Client::connect_to(port, &"a".repeat(64), Duration::from_millis(500)),
        Err(ClientError::NotRunning)
    ));
}

#[test]
fn concurrent_clients() {
    let (server, _dir, disc) = start();
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let disc = disc.clone();
            std::thread::spawn(move || {
                let mut c = Client::connect(&disc, T).unwrap();
                for j in 0..20 {
                    assert_eq!(
                        c.call("echo", json!(i * 100 + j)).unwrap(),
                        json!(i * 100 + j)
                    );
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    drop(server);
}
