//! The sync shell: one WebSocket per attempt, binary `doc` frames, and the
//! replica that outlives every socket.
//!
//! The first frame on each socket is the server's Welcome. The first ever
//! Welcome builds the replica (`join`); later ones adopt the server state and
//! resend what is still queued (`rejoin`). Nothing is sent before it.

use std::rc::Rc;

use doc::{Change, Edit, Frame, Presence, Reject, Replica, Update, WireError};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};

/// The room from the URL hash, made to fit the server's `^[a-z0-9-]{1,64}$`.
pub fn room_from_hash(hash: &str) -> String {
    let room: String = hash
        .trim_start_matches('#')
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(64)
        .collect();
    if room.is_empty() {
        "lobby".into()
    } else {
        room
    }
}

/// `ws(s)://<this page's host><this page's directory>ws/<room>`, so the app
/// works under a path prefix such as `/draw/`.
pub fn url(room: &str) -> String {
    let loc = web_sys::window().expect("window").location();
    let scheme = match loc.protocol().as_deref() {
        Ok("https:") => "wss",
        _ => "ws",
    };
    let host = loc.host().unwrap_or_else(|_| "localhost:8080".into());
    let path = loc.pathname().unwrap_or_default();
    format!("{scheme}://{host}{}ws/{room}", dir(&path))
}

/// Everything up to and including the last `/`.
fn dir(path: &str) -> &str {
    path.rfind('/').map_or("/", |i| &path[..=i])
}

/// What one server frame did.
#[derive(Debug)]
pub enum Got {
    /// The replica was built or rebuilt from a Welcome. Rebuild everything.
    Welcome,
    Update(Update),
}

/// A live socket and the handlers that keep it pumping.
#[allow(dead_code, reason = "the closures must live as long as the socket")]
#[derive(Debug)]
pub struct Socket {
    ws: WebSocket,
    on_open: Closure<dyn FnMut()>,
    on_message: Closure<dyn FnMut(MessageEvent)>,
    on_close: Closure<dyn FnMut()>,
}

impl Socket {
    /// Opens a socket. `frame` gets every binary message; `opened` and
    /// `closed` fire once each.
    pub fn open(
        url: &str,
        opened: Rc<dyn Fn()>,
        frame: Rc<dyn Fn(Vec<u8>)>,
        closed: Rc<dyn Fn()>,
    ) -> Option<Socket> {
        let ws = WebSocket::new(url).ok()?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let on_open = Closure::<dyn FnMut()>::new(move || opened());
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                frame(js_sys::Uint8Array::new(&buf).to_vec());
            }
        });
        let on_close = Closure::<dyn FnMut()>::new(move || closed());
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Some(Socket {
            ws,
            on_open,
            on_message,
            on_close,
        })
    }

    fn send(&self, f: &Frame) {
        let _ = self.ws.send_with_u8_array(f.as_bytes());
    }

    fn close(&self) {
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

#[derive(Debug)]
pub struct Collab {
    pub room: String,
    replica: Option<Replica>,
    socket: Option<Socket>,
    /// The current socket has delivered its Welcome.
    welcomed: bool,
    /// Consecutive failed attempts, for backoff.
    attempts: u32,
}

impl Collab {
    pub fn new(room: String) -> Collab {
        Collab {
            room,
            replica: None,
            socket: None,
            welcomed: false,
            attempts: 0,
        }
    }

    pub fn replica(&self) -> Option<&Replica> {
        self.replica.as_ref()
    }

    pub fn replica_mut(&mut self) -> Option<&mut Replica> {
        self.replica.as_mut()
    }

    pub fn online(&self) -> bool {
        self.welcomed
    }

    pub fn attach(&mut self, socket: Socket) {
        if let Some(old) = self.socket.take() {
            old.close();
        }
        self.socket = Some(socket);
        self.welcomed = false;
    }

    pub fn opened(&self) {
        let hello = match &self.replica {
            None => Replica::hello(),
            Some(r) => r.hello_again(),
        };
        if let Some(s) = &self.socket {
            s.send(&hello);
        }
    }

    pub fn receive(&mut self, bytes: &[u8]) -> Result<Got, WireError> {
        if !self.welcomed {
            let resend = match &mut self.replica {
                None => {
                    self.replica = Some(Replica::join(bytes)?);
                    Vec::new()
                }
                Some(r) => r.rejoin(bytes)?,
            };
            self.welcomed = true;
            self.attempts = 0;
            for f in &resend {
                self.send(f);
            }
            return Ok(Got::Welcome);
        }
        let r = self.replica.as_mut().ok_or(WireError::Unexpected)?;
        r.receive(bytes).map(Got::Update)
    }

    /// Applies locally and sends if online. Offline edits are resent by
    /// `rejoin`. None before the first Welcome.
    pub fn edit(&mut self, changes: Vec<Change>) -> Option<Result<Edit, Reject>> {
        let r = self.replica.as_mut()?;
        let result = r.edit(changes);
        if let Ok(e) = &result {
            self.send(&e.send);
        }
        Some(result)
    }

    pub fn presence(&self, p: &Presence) {
        if let Some(r) = &self.replica {
            self.send(&r.presence(p));
        }
    }

    fn send(&self, f: &Frame) {
        if let (true, Some(s)) = (self.welcomed, &self.socket) {
            s.send(f);
        }
    }

    /// The socket is gone. Returns how long to wait before the next try.
    pub fn closed(&mut self) -> u32 {
        if let Some(old) = self.socket.take() {
            old.close();
        }
        self.welcomed = false;
        self.attempts = self.attempts.saturating_add(1);
        (250u32 << self.attempts.min(5)).min(5000)
    }
}
