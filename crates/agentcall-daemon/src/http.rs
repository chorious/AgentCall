use crate::actor::submit_session_command;
use crate::commands::{CommandEnvelopeV1, CommandType};
use crate::commands::{PreparedCommand, prepare_session_send_command};
use crate::hooks::{
    EventAppendRequest, HookIngestRequest, append_event_request, file_claims_state, ingest_hook,
    unmatched_hooks_state,
};
use crate::mcp::{McpCallRequest, mcp_call, mcp_tools};
use crate::routes::{
    ContextRequest, RouteRequest, TranscriptIndexRequest, checkpoint_session, create_context,
    handle_route, index_transcript, route_state,
};
use crate::session::{
    ResizeRequest, Session, StartRequest, StreamEvent, get_session, list_sessions, resize_session,
    start_session,
};
use crate::state::{AppState, append_agent_event};
use crate::summary::{
    board_owner_filter, board_state, deprecated_clean_tail_value, projects_state, runtime_health,
    session_summary, terminal_snapshot_value,
};
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_WS_FRAME_BYTES: u64 = 64 * 1024;

pub(crate) fn handle_connection(
    mut stream: TcpStream,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let request = read_request(&mut stream)?;
    let response = route(request, state);
    match response {
        Response::Fixed {
            status,
            content_type,
            body,
        } => {
            write_fixed(&mut stream, status, content_type, &body)?;
        }
        Response::Sse { session } => {
            write_sse(stream, session)?;
        }
        Response::WebSocket {
            state,
            session,
            key,
        } => {
            write_ws(stream, state, session, key)?;
        }
    }
    Ok(())
}

pub(crate) struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

pub(crate) enum Response {
    Fixed {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Sse {
        session: Arc<Session>,
    },
    WebSocket {
        state: Arc<AppState>,
        session: Arc<Session>,
        key: String,
    },
}

pub(crate) fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
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
    if content_length > MAX_HTTP_BODY_BYTES {
        return Ok(Request {
            method: "__payload_too_large".to_string(),
            path: String::new(),
            headers,
            body: vec![],
        });
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

pub(crate) fn route(request: Request, state: Arc<AppState>) -> Response {
    let path = request.path.split('?').next().unwrap_or("/");
    if request.method == "__payload_too_large" {
        return error_response(413, "payload too large");
    }
    if path.starts_with("/api/") {
        if let Err(response) = authorize_api_request(&request, &state) {
            return response;
        }
    }
    match (request.method.as_str(), path) {
        ("GET", "/") => static_file("web/index.html"),
        ("GET", "/board") => static_file("web/board.html"),
        ("GET", "/app.js") => static_file("web/app.js"),
        ("GET", "/board.js") => static_file("web/board.js"),
        ("GET", "/board.css") => static_file("web/board.css"),
        ("GET", "/styles.css") => static_file("web/styles.css"),
        ("GET", "/vendor/xterm.js") => static_file("web/vendor/xterm.js"),
        ("GET", "/vendor/xterm.css") => static_file("web/vendor/xterm.css"),
        ("GET", "/vendor/fit-addon.js") => static_file("web/vendor/fit-addon.js"),
        ("GET", "/api/sessions") => json_response(&list_sessions(&state)),
        ("GET", "/api/board") => {
            let query = query_params(&request.path);
            let owner_id = board_owner_filter(
                query.get("scope").map(String::as_str),
                query.get("owner_id").map(String::as_str),
            );
            json_response(&board_state(
                &state,
                query.get("view").map(String::as_str),
                query.get("filter").map(String::as_str),
                query.get("section").map(String::as_str),
                owner_id.as_deref(),
                query
                    .get("root")
                    .or_else(|| query.get("workspace"))
                    .map(String::as_str),
            ))
        }
        ("GET", "/api/runtime/health") => json_response(&runtime_health(&state)),
        ("GET", "/api/projects") => json_response(&projects_state(&state)),
        ("GET", "/api/file-claims") => json_response(&file_claims_state(&state)),
        ("GET", "/api/hooks/unmatched") => json_response(&unmatched_hooks_state(&state)),
        ("GET", "/api/mcp/tools") => json_response(&mcp_tools()),
        ("POST", "/api/mcp/call") => match parse_json::<McpCallRequest>(&request.body)
            .and_then(|req| mcp_call(&state, req))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "/api/events") => match parse_json::<EventAppendRequest>(&request.body)
            .and_then(|req| append_event_request(&state, req))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "/api/hooks/ingest") => match parse_json::<HookIngestRequest>(&request.body)
            .and_then(|req| ingest_hook(&state, req))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "/api/routes") => match parse_json::<RouteRequest>(&request.body)
            .and_then(|req| handle_route(&state, req))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "/api/context") => match parse_json::<ContextRequest>(&request.body)
            .and_then(|req| create_context(&state, req))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "/api/transcripts/index") => {
            match parse_json::<TranscriptIndexRequest>(&request.body)
                .and_then(|req| index_transcript(&state, req))
            {
                Ok(result) => json_response(&result),
                Err(err) => error_response(400, &err),
            }
        }
        ("POST", "/api/sessions") => match parse_json::<StartRequest>(&request.body)
            .and_then(|req| start_session(&state, req))
        {
            Ok(info) => json_response(&info),
            Err(err) => error_response(400, &err),
        },
        _ => dynamic_route(request, state),
    }
}

