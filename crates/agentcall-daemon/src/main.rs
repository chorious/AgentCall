use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const REPLAY_LIMIT: usize = 512 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = 3293u16;
    let mut workspace = env::current_dir()?;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args
                    .next()
                    .ok_or("missing --port value")?
                    .parse::<u16>()?;
            }
            "--workspace" => {
                workspace = PathBuf::from(args.next().ok_or("missing --workspace value")?);
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }

    let state = Arc::new(AppState::new(normalize_path(workspace)?));
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("AgentCall daemon: http://localhost:{port}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, state) {
                        eprintln!("request error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }
    Ok(())
}

struct AppState {
    workspace: PathBuf,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    seq: AtomicU64,
}

impl AppState {
    fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}

struct Session {
    name: String,
    command: Vec<String>,
    cwd: PathBuf,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send>>,
    status: Mutex<String>,
    created_at: u64,
    updated_at: AtomicU64,
    replay: Mutex<Vec<u8>>,
    clients: Mutex<Vec<Sender<StreamEvent>>>,
}

#[derive(Clone, Serialize)]
struct SessionInfo {
    name: String,
    command: Vec<String>,
    cwd: String,
    status: String,
    created_at: u64,
    updated_at: u64,
    replay_bytes: usize,
}

#[derive(Clone, Serialize)]
struct StreamEvent {
    seq: u64,
    kind: String,
    data: String,
    status: Option<String>,
}

#[derive(Deserialize)]
struct StartRequest {
    name: String,
    command: Vec<String>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

#[derive(Deserialize)]
struct InputRequest {
    text: String,
    enter: Option<bool>,
}

#[derive(Deserialize)]
struct ResizeRequest {
    cols: u16,
    rows: u16,
}

fn handle_connection(mut stream: TcpStream, state: Arc<AppState>) -> std::io::Result<()> {
    let request = read_request(&mut stream)?;
    let response = route(request, state);
    match response {
        Response::Fixed { status, content_type, body } => {
            write_fixed(&mut stream, status, content_type, &body)?;
        }
        Response::Sse { session } => {
            write_sse(stream, session)?;
        }
        Response::WebSocket { session, key } => {
            write_ws(stream, session, key)?;
        }
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

enum Response {
    Fixed {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Sse {
        session: Arc<Session>,
    },
    WebSocket {
        session: Arc<Session>,
        key: String,
    },
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let parts: Vec<&str> = first.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(Request {
            method: String::new(),
            path: String::new(),
            headers: HashMap::new(),
            body: vec![],
        });
    }

    let mut content_length = 0usize;
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse::<usize>().unwrap_or(0);
            }
            headers.insert(name, value);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Request {
        method: parts[0].to_string(),
        path: parts[1].to_string(),
        headers,
        body,
    })
}

fn route(request: Request, state: Arc<AppState>) -> Response {
    let path = request.path.split('?').next().unwrap_or("/");
    match (request.method.as_str(), path) {
        ("GET", "/") => static_file("web/index.html"),
        ("GET", "/app.js") => static_file("web/app.js"),
        ("GET", "/styles.css") => static_file("web/styles.css"),
        ("GET", "/vendor/xterm.js") => static_file("web/vendor/xterm.js"),
        ("GET", "/vendor/xterm.css") => static_file("web/vendor/xterm.css"),
        ("GET", "/vendor/fit-addon.js") => static_file("web/vendor/fit-addon.js"),
        ("GET", "/api/sessions") => json_response(&list_sessions(&state)),
        ("POST", "/api/sessions") => match parse_json::<StartRequest>(&request.body)
            .and_then(|req| start_session(&state, req))
        {
            Ok(info) => json_response(&info),
            Err(err) => error_response(400, &err),
        },
        _ => dynamic_route(request, state),
    }
}

fn dynamic_route(request: Request, state: Arc<AppState>) -> Response {
    let path = request.path.split('?').next().unwrap_or("/");
    let Some(rest) = path.strip_prefix("/api/sessions/") else {
        return error_response(404, "not found");
    };
    let mut parts = rest.rsplitn(2, '/');
    let action = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    let name = url_decode(name);
    match (request.method.as_str(), action) {
        ("GET", "ws") => {
            let key = request.headers.get("sec-websocket-key").cloned();
            match (get_session(&state, &name), key) {
                (Some(session), Some(key)) => Response::WebSocket { session, key },
                (None, _) => error_response(404, "session not found"),
                (_, None) => error_response(400, "missing websocket key"),
            }
        }
        ("GET", "stream") => match get_session(&state, &name) {
            Some(session) => Response::Sse { session },
            None => error_response(404, "session not found"),
        },
        ("POST", "input") => match parse_json::<InputRequest>(&request.body)
            .and_then(|req| write_input(&state, &name, req))
        {
            Ok(()) => json_response(&serde_json::json!({"ok": true})),
            Err(err) => error_response(400, &err),
        },
        ("POST", "resize") => match parse_json::<ResizeRequest>(&request.body)
            .and_then(|req| resize_session(&state, &name, req))
        {
            Ok(()) => json_response(&serde_json::json!({"ok": true})),
            Err(err) => error_response(400, &err),
        },
        ("POST", "stop") => match stop_session(&state, &name) {
            Ok(()) => json_response(&serde_json::json!({"ok": true})),
            Err(err) => error_response(400, &err),
        },
        _ => error_response(404, "not found"),
    }
}

fn start_session(state: &Arc<AppState>, req: StartRequest) -> Result<SessionInfo, String> {
    if !safe_name(&req.name) {
        return Err("unsafe session name".to_string());
    }
    if req.command.is_empty() {
        return Err("missing command".to_string());
    }
    if state.sessions.lock().unwrap().contains_key(&req.name) {
        return Err("session already exists".to_string());
    }

    let cwd = req
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| state.workspace.clone());
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: req.rows.unwrap_or(40),
            cols: req.cols.unwrap_or(100),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| err.to_string())?;

    let mut command = CommandBuilder::new(&req.command[0]);
    for arg in req.command.iter().skip(1) {
        command.arg(arg);
    }
    command.cwd(&cwd);

    let child = pair.slave.spawn_command(command).map_err(|err| err.to_string())?;
    let reader = pair.master.try_clone_reader().map_err(|err| err.to_string())?;
    let writer = pair.master.take_writer().map_err(|err| err.to_string())?;

    let session = Arc::new(Session {
        name: req.name.clone(),
        command: req.command,
        cwd,
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        status: Mutex::new("running".to_string()),
        created_at: now_ms(),
        updated_at: AtomicU64::new(now_ms()),
        replay: Mutex::new(Vec::new()),
        clients: Mutex::new(Vec::new()),
    });

    state
        .sessions
        .lock()
        .unwrap()
        .insert(session.name.clone(), Arc::clone(&session));
    spawn_reader(Arc::clone(state), Arc::clone(&session), reader);
    spawn_waiter(Arc::clone(state), Arc::clone(&session));
    Ok(session_info(&session))
}

