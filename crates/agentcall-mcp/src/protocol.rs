use crate::config::Config;
use crate::tools::{call_tool, list_tools};
use serde_json::{Value, json};
use std::fs::{OpenOptions, create_dir_all};
use std::io::{self, BufRead, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentcall-mcp";
const SERVER_VERSION: &str = "3.0.0";
pub(crate) const MAX_MCP_INPUT_LINE_BYTES: usize = 1024 * 1024;
const TOOL_TEXT_CAP_BYTES: usize = 128 * 1024;
const TOOL_TEXT_PREVIEW_BYTES: usize = 16 * 1024;

pub(crate) fn serve(config: Config) -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout();
    while let Some(line_result) = read_bounded_line(&mut reader)? {
        let line = match line_result {
            Ok(line) => line,
            Err(message) => {
                let response = error_response(json!(null), -32600, &message);
                writeln!(stdout, "{}", serde_json::to_string(&response).unwrap())?;
                stdout.flush()?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let line = line.trim_start_matches('\u{feff}');
        let response = match serde_json::from_str::<Value>(line) {
            Ok(request) => handle_message(&config, request),
            Err(err) => Some(error_response(
                json!(null),
                -32700,
                &format!("Parse error: {err}"),
            )),
        };
        if let Some(response) = response {
            writeln!(stdout, "{}", serde_json::to_string(&response).unwrap())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(reader: &mut R) -> io::Result<Option<Result<String, String>>> {
    let mut buf = Vec::new();
    loop {
        let (take_len, found_newline, bytes) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let found_newline = available.iter().position(|byte| *byte == b'\n');
            let take_len = found_newline.map(|pos| pos + 1).unwrap_or(available.len());
            let bytes = available[..take_len].to_vec();
            (take_len, found_newline.is_some(), bytes)
        };
        if buf.len().saturating_add(take_len) > MAX_MCP_INPUT_LINE_BYTES {
            reader.consume(take_len);
            if !found_newline {
                drain_line(reader)?;
            }
            return Ok(Some(Err(format!(
                "MCP input line exceeded {} bytes; request rejected before JSON parsing and transport remains open",
                MAX_MCP_INPUT_LINE_BYTES
            ))));
        }
        buf.extend_from_slice(&bytes);
        reader.consume(take_len);
        if found_newline {
            break;
        }
    }
    if matches!(buf.last(), Some(b'\n')) {
        buf.pop();
        if matches!(buf.last(), Some(b'\r')) {
            buf.pop();
        }
    }
    let line = String::from_utf8(buf)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned());
    Ok(Some(Ok(line)))
}

fn drain_line<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let (consume_len, found_newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
                (pos + 1, true)
            } else {
                (available.len(), false)
            }
        };
        reader.consume(consume_len);
        if found_newline {
            return Ok(());
        }
    }
}

fn handle_message(config: &Config, request: Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(json!(null));
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => Some(success_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}
            }),
        )),
        "notifications/initialized" => None,
        "ping" => Some(success_response(id, json!({}))),
        "tools/list" => Some(success_response(id, json!({ "tools": list_tools(config) }))),
        "tools/call" => Some(handle_tool_call(
            config,
            id,
            request.get("params").cloned().unwrap_or(json!({})),
        )),
        _ => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

fn handle_tool_call(config: &Config, id: Value, params: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let call_started = Instant::now();
    let result = call_tool(config, name, args);
    let call_ms = call_started.elapsed().as_millis();
    let render_started = Instant::now();
    let (tool_result, is_error, original_bytes, truncated) = match result {
        Ok(value) => {
            let rendered = tool_text(value);
            (
                rendered.result,
                false,
                rendered.original_bytes,
                rendered.truncated,
            )
        }
        Err(err) => (tool_error(&err), true, err.len(), false),
    };
    let render_ms = render_started.elapsed().as_millis();
    append_timing_log(
        config,
        json!({
            "ts_unix_ms": now_unix_ms(),
            "tool": name,
            "call_ms": call_ms,
            "render_ms": render_ms,
            "original_bytes": original_bytes,
            "truncated": truncated,
            "is_error": is_error
        }),
    );
    success_response(id, tool_result)
}

struct RenderedToolText {
    result: Value,
    original_bytes: usize,
    truncated: bool,
}

fn tool_text(value: Value) -> RenderedToolText {
    let text = serde_json::to_string(&value).unwrap();
    let original_bytes = text.len();
    let (text, truncated) = if original_bytes > TOOL_TEXT_CAP_BYTES {
        (
            serde_json::to_string(&json!({
                "truncated": true,
                "original_bytes": original_bytes,
                "cap_bytes": TOOL_TEXT_CAP_BYTES,
                "preview_bytes": TOOL_TEXT_PREVIEW_BYTES,
                "preview": truncate_utf8(&text, TOOL_TEXT_PREVIEW_BYTES),
                "hint": "Tool response exceeded MCP compact response cap; use a narrower board/session query or debug log if full data is required."
            }))
            .unwrap(),
            true,
        )
    } else {
        (text, false)
    };
    RenderedToolText {
        result: json!({
            "content": [{"type": "text", "text": text}],
            "isError": false
        }),
        original_bytes,
        truncated,
    }
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn truncate_utf8(text: &str, cap_bytes: usize) -> &str {
    if text.len() <= cap_bytes {
        return text;
    }
    let mut end = cap_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn append_timing_log(config: &Config, entry: Value) {
    let dir = config.workspace.join(".agentcall").join("logs").join("mcp");
    if let Err(err) = create_dir_all(&dir) {
        eprintln!("agentcall-mcp: failed to create timing log dir: {err}");
        return;
    }
    let path = dir.join("recent.ndjson");
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Ok(line) = serde_json::to_string(&entry) {
                if let Err(err) = writeln!(file, "{line}") {
                    eprintln!("agentcall-mcp: failed to write timing log: {err}");
                }
            }
        }
        Err(err) => eprintln!(
            "agentcall-mcp: failed to open timing log {}: {err}",
            path.display()
        ),
    }
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_text_is_compact_json_by_default() {
        let rendered = tool_text(json!({"a": 1, "b": ["x", "y"]}));
        let text = rendered.result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, r#"{"a":1,"b":["x","y"]}"#);
        assert!(!rendered.truncated);
    }

    #[test]
    fn tool_text_caps_large_payloads_with_valid_json_preview() {
        let rendered = tool_text(json!({"data": "x".repeat(TOOL_TEXT_CAP_BYTES + 1024)}));
        let text = rendered.result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["truncated"], true);
        assert!(parsed["original_bytes"].as_u64().unwrap() > TOOL_TEXT_CAP_BYTES as u64);
        assert!(parsed["preview"].as_str().unwrap().len() <= TOOL_TEXT_PREVIEW_BYTES);
    }

    #[test]
    fn bounded_reader_rejects_oversized_line_and_keeps_next_request() {
        let mut input = Vec::new();
        input.extend_from_slice(&vec![b'x'; MAX_MCP_INPUT_LINE_BYTES + 1]);
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let mut cursor = io::Cursor::new(input);

        let first = read_bounded_line(&mut cursor).unwrap().unwrap();
        assert!(first.unwrap_err().contains("exceeded"));

        let second = read_bounded_line(&mut cursor).unwrap().unwrap().unwrap();
        assert!(second.contains("\"method\":\"ping\""));
    }
}