fn authorize_api_request(request: &Request, state: &AppState) -> Result<(), Response> {
    if !host_header_is_loopback(request) {
        return Err(error_response(403, "host not allowed"));
    }
    if !origin_header_is_loopback(request) {
        return Err(error_response(403, "origin not allowed"));
    }
    if state.config.dev_open_loopback.unwrap_or(false) {
        return Ok(());
    }
    let expected = configured_daemon_token(state).ok_or_else(|| {
        error_response(
            401,
            "daemon token is required; set daemon_token in config/agentcall.local.json or AGENTCALL_DAEMON_TOKEN",
        )
    })?;
    let supplied = request_token(request);
    if supplied.as_deref() == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(error_response(401, "invalid or missing daemon token"))
    }
}

fn configured_daemon_token(state: &AppState) -> Option<String> {
    std::env::var("AGENTCALL_DAEMON_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .config
                .daemon_token
                .clone()
                .filter(|value| !value.trim().is_empty())
        })
}

fn request_token(request: &Request) -> Option<String> {
    request
        .headers
        .get("x-agentcall-token")
        .cloned()
        .or_else(|| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::to_string)
        })
        .or_else(|| query_params(&request.path).get("token").cloned())
        .filter(|value| !value.trim().is_empty())
}

fn host_header_is_loopback(request: &Request) -> bool {
    request
        .headers
        .get("host")
        .map(|value| endpoint_host_is_loopback(value))
        .unwrap_or(true)
}

fn origin_header_is_loopback(request: &Request) -> bool {
    request
        .headers
        .get("origin")
        .map(|value| value == "null" || endpoint_host_is_loopback(value))
        .unwrap_or(true)
}

fn endpoint_host_is_loopback(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(&value);
    if without_scheme == "::1" {
        return true;
    }
    let host = without_scheme
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme);
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

pub(crate) fn dynamic_route(request: Request, state: Arc<AppState>) -> Response {
    let path = request.path.split('?').next().unwrap_or("/");
    if let Some(rest) = path.strip_prefix("/api/routes/") {
        let id = url_decode(rest);
        return match (request.method.as_str(), route_state(&state, &id)) {
            ("GET", Some(value)) => json_response(&value),
            ("GET", None) => error_response(404, "route not found"),
            _ => error_response(404, "not found"),
        };
    }
    let Some(rest) = path.strip_prefix("/api/sessions/") else {
        return error_response(404, "not found");
    };
    let mut parts = rest.split('/');
    let name = url_decode(parts.next().unwrap_or(""));
    let action = parts.collect::<Vec<_>>().join("/");
    match (request.method.as_str(), action.as_str()) {
        ("GET", "ws") => {
            let key = request.headers.get("sec-websocket-key").cloned();
            match (get_session(&state, &name), key) {
                (Some(session), Some(key)) => Response::WebSocket {
                    state,
                    session,
                    key,
                },
                (None, _) => error_response(404, "session not found"),
                (_, None) => error_response(400, "missing websocket key"),
            }
        }
        ("GET", "stream") => match get_session(&state, &name) {
            Some(session) => Response::Sse { session },
            None => error_response(404, "session not found"),
        },
        ("GET", "summary") => match get_session(&state, &name) {
            Some(session) => json_response(&session_summary(&state, &session)),
            None => error_response(404, "session not found"),
        },
        ("GET", "output/clean") => match get_session(&state, &name) {
            Some(session) => json_response(&serde_json::json!({
                "session": name,
                "clean_tail": deprecated_clean_tail_value(&session, 120),
                "terminal": terminal_snapshot_value(&session, 120),
            })),
            None => error_response(404, "session not found"),
        },
        ("POST", "input") => match parse_json::<serde_json::Value>(&request.body)
            .and_then(|args| submit_http_session_command(&state, &name, "send", args))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "resize") => match parse_json::<ResizeRequest>(&request.body)
            .and_then(|req| resize_session(&state, &name, req))
        {
            Ok(()) => json_response(&serde_json::json!({"ok": true})),
            Err(err) => error_response(400, &err),
        },
        ("POST", "stop") => match parse_json_or_empty(&request.body)
            .and_then(|args| submit_http_session_command(&state, &name, "stop", args))
        {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        ("POST", "checkpoint") => match checkpoint_session(&state, &name) {
            Ok(result) => json_response(&result),
            Err(err) => error_response(400, &err),
        },
        _ => error_response(404, "not found"),
    }
}

