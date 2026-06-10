use crate::config::Config;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) fn daemon_get(config: &Config, path: &str) -> Result<Value, String> {
    daemon_request(config, "GET", path, None)
}

pub(crate) fn daemon_post_json(config: &Config, path: &str, body: Value) -> Result<Value, String> {
    daemon_request(config, "POST", path, Some(body))
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
) -> Result<Value, String> {
    let (host, port) = parse_daemon_url(&config.daemon_url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|err| format!("failed to resolve daemon {}: {err}", config.daemon_url))?
        .next()
        .ok_or_else(|| format!("failed to resolve daemon {}", config.daemon_url))?;
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|err| format!("failed to connect daemon {}: {err}", config.daemon_url))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|err| format!("failed to set daemon read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
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
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n{token_header}Connection: close\r\n\r\n")
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
    if !head.starts_with("HTTP/1.1 200") {
        return Err(format!(
            "daemon returned non-200 response: {}",
            head.lines().next().unwrap_or(&head)
        ));
    }
    let content_length = content_length_from_head(&head)?;
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|err| format_io_error("read daemon response body", err))?;
    serde_json::from_slice(&body).map_err(|err| format!("invalid daemon JSON: {err}"))
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
}
