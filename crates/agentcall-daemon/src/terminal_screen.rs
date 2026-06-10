use crate::terminal::tail_lines;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const SCREEN_SCROLLBACK_ROWS: usize = 2000;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TerminalSnapshot {
    pub(crate) schema_version: u16,
    pub(crate) seq: u64,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) mode: String,
    pub(crate) cursor: TerminalCursor,
    pub(crate) text: String,
    pub(crate) tail: Vec<String>,
    pub(crate) quality: TerminalSnapshotQuality,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TerminalCursor {
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) hidden: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TerminalSnapshotQuality {
    pub(crate) source: String,
    pub(crate) volatile: bool,
    pub(crate) frames_seen: u64,
    pub(crate) volatile_frames: u64,
    pub(crate) meaningful_frames: u64,
    pub(crate) lines_returned: usize,
    pub(crate) lines_suppressed: usize,
    pub(crate) screen_unreliable: bool,
    pub(crate) confidence: String,
}

pub(crate) trait TerminalEmulator {
    fn process(&mut self, bytes: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);
    fn snapshot(&self, max_lines: usize) -> TerminalSnapshot;
}

pub(crate) struct TerminalScreen {
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
    seq: u64,
    last_snapshot_hash: u64,
    last_semantic_hash: u64,
    frames_seen: u64,
    volatile_frames: u64,
    meaningful_frames: u64,
    screen_unreliable: bool,
}

impl TerminalScreen {
    pub(crate) fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, SCREEN_SCROLLBACK_ROWS),
            rows,
            cols,
            seq: 0,
            last_snapshot_hash: 0,
            last_semantic_hash: 0,
            frames_seen: 0,
            volatile_frames: 0,
            meaningful_frames: 0,
            screen_unreliable: false,
        }
    }

    fn snapshot_text(&self) -> String {
        self.parser.screen().contents()
    }
}

impl TerminalEmulator for TerminalScreen {
    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.seq = self.seq.saturating_add(1);
        self.frames_seen = self.frames_seen.saturating_add(1);

        let text = self.snapshot_text();
        let snapshot_hash = stable_hash(&text);
        if snapshot_hash == self.last_snapshot_hash {
            return;
        }

        if looks_like_volatile_screen(&text) {
            self.volatile_frames = self.volatile_frames.saturating_add(1);
        } else {
            self.meaningful_frames = self.meaningful_frames.saturating_add(1);
            self.last_semantic_hash = snapshot_hash;
        }
        self.last_snapshot_hash = snapshot_hash;
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn snapshot(&self, max_lines: usize) -> TerminalSnapshot {
        let screen = self.parser.screen();
        let text = self.snapshot_text();
        let (tail, suppressed) = screen_snapshot_tail(&text, max_lines);
        let (row, col) = screen.cursor_position();
        let volatile = self.frames_seen > 0 && self.volatile_frames >= self.meaningful_frames;
        TerminalSnapshot {
            schema_version: 1,
            seq: self.seq,
            rows: self.rows,
            cols: self.cols,
            mode: if screen.alternate_screen() {
                "alternate"
            } else {
                "normal"
            }
            .to_string(),
            cursor: TerminalCursor {
                row,
                col,
                hidden: screen.hide_cursor(),
            },
            text,
            quality: TerminalSnapshotQuality {
                source: "vt100".to_string(),
                volatile,
                frames_seen: self.frames_seen,
                volatile_frames: self.volatile_frames,
                meaningful_frames: self.meaningful_frames,
                lines_returned: tail.len(),
                lines_suppressed: suppressed,
                screen_unreliable: self.screen_unreliable,
                confidence: if self.screen_unreliable {
                    "low"
                } else if volatile {
                    "medium"
                } else {
                    "high"
                }
                .to_string(),
            },
            tail,
        }
    }
}

pub(crate) fn screen_snapshot_tail(text: &str, max_lines: usize) -> (Vec<String>, usize) {
    let mut suppressed = 0usize;
    let mut lines = Vec::new();
    let mut previous = String::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            suppressed += 1;
            continue;
        }
        let normalized = normalize_line(line);
        if !should_keep_snapshot_line(line) {
            suppressed += 1;
            continue;
        }
        if normalized == previous {
            suppressed += 1;
            continue;
        }
        previous = normalized;
        lines.push(line.to_string());
    }
    let joined = tail_lines(&lines.join("\n"), max_lines);
    let tail = joined
        .lines()
        .map(str::to_string)
        .collect::<Vec<String>>();
    let truncated = lines.len().saturating_sub(tail.len());
    (tail, suppressed.saturating_add(truncated))
}