pub(crate) fn write_sse(mut stream: TcpStream, session: Arc<Session>) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    )?;
    let replay = { session.clean_replay.lock().unwrap().clone() };
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

pub(crate) fn write_event(stream: &mut TcpStream, event: &StreamEvent) -> std::io::Result<()> {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    stream.write_all(format!("data: {data}\n\n").as_bytes())?;
    stream.flush()
}

pub(crate) fn write_ws(
    mut stream: TcpStream,
    state: Arc<AppState>,
    session: Arc<Session>,
    key: String,
) -> std::io::Result<()> {
    let accept = websocket_accept_key(&key);
    stream.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;

    let replay = { session.clean_replay.lock().unwrap().clone() };
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

    loop {
        let frame = match read_ws_frame(&mut stream) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                append_agent_event(
                    &state,
                    "ws.frame_too_large",
                    "WebSocket frame exceeded AgentCall limit.",
                    serde_json::json!({"name": session.name, "limit_bytes": MAX_WS_FRAME_BYTES}),
                );
                break;
            }
            Err(err) => return Err(err),
        };
        match frame.opcode {
            0x1 => {
                if let Ok(text) = String::from_utf8(frame.payload) {
                    handle_ws_message(&state, &session, &text);
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

pub(crate) fn handle_ws_message(state: &AppState, session: &Arc<Session>, text: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    match value.get("type").and_then(|v| v.as_str()) {
        Some("input") => {
            if let Some(data) = value.get("data").and_then(|v| v.as_str()) {
                let command = websocket_input_command(state, session, data);
                if let Err(err) = submit_session_command(state, &session.name, command) {
                    append_agent_event(
                        state,
                        "pty.input_rejected",
                        "WebSocket input was rejected by actor command path.",
                        serde_json::json!({"name": session.name, "chars": data.len(), "transport": "websocket", "error": err}),
                    );
                }
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
            append_agent_event(
                state,
                "pty.resized",
                "PTY session resized.",
                serde_json::json!({"name": session.name, "cols": cols, "rows": rows, "transport": "websocket"}),
            );
        }
        _ => {}
    }
}

fn websocket_input_command(
    state: &AppState,
    session: &Arc<Session>,
    data: &str,
) -> CommandEnvelopeV1 {
    let seq = state.next_seq();
    CommandEnvelopeV1 {
        schema_version: 1,
        command_id: format!("cmd-ws-{}-{seq}", session.name),
        session_id: session.name.clone(),
        run_id: None,
        owner_id: "websocket".to_string(),
        owner_lease_id: format!("websocket-{}", session.name),
        lease_generation: 0,
        idempotency_key: format!("ws-{}-{seq}", session.name),
        control_epoch: None,
        control_token_hash: None,
        command_type: CommandType::SendInput,
        payload: serde_json::json!({
            "text": data,
            "enter": false,
            "transport": "websocket",
        }),
        precondition: None,
        created_at: chrono::Utc::now().to_rfc3339(),
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
    if len > MAX_WS_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "websocket frame too large",
        ));
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

pub(crate) fn write_ws_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    write_ws_frame(stream, 0x1, text.as_bytes())
}

pub(crate) fn write_ws_frame(
    stream: &mut TcpStream,
    opcode: u8,
    payload: &[u8],
) -> std::io::Result<()> {
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

pub(crate) fn websocket_accept_key(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

pub(crate) fn sha1(input: &[u8]) -> [u8; 20] {
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

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
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

pub(crate) fn static_file(path: &str) -> Response {
    let body = fs::read(path).unwrap_or_else(|_| b"not found".to_vec());
    let status = if body == b"not found".to_vec() {
        404
    } else {
        200
    };
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

pub(crate) fn json_response<T: Serialize>(value: &T) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::Fixed {
        status: 200,
        content_type: "application/json; charset=utf-8",
        body,
    }
}

pub(crate) fn error_response(status: u16, message: &str) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap();
    Response::Fixed {
        status,
        content_type: "application/json; charset=utf-8",
        body,
    }
}

pub(crate) fn write_fixed(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        413 => "Payload Too Large",
        404 => "Not Found",
        _ => "Error",
    };
    stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Write);
    Ok(())
}

pub(crate) fn parse_json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|err| err.to_string())
}

fn parse_json_or_empty(body: &[u8]) -> Result<serde_json::Value, String> {
    if body.is_empty() {
        Ok(serde_json::json!({}))
    } else {
        parse_json::<serde_json::Value>(body)
    }
}

fn submit_http_session_command(
    state: &AppState,
    name: &str,
    action: &str,
    mut args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if !args.is_object() {
        return Err("session command body must be a JSON object".to_string());
    }
    args["action"] = serde_json::json!(action);
    if action == "send" && !args.get("text").is_some_and(serde_json::Value::is_string) {
        return Err("input command requires text".to_string());
    }
    match prepare_session_send_command(state, name, action, &args)? {
        PreparedCommand::Submit(command) => submit_session_command(state, name, command),
        PreparedCommand::Deduped(value) => Ok(value),
    }
}

pub(crate) fn url_decode(value: &str) -> String {
    value
        .replace("%20", " ")
        .replace("%2F", "/")
        .replace("%5C", "\\")
        .replace("%3A", ":")
}

pub(crate) fn query_params(path: &str) -> HashMap<String, String> {
    let Some((_, query)) = path.split_once('?') else {
        return HashMap::new();
    };
    query
        .split('&')
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            if key.is_empty() {
                None
            } else {
                Some((url_decode(key), url_decode(value)))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;

    fn test_state(name: &str, config: LocalConfig) -> Arc<AppState> {
        let root =
            std::env::temp_dir().join(format!("agentcall-http-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Arc::new(AppState::new(root, config, None))
    }

    fn request(method: &str, path: &str) -> Request {
        let mut headers = HashMap::new();
        headers.insert("host".to_string(), "127.0.0.1:3293".to_string());
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body: vec![],
        }
    }

    fn fixed_status(response: Response) -> u16 {
        match response {
            Response::Fixed { status, .. } => status,
            _ => panic!("expected fixed response"),
        }
    }

    #[test]
    fn api_requires_token_when_loopback_dev_mode_is_disabled() {
        let state = test_state(
            "requires-token",
            LocalConfig {
                daemon_token: Some("secret".to_string()),
                dev_open_loopback: Some(false),
                ..LocalConfig::default()
            },
        );
        let response = route(request("GET", "/api/runtime/health"), Arc::clone(&state));
        assert_eq!(fixed_status(response), 401);

        let mut authed = request("GET", "/api/runtime/health");
        authed
            .headers
            .insert("x-agentcall-token".to_string(), "secret".to_string());
        let response = route(authed, Arc::clone(&state));
        assert_eq!(fixed_status(response), 200);
        let _ = std::fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn api_rejects_when_token_is_not_configured() {
        let state = test_state(
            "missing-token",
            LocalConfig {
                dev_open_loopback: Some(false),
                ..LocalConfig::default()
            },
        );
        let response = route(request("GET", "/api/runtime/health"), Arc::clone(&state));
        assert_eq!(fixed_status(response), 401);
        let _ = std::fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn dev_open_loopback_allows_local_api_without_token() {
        let state = test_state(
            "dev-open",
            LocalConfig {
                dev_open_loopback: Some(true),
                ..LocalConfig::default()
            },
        );
        let response = route(request("GET", "/api/runtime/health"), Arc::clone(&state));
        assert_eq!(fixed_status(response), 200);
        let _ = std::fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn api_rejects_non_loopback_origin_even_with_token() {
        let state = test_state(
            "bad-origin",
            LocalConfig {
                daemon_token: Some("secret".to_string()),
                ..LocalConfig::default()
            },
        );
        let mut req = request("GET", "/api/runtime/health");
        req.headers
            .insert("x-agentcall-token".to_string(), "secret".to_string());
        req.headers
            .insert("origin".to_string(), "https://example.com".to_string());
        let response = route(req, Arc::clone(&state));
        assert_eq!(fixed_status(response), 403);
        let _ = std::fs::remove_dir_all(&state.workspace);
    }
}
