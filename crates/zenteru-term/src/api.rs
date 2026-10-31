//! The plugin API server (docs/PROTOCOL.md): a Unix socket speaking
//! newline-delimited JSON. Connection handling, cookie auth, subscriptions
//! and registrations live here; the app answers everything else from its
//! main loop via `poll` / `reply`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use crate::json::Json;

pub type ConnId = u64;

#[derive(Debug)]
pub enum Incoming {
    Request { conn: ConnId, id: Json, method: String, params: Json },
    /// Answer to a server-initiated `rpc.invoke`.
    Response { conn: ConnId, id: String, result: Result<Json, Json> },
    Closed(ConnId),
}

#[derive(Clone, Debug)]
pub struct Subscription {
    pub conn: ConnId,
    pub id: u64,
    pub kind: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Registration {
    pub conn: ConnId,
    pub handler: String,
    pub kind: String,
    pub params: Json,
}

struct Conn {
    stream: Mutex<UnixStream>,
    authed: Mutex<bool>,
    name: Mutex<String>,
}

struct Shared {
    conns: Mutex<HashMap<ConnId, Arc<Conn>>>,
    subs: Mutex<Vec<Subscription>>,
    regs: Mutex<Vec<Registration>>,
    cookie: String,
    version: String,
    next_sub: AtomicU64,
}

pub struct ApiServer {
    shared: Arc<Shared>,
    inbox: Receiver<Incoming>,
    next_call: AtomicU64,
    pub socket_path: PathBuf,
}

impl ApiServer {
    /// Bind `dir/api.sock` (dir is created 0700) and start accepting.
    pub fn start(dir: &Path, cookie: String, version: &str) -> io::Result<ApiServer> {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let socket_path = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        let cookie_path = dir.join("cookie");
        std::fs::write(&cookie_path, &cookie)?;
        std::fs::set_permissions(&cookie_path, std::fs::Permissions::from_mode(0o600))?;

        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            subs: Mutex::new(Vec::new()),
            regs: Mutex::new(Vec::new()),
            cookie,
            version: version.to_string(),
            next_sub: AtomicU64::new(1),
        });
        let (tx, rx) = channel();
        let sh = shared.clone();
        std::thread::Builder::new().name("api-accept".into()).spawn(move || {
            let mut next: ConnId = 1;
            for stream in listener.incoming().flatten() {
                if !same_user(&stream) {
                    continue;
                }
                let id = next;
                next += 1;
                let Ok(w) = stream.try_clone() else { continue };
                let conn = Arc::new(Conn { stream: Mutex::new(w), authed: Mutex::new(false), name: Mutex::new(String::new()) });
                sh.conns.lock().unwrap().insert(id, conn.clone());
                let (sh2, tx2) = (sh.clone(), tx.clone());
                let _ = std::thread::Builder::new().name(format!("api-conn-{id}")).spawn(move || serve(id, stream, conn, sh2, tx2));
            }
        })?;
        Ok(ApiServer { shared, inbox: rx, next_call: AtomicU64::new(1), socket_path })
    }

    /// Everything that arrived since the last poll.
    pub fn poll(&self) -> Vec<Incoming> {
        self.inbox.try_iter().collect()
    }

    pub fn conn_name(&self, conn: ConnId) -> String {
        self.shared.conns.lock().unwrap().get(&conn).map(|c| c.name.lock().unwrap().clone()).unwrap_or_default()
    }

    pub fn reply(&self, conn: ConnId, id: &Json, result: Result<Json, (&str, String)>) {
        let msg = match result {
            Ok(r) => Json::obj().with("id", id.clone()).with("result", r),
            Err((code, m)) => Json::obj().with("id", id.clone()).with("error", Json::obj().with("code", code).with("message", m)),
        };
        send(&self.shared, conn, &msg);
    }

    /// Deliver an event to every matching subscription.
    pub fn emit(&self, kind: &str, session_id: Option<&str>, data: Json) {
        let subs: Vec<Subscription> = self.shared.subs.lock().unwrap().iter().filter(|s| s.kind == kind).cloned().collect();
        for s in subs {
            if let (Some(want), Some(have)) = (&s.session_id, session_id) {
                if want != have && want != "all" {
                    continue;
                }
            }
            let msg = Json::obj().with("event", kind).with("subscription", s.id).with("data", data.clone());
            send(&self.shared, s.conn, &msg);
        }
    }

    pub fn has_subscribers(&self, kind: &str) -> bool {
        self.shared.subs.lock().unwrap().iter().any(|s| s.kind == kind)
    }

    /// Start a server-initiated call; the answer arrives as `Incoming::Response`.
    pub fn call(&self, conn: ConnId, handler: &str, args: Json) -> String {
        let id = format!("srv{}", self.next_call.fetch_add(1, Ordering::Relaxed));
        let msg = Json::obj()
            .with("id", id.as_str())
            .with("method", "rpc.invoke")
            .with("params", Json::obj().with("handler", handler).with("args", args));
        send(&self.shared, conn, &msg);
        id
    }

    pub fn registrations(&self, kind: &str) -> Vec<Registration> {
        self.shared.regs.lock().unwrap().iter().filter(|r| r.kind == kind).cloned().collect()
    }

    pub fn registration(&self, handler: &str) -> Option<Registration> {
        self.shared.regs.lock().unwrap().iter().find(|r| r.handler == handler).cloned()
    }
}