fn stable_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn looks_like_volatile_screen(text: &str) -> bool {
    let semantic = text.lines().filter(|line| should_keep_snapshot_line(line)).count();
    let noise = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !should_keep_snapshot_line(line))
        .count();
    noise > semantic && semantic <= 2
}

fn should_keep_snapshot_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if has_keep_marker(trimmed) {
        return true;
    }
    let alnum_count = trimmed.chars().filter(|ch| ch.is_alphanumeric()).count();
    let glyph_count = trimmed
        .chars()
        .filter(|ch| !ch.is_alphanumeric() && !ch.is_whitespace())
        .count();
    if alnum_count == 0 {
        return false;
    }
    if glyph_count > alnum_count.saturating_mul(2) && !looks_like_menu_option(trimmed) {
        return false;
    }
    if looks_like_decorative_line(trimmed) {
        return false;
    }
    let long_word = trimmed
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .any(|word| word.chars().count() >= 3);
    long_word || looks_like_menu_option(trimmed)
}

fn has_keep_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.contains("AGENTCALL_SMOKE_")
        || lower.contains("permission")
        || lower.contains("allow")
        || lower.contains("deny")
        || lower.contains("denied")
        || lower.contains("blocked")
        || lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("report")
        || lower.contains("complete")
        || lower.contains("done")
        || lower.contains("write")
        || lower.contains("read")
        || lower.contains("bash")
        || line.contains("\\")
        || line.contains("/")
        || looks_like_menu_option(line)
}

fn looks_like_menu_option(line: &str) -> bool {
    let trimmed = line
        .trim_start_matches(|ch: char| !ch.is_ascii_digit())
        .trim_start();
    let mut chars = trimmed.chars();
    matches!(chars.next(), Some('0'..='9'))
        && matches!(chars.next(), Some('.' | ')' | ':' | ' '))
}

fn looks_like_decorative_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let decorations = [
        "puttering",
        "blanching",
        "ideating",
        "brewed",
        "cooked",
        "thinking",
        "for shortcuts",
        "auto mode on",
    ];
    if decorations.iter().any(|marker| lower.contains(marker)) && !has_keep_marker(line) {
        return true;
    }
    let cleaned = lower
        .replace(['✻', '✶', '✢', '✽', '●', '⏵', '←', '·'], "")
        .trim()
        .to_string();
    cleaned.is_empty()
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_replay_drops_intermediate_spinner_frames() {
        let mut screen = TerminalScreen::new(10, 80);
        screen.process(b"Puttering...\r\x1b[KBlanching...\r\x1b[KDone\n");
        let snapshot = screen.snapshot(10);
        let tail = snapshot.tail.join("\n");
        assert!(tail.contains("Done"));
        assert!(!tail.contains("Puttering"));
        assert!(!tail.contains("Blanching"));
    }

    #[test]
    fn vt_replay_handles_carriage_return_and_clear_line() {
        let mut screen = TerminalScreen::new(10, 80);
        screen.process(b"partial stale\r\x1b[Kfresh line\n");
        let snapshot = screen.snapshot(10);
        let tail = snapshot.tail.join("\n");
        assert!(tail.contains("fresh line"));
        assert!(!tail.contains("partial stale"));
    }

    #[test]
    fn vt_replay_preserves_permission_menu_options() {
        let mut screen = TerminalScreen::new(10, 80);
        screen.process(b"Permission requested\n\xE2\x9D\xAF 1. Yes, allow\n  2. No\nEsc to cancel\n");
        let snapshot = screen.snapshot(10);
        let tail = snapshot.tail.join("\n");
        assert!(tail.contains("1. Yes, allow"));
        assert!(tail.contains("2. No"));
        assert!(tail.contains("Esc to cancel"));
    }

    #[test]
    fn vt_replay_preserves_smoke_ok_marker() {
        let mut screen = TerminalScreen::new(10, 80);
        screen.process(b"\x1b[31mAGENTCALL_SMOKE_A_OK\x1b[m\n");
        let snapshot = screen.snapshot(10);
        assert!(snapshot.tail.join("\n").contains("AGENTCALL_SMOKE_A_OK"));
    }
}
