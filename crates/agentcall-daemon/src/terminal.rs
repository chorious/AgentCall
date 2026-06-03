use serde::Serialize;

const CLEAN_LIMIT: usize = 512 * 1024;

#[derive(Clone, Default, Serialize)]
pub(crate) struct DecodeHealth {
    pub(crate) pending_bytes: usize,
    pub(crate) invalid_sequence_count: u64,
    pub(crate) replacement_count: u64,
    pub(crate) last_decode_error: Option<String>,
}

pub(crate) fn decode_utf8_stream(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    health: &mut DecodeHealth,
) -> String {
    pending.extend_from_slice(bytes);
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.push_str(text);
                pending.clear();
                health.pending_bytes = 0;
                health.last_decode_error = None;
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    let valid = pending.drain(..valid_up_to).collect::<Vec<_>>();
                    output.push_str(std::str::from_utf8(&valid).unwrap_or(""));
                    continue;
                }
                if let Some(error_len) = err.error_len() {
                    let drain_len = error_len.max(1).min(pending.len());
                    pending.drain(..drain_len);
                    output.push('\u{fffd}');
                    health.invalid_sequence_count += 1;
                    health.replacement_count += 1;
                    health.last_decode_error = Some(err.to_string());
                    continue;
                }
                health.pending_bytes = pending.len();
                health.last_decode_error =
                    Some("incomplete utf-8 sequence at chunk boundary".to_string());
                break;
            }
        }
    }
    health.pending_bytes = pending.len();
    output
}

pub(crate) fn append_limited_text(target: &mut String, text: &str) {
    target.push_str(text);
    if target.len() > CLEAN_LIMIT {
        let drop = target.len() - CLEAN_LIMIT;
        let keep_from = target
            .char_indices()
            .find(|(index, _)| *index >= drop)
            .map(|(index, _)| index)
            .unwrap_or(drop);
        target.drain(..keep_from);
    }
}

pub(crate) fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            push_newline(&mut output);
            continue;
        }
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                let mut final_char = None;
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        final_char = Some(next);
                        break;
                    }
                }
                match final_char {
                    Some('H' | 'f' | 'E' | 'F' | 'A' | 'B' | 'J' | 'K') => {
                        push_newline(&mut output)
                    }
                    Some('C' | 'G') => push_space(&mut output),
                    _ => {}
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.peek().copied() == Some('\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    output
}

fn push_space(output: &mut String) {
    if !output.chars().last().is_some_and(|ch| ch.is_whitespace()) {
        output.push(' ');
    }
}

fn push_newline(output: &mut String) {
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

pub(crate) fn clean_terminal_text(text: &str) -> String {
    strip_ansi(text)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn tail_lines(text: &str, lines: usize) -> String {
    let items = text.lines().collect::<Vec<_>>();
    let start = items.len().saturating_sub(lines);
    items[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_decoder_preserves_split_utf8() {
        let text = "中文🙂";
        let bytes = text.as_bytes();
        let mut pending = Vec::new();
        let mut health = DecodeHealth::default();
        let first = decode_utf8_stream(&mut pending, &bytes[..2], &mut health);
        let second = decode_utf8_stream(&mut pending, &bytes[2..5], &mut health);
        let third = decode_utf8_stream(&mut pending, &bytes[5..], &mut health);
        assert_eq!(format!("{first}{second}{third}"), text);
        assert_eq!(health.replacement_count, 0);
        assert_eq!(health.pending_bytes, 0);
    }

    #[test]
    fn clean_text_strips_ansi() {
        assert_eq!(clean_terminal_text("\x1b[31mhello\x1b[m\n"), "hello");
    }

    #[test]
    fn clean_text_preserves_cursor_position_separators() {
        assert_eq!(clean_terminal_text("left\x1b[2;10Hright"), "left\nright");
    }
}