fn send(sh: &Shared, conn: ConnId, msg: &Json) {
    let c = sh.conns.lock().unwrap().get(&conn).cloned();
    if let Some(c) = c {
        let mut line = msg.to_string();
        line.push('\n');
        let mut s = c.stream.lock().unwrap();
        if s.write_all(line.as_bytes()).is_err() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn err(code: &str, m: &str) -> Json {
    Json::obj().with("code", code).with("message", m)
}

fn serve(id: ConnId, stream: UnixStream, conn: Arc<Conn>, sh: Arc<Shared>, tx: Sender<Incoming>) {
    let reader = BufReader::new(stream);
    for line in reader.split(b'\n') {
        let Ok(line) = line else { break };
        if line.len() > 32 << 20 {
            break;
        }
        let Ok(text) = String::from_utf8(line) else { continue };
        if text.trim().is_empty() {
            continue;
        }
        let msg = match Json::parse(&text) {
            Ok(m) => m,
            Err(e) => {
                send(&sh, id, &Json::obj().with("id", Json::Null).with("error", err("bad_request", &e)));
                continue;
            }
        };
        let rid = msg.get("id").clone();
        // A response to one of our calls?
        if msg.get("method").is_null() {
            if let Some(s) = rid.as_str() {
                let result = if msg.get("error").is_null() { Ok(msg.get("result").clone()) } else { Err(msg.get("error").clone()) };
                if *conn.authed.lock().unwrap() {
                    let _ = tx.send(Incoming::Response { conn: id, id: s.to_string(), result });
                }
            }
            continue;
        }
        let method = msg.get("method").as_str().unwrap_or("").to_string();
        let params = msg.get("params").clone();
        let reply = |r: Result<Json, Json>| {
            let m = match r {
                Ok(v) => Json::obj().with("id", rid.clone()).with("result", v),
                Err(e) => Json::obj().with("id", rid.clone()).with("error", e),
            };
            send(&sh, id, &m);
        };
        if method == "auth.hello" {
            let ok = params.get("cookie").as_str().is_some_and(|c| ct_eq(c.as_bytes(), sh.cookie.as_bytes()));
            if ok {
                *conn.authed.lock().unwrap() = true;
                *conn.name.lock().unwrap() = params.get("name").as_str().unwrap_or("plugin").to_string();
                reply(Ok(Json::obj().with("app", "zenteru").with("version", sh.version.as_str()).with("protocol", 1)));
            } else {
                reply(Err(err("unauthorized", "bad cookie")));
            }
            continue;
        }
        if !*conn.authed.lock().unwrap() {
            reply(Err(err("unauthorized", "call auth.hello first")));
            continue;
        }
        match method.as_str() {
            "notify.subscribe" => {
                let Some(kind) = params.get("kind").as_str() else {
                    reply(Err(err("bad_request", "kind required")));
                    continue;
                };
                let sid = sh.next_sub.fetch_add(1, Ordering::Relaxed);
                sh.subs.lock().unwrap().push(Subscription {
                    conn: id,
                    id: sid,
                    kind: kind.to_string(),
                    session_id: params.get("session_id").as_str().map(String::from),
                });
                reply(Ok(Json::obj().with("subscription", sid)));
            }
            "notify.unsubscribe" => {
                let sid = params.get("subscription").as_i64().unwrap_or(-1) as u64;
                sh.subs.lock().unwrap().retain(|s| !(s.conn == id && s.id == sid));
                reply(Ok(Json::obj()));
            }
            "rpc.register" => {
                let (Some(handler), Some(kind)) = (params.get("handler").as_str(), params.get("kind").as_str()) else {
                    reply(Err(err("bad_request", "handler and kind required")));
                    continue;
                };
                let mut regs = sh.regs.lock().unwrap();
                regs.retain(|r| r.handler != handler);
                regs.push(Registration { conn: id, handler: handler.to_string(), kind: kind.to_string(), params: params.clone() });
                drop(regs);
                reply(Ok(Json::obj()));
                // Let the app refresh menus / keybindings.
                let _ = tx.send(Incoming::Request { conn: id, id: Json::Null, method: "_registered".into(), params });
            }
            "rpc.unregister" => {
                let handler = params.get("handler").as_str().unwrap_or("");
                sh.regs.lock().unwrap().retain(|r| !(r.conn == id && r.handler == handler));
                reply(Ok(Json::obj()));
            }
            _ => {
                let _ = tx.send(Incoming::Request { conn: id, id: rid, method, params });
            }
        }
    }
    sh.conns.lock().unwrap().remove(&id);
    sh.subs.lock().unwrap().retain(|s| s.conn != id);
    sh.regs.lock().unwrap().retain(|r| r.conn != id);
    let _ = tx.send(Incoming::Closed(id));
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Only accept peers running as our own user.
fn same_user(s: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn getpeereid(fd: i32, uid: *mut u32, gid: *mut u32) -> i32;
        fn getuid() -> u32;
    }
    let (mut uid, mut gid) = (0u32, 0u32);
    unsafe { getpeereid(s.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == getuid() }
}

/// A random hex cookie from /dev/urandom.
pub fn random_cookie() -> String {
    use std::io::Read;
    let mut b = [0u8; 24];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn auth_subscribe_and_request() {
        let dir = std::env::temp_dir().join(format!("zt-api-test-{}", std::process::id()));
        let srv = ApiServer::start(&dir, "secret".into(), "0.1.0").unwrap();
        let s = UnixStream::connect(&srv.socket_path).unwrap();
        let mut w = s.try_clone().unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        let mut rpc = |w: &mut UnixStream, r: &mut BufReader<UnixStream>, msg: &str| {
            w.write_all(msg.as_bytes()).unwrap();
            w.write_all(b"\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            Json::parse(&line).unwrap()
        };
        let v = rpc(&mut w, &mut r, r#"{"id":1,"method":"app.get"}"#);
        assert_eq!(v.get("error").get("code").as_str(), Some("unauthorized"));
        let v = rpc(&mut w, &mut r, r#"{"id":2,"method":"auth.hello","params":{"cookie":"secret","name":"t"}}"#);
        assert_eq!(v.get("result").get("protocol").as_i64(), Some(1));
        let v = rpc(&mut w, &mut r, r#"{"id":3,"method":"notify.subscribe","params":{"kind":"layout"}}"#);
        assert!(v.get("result").get("subscription").as_i64().is_some());
        w.write_all(b"{\"id\":4,\"method\":\"app.get\"}\n").unwrap();
        let mut got = None;
        for _ in 0..100 {
            if let Some(Incoming::Request { method, id, conn, .. }) = srv.poll().into_iter().find(|i| matches!(i, Incoming::Request { method, .. } if method == "app.get")) {
                srv.reply(conn, &id, Ok(Json::obj().with("ok", true)));
                got = Some(method);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got.as_deref(), Some("app.get"));
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        assert_eq!(Json::parse(&l).unwrap().get("result").get("ok").as_bool(), Some(true));
        srv.emit("layout", None, Json::obj());
        l.clear();
        r.read_line(&mut l).unwrap();
        assert_eq!(Json::parse(&l).unwrap().get("event").as_str(), Some("layout"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