fn spawn_reader(state: Arc<AppState>, session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let bytes = &buf[..n];
                    {
                        let mut replay = session.replay.lock().unwrap();
                        replay.extend_from_slice(bytes);
                        if replay.len() > REPLAY_LIMIT {
                            let drop = replay.len() - REPLAY_LIMIT;
                            replay.drain(..drop);
                        }
                    }
                    session.updated_at.store(now_ms(), Ordering::Relaxed);
                    let data = String::from_utf8_lossy(bytes).to_string();
                    broadcast(
                        &session,
                        StreamEvent {
                            seq: state.next_seq(),
                            kind: "output".to_string(),
                            data,
                            status: None,
                        },
                    );
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_waiter(state: Arc<AppState>, session: Arc<Session>) {
    thread::spawn(move || {
        let status = {
            let mut child = session.child.lock().unwrap();
            match child.wait() {
                Ok(exit) => format!("exited:{exit:?}"),
                Err(err) => format!("error:{err}"),
            }
        };
        *session.status.lock().unwrap() = status.clone();
        session.updated_at.store(now_ms(), Ordering::Relaxed);
        broadcast(
            &session,
            StreamEvent {
                seq: state.next_seq(),
                kind: "status".to_string(),
                data: String::new(),
                status: Some(status),
            },
        );
    });
}

fn list_sessions(state: &AppState) -> Vec<SessionInfo> {
    let mut sessions: Vec<SessionInfo> = state
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(session_info)
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    sessions
}

fn session_info(session: &Arc<Session>) -> SessionInfo {
    SessionInfo {
        name: session.name.clone(),
        command: session.command.clone(),
        cwd: session.cwd.display().to_string(),
        status: session.status.lock().unwrap().clone(),
        created_at: session.created_at,
        updated_at: session.updated_at.load(Ordering::Relaxed),
        replay_bytes: session.replay.lock().unwrap().len(),
    }
}

fn get_session(state: &AppState, name: &str) -> Option<Arc<Session>> {
    state.sessions.lock().unwrap().get(name).cloned()
}

fn write_input(state: &AppState, name: &str, req: InputRequest) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    let mut payload = req.text;
    if req.enter.unwrap_or(true) {
        payload.push('\r');
    }
    session
        .writer
        .lock()
        .unwrap()
        .write_all(payload.as_bytes())
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn resize_session(state: &AppState, name: &str, req: ResizeRequest) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    session
        .master
        .lock()
        .unwrap()
        .resize(PtySize {
            rows: req.rows,
            cols: req.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn stop_session(state: &AppState, name: &str) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    session
        .child
        .lock()
        .unwrap()
        .kill()
        .map_err(|err| err.to_string())
}

fn broadcast(session: &Arc<Session>, event: StreamEvent) {
    let mut clients = session.clients.lock().unwrap();
    clients.retain(|tx| tx.send(event.clone()).is_ok());
}

fn write_sse(mut stream: TcpStream, session: Arc<Session>) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )?;
    let replay = {
        let bytes = session.replay.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).to_string()
    };
    if !replay.is_empty() {
        write_event(
            &mut stream,
            &StreamEvent {
                seq: 0,
                kind: "replay".to_string(),
                data: replay,
                status: None,
            },
        )?;
    }

    let (tx, rx) = mpsc::channel::<StreamEvent>();
    session.clients.lock().unwrap().push(tx);
    for event in rx {
        if write_event(&mut stream, &event).is_err() {
            break;
        }
    }
    Ok(())
}

