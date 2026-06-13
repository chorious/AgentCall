use crate::config::Config;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) fn daemon_get(config: &Config, path: &str) -> Result<Value, String> {
    daemon_request(
        config,
        "GET",
        path,
        None,
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        WRITE_TIMEOUT,
    )
}

pub(crate) fn daemon_get_with_timeout(
    config: &Config,
    path: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let timeout = timeout.max(Duration::from_millis(1));
    daemon_request(config, "GET", path, None, timeout, timeout, timeout)
}

pub(crate) fn daemon_post_json(config: &Config, path: &str, body: Value) -> Result<Value, String> {
    daemon_request(
        config,
        "POST",
        path,
        Some(body),
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        WRITE_TIMEOUT,
    )
}

pub(crate) fn parse_daemon_url(url: &str) -> Result<(String, u16), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("daemon-url must start with http://")?;
    let host_port = rest.trim_end_matches('/');
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or("daemon-url must include a port")?;
    let port = port
        .parse::<u16>()
        .map_err(|err| format!("invalid daemon port: {err}"))?;
    Ok((host.to_string(), port))
}

fn daemon_request(
    config: &Config,
    method: &str,
    path: &str,
    body: Option<Value>,
    connect_timeout: Duration,
    read_timeout: Duration,
    write_timeout: Duration,
) -> Result<Value, String> {
    let (host, port) = parse_daemon_url(&config.daemon_url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|err| format!("failed to resolve daemon {}: {err}", config.daemon_url))?
        .next()
        .ok_or_else(|| format!("failed to resolve daemon {}", config.daemon_url))?;
    let mut stream = TcpStream::connect_timeout(&address, connect_timeout)
        .map_err(|err| format!("failed to connect daemon {}: {err}", config.daemon_url))?;
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| format!("failed to set daemon read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(write_timeout))
        .map_err(|err| format!("failed to set daemon write timeout: {err}"))?;
    let body_text = body
        .map(|value| serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_default();
    let token_header = config
        .daemon_token
        .as_ref()
        .map(|token| format!("X-AgentCall-Token: {token}\r\n"))
        .unwrap_or_default();
    let request = if method == "POST" {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\n{token_header}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_text.len(),
            body_text
        )
    } else {
        format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n{token_header}Connection: close\r\n\r\n"
        )
    };
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format_io_error("write daemon request", err))?;
    stream
        .flush()
        .map_err(|err| format_io_error("flush daemon request", err))?;
    read_http_json(stream)
}

fn read_http_json(stream: TcpStream) -> Result<Value, String> {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| format_io_error("read daemon response head", err))?;
        if read == 0 {
            return Err("daemon closed connection before response head".to_string());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
    }
    let status = http_status_from_head(&head).unwrap_or(0);
    let content_length = content_length_from_head(&head)?;
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|err| format_io_error("read daemon response body", err))?;
    if status != 200 {
        return Err(format_non_200_daemon_error(status, &head, &body));
    }
    serde_json::from_slice(&body).map_err(|err| format!("invalid daemon JSON: {err}"))
}

fn http_status_from_head(head: &str) -> Option<u16> {
    let status_line = head.lines().next()?;
    let mut parts = status_line.split_whitespace();
    let _http = parts.next()?;
    parts.next()?.parse::<u16>().ok()
}

fn format_non_200_daemon_error(status: u16, head: &str, body: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        return serde_json::to_string(&value).unwrap_or_else(|_| {
            format!(
                "daemon returned HTTP {status}: {}",
                head.lines().next().unwrap_or(head)
            )
        });
    }
    let body_text = String::from_utf8_lossy(body);
    if body_text.trim().is_empty() {
        format!(
            "daemon returned HTTP {status}: {}",
            head.lines().next().unwrap_or(head)
        )
    } else {
        format!("daemon returned HTTP {status}: {body_text}")
    }
}

fn content_length_from_head(head: &str) -> Result<usize, String> {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|err| format!("invalid daemon Content-Length: {err}"));
        }
    }
    Err("daemon response missing Content-Length".to_string())
}

fn format_io_error(action: &str, err: std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            format!("daemon_query_timeout while attempting to {action}: {err}")
        }
        _ => format!("failed to {action}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_errors_are_classified_for_mcp_callers() {
        let message = format_io_error(
            "read daemon response body",
            std::io::Error::from(std::io::ErrorKind::TimedOut),
        );
        assert!(message.contains("daemon_query_timeout"));
        assert!(message.contains("read daemon response body"));
    }

    #[test]
    fn non_200_daemon_response_preserves_structured_error_body() {
        let body = br#"{"error":{"code":"workspace_busy","category":"safety_lock","message":"busy","details":{"existing_session":"worker-a"}}}"#;
        let message = format_non_200_daemon_error(
            409,
            "HTTP/1.1 409 Conflict\r\nContent-Length: 120\r\n",
            body,
        );
        let value: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(value["error"]["code"], "workspace_busy");
        assert_eq!(value["error"]["details"]["existing_session"], "worker-a");
    }
}