fn write_event(stream: &mut TcpStream, event: &StreamEvent) -> std::io::Result<()> {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    stream.write_all(format!("data: {data}\n\n").as_bytes())?;
    stream.flush()
}

fn write_ws(mut stream: TcpStream, session: Arc<Session>, key: String) -> std::io::Result<()> {
    let accept = websocket_accept_key(&key);
    stream.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;

    let replay = {
        let bytes = session.replay.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).to_string()
    };
    if !replay.is_empty() {
        let event = StreamEvent {
            seq: 0,
            kind: "replay".to_string(),
            data: replay,
            status: None,
        };
        write_ws_text(&mut stream, &serde_json::to_string(&event).unwrap())?;
    }

    let (tx, rx) = mpsc::channel::<StreamEvent>();
    session.clients.lock().unwrap().push(tx);
    let mut writer = stream.try_clone()?;
    thread::spawn(move || {
        for event in rx {
            let Ok(data) = serde_json::to_string(&event) else {
                continue;
            };
            if write_ws_text(&mut writer, &data).is_err() {
                break;
            }
        }
    });

    while let Some(frame) = read_ws_frame(&mut stream)? {
        match frame.opcode {
            0x1 => {
                if let Ok(text) = String::from_utf8(frame.payload) {
                    handle_ws_message(&session, &text);
                }
            }
            0x8 => break,
            0x9 => {
                write_ws_frame(&mut stream, 0xA, &frame.payload)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_ws_message(session: &Arc<Session>, text: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    match value.get("type").and_then(|v| v.as_str()) {
        Some("input") => {
            if let Some(data) = value.get("data").and_then(|v| v.as_str()) {
                let _ = session.writer.lock().unwrap().write_all(data.as_bytes());
            }
        }
        Some("resize") => {
            let cols = value.get("cols").and_then(|v| v.as_u64()).unwrap_or(100) as u16;
            let rows = value.get("rows").and_then(|v| v.as_u64()).unwrap_or(36) as u16;
            let _ = session.master.lock().unwrap().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        _ => {}
    }
}

struct WsFrame {
    opcode: u8,
    payload: Vec<u8>,
}

fn read_ws_frame(stream: &mut TcpStream) -> std::io::Result<Option<WsFrame>> {
    let mut head = [0u8; 2];
    match stream.read_exact(&mut head) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    let mut len = (head[1] & 0x7F) as u64;
    if len == 126 {
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf)?;
        len = u16::from_be_bytes(buf) as u64;
    } else if len == 127 {
        let mut buf = [0u8; 8];
        stream.read_exact(&mut buf)?;
        len = u64::from_be_bytes(buf);
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Ok(Some(WsFrame { opcode, payload }))
}

fn write_ws_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    write_ws_frame(stream, 0x1, text.as_bytes())
}

fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(&[0x80 | opcode])?;
    if payload.len() < 126 {
        stream.write_all(&[payload.len() as u8])?;
    } else if payload.len() <= u16::MAX as usize {
        stream.write_all(&[126])?;
        stream.write_all(&(payload.len() as u16).to_be_bytes())?;
    } else {
        stream.write_all(&[127])?;
        stream.write_all(&(payload.len() as u64).to_be_bytes())?;
    }
    stream.write_all(payload)?;
    stream.flush()
}

fn websocket_accept_key(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let bit_len = (input.len() as u64) * 8;
    let mut msg = input.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let offset = i * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn static_file(path: &str) -> Response {
    let body = fs::read(path).unwrap_or_else(|_| b"not found".to_vec());
    let status = if body == b"not found".to_vec() { 404 } else { 200 };
    let content_type = match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        _ => "application/octet-stream",
    };
    Response::Fixed {
        status,
        content_type,
        body,
    }
}

fn json_response<T: Serialize>(value: &T) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::Fixed {
        status: 200,
        content_type: "application/json; charset=utf-8",
        body,
    }
}

fn error_response(status: u16, message: &str) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap();
    Response::Fixed {
        status,
        content_type: "application/json; charset=utf-8",
        body,
    }
}

fn write_fixed(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)
}

fn parse_json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|err| err.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn normalize_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn url_decode(value: &str) -> String {
    value.replace("%20", " ")
        .replace("%2F", "/")
        .replace("%5C", "\\")
        .replace("%3A", ":")
}
