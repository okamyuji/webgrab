//! エラー型と終了コード（設計§5 終了コード表）。

use std::fmt;

/// webgrabの終了コード。設計§5の表と1対1で対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    Internal = 1,
    Usage = 2,
    Network = 3,
    Http = 4,
    Robots = 5,
    Empty = 6,
    Render = 7,
    Netguard = 8,
}

impl ExitCode {
    /// stderr先頭行の `error=` トークン（空白を含まない）。
    pub fn token(self) -> &'static str {
        match self {
            ExitCode::Success => "ok",
            ExitCode::Internal => "internal",
            ExitCode::Usage => "usage",
            ExitCode::Network => "network",
            ExitCode::Http => "http",
            ExitCode::Robots => "robots",
            ExitCode::Empty => "empty",
            ExitCode::Render => "render",
            ExitCode::Netguard => "netguard",
        }
    }
}

/// webgrabのエラー。終了コードと人間可読メッセージを持つ。
#[derive(Debug)]
pub struct WebgrabError {
    pub code: ExitCode,
    pub message: String,
    /// 追加の診断行（stderr 2行目以降、空白を含んでよい）。
    pub detail: Option<String>,
    /// 先頭行に付加する機械可読トークン（例: `hint=--render/--raw`）。
    pub tokens: Vec<(&'static str, String)>,
}

const DETAIL_MAX_BYTES: usize = 512;

/// stderrの詳細行を1行に畳む。C0/DEL/C1制御文字は除去し、改行・タブ・行区切りは空白へ。
/// ANSI CSI制御シーケンス（ESC [ params final）は単一スペースに置換。
/// 512バイトを超えない最大の文字境界で切り詰めて `…` を付す。
pub fn sanitize_detail(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(DETAIL_MAX_BYTES + 4));
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        // Handle ANSI CSI sequences (ESC [ params final) with bounded lookahead.
        // CSI final byte is in range 0x40–0x7E. If no final byte found within bounded
        // lookahead (100 chars), drop the ESC and '[', continue with remaining text.
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            let mut saved_chars = Vec::new();
            let mut found_final = false;
            chars.next(); // skip '['
            for _ in 0..100 {
                if chars.peek().is_some() {
                    let ch_to_check = chars.next().unwrap();
                    saved_chars.push(ch_to_check);
                    // CSI final byte: 0x40–0x7E (ASCII @–~)
                    if ch_to_check.len_utf8() == 1 {
                        let b = ch_to_check as u8;
                        if (0x40..=0x7E).contains(&b) {
                            found_final = true;
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            if found_final {
                if out.len() + 1 > DETAIL_MAX_BYTES {
                    out.push('…');
                    return out;
                }
                out.push(' ');
            } else {
                // 終端バイトが無いならCSIではない。ESCだけをC0として落とし、`[`と
                // 後続テキストは本文として残す（誤って1文字消すと診断が読めなくなる）。
                if out.len() + 1 > DETAIL_MAX_BYTES {
                    out.push('…');
                    return out;
                }
                out.push('[');
                for ch in saved_chars {
                    let c = match ch {
                        '\t' | '\n' | '\r' | '\u{2028}' | '\u{2029}' => ' ',
                        '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => continue,
                        c => c,
                    };
                    if out.len() + c.len_utf8() > DETAIL_MAX_BYTES {
                        out.push('…');
                        return out;
                    }
                    out.push(c);
                }
            }
            continue;
        }

        let c = match c {
            '\t' | '\n' | '\r' | '\u{2028}' | '\u{2029}' => ' ',
            '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => continue,
            c => c,
        };
        if out.len() + c.len_utf8() > DETAIL_MAX_BYTES {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

impl WebgrabError {
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            tokens: Vec::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(sanitize_detail(&detail.into()));
        self
    }

    pub fn with_token(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.tokens.push((key, value.into()));
        self
    }

    /// stderrへ出す行の列。トークン付きなら先頭行はトークンのみで、メッセージは2行目。
    pub fn stderr_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.tokens.is_empty() {
            lines.push(format!(
                "webgrab: error={} {}",
                self.code.token(),
                self.message
            ));
        } else {
            let toks: Vec<String> = self
                .tokens
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            lines.push(format!(
                "webgrab: error={} {}",
                self.code.token(),
                toks.join(" ")
            ));
            lines.push(self.message.clone());
        }
        if let Some(d) = &self.detail {
            lines.push(d.clone());
        }
        lines
    }

    /// stderrへ機械可読書式で出力する。
    pub fn print_stderr(&self) {
        for l in self.stderr_lines() {
            eprintln!("{l}");
        }
    }
}

impl fmt::Display for WebgrabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error={} {}", self.code.token(), self.message)
    }
}

impl std::error::Error for WebgrabError {}

pub type Result<T> = std::result::Result<T, WebgrabError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_map_to_expected_numbers() {
        assert_eq!(ExitCode::Success as i32, 0);
        assert_eq!(ExitCode::Netguard as i32, 8);
        assert_eq!(ExitCode::Http as i32, 4);
    }

    #[test]
    fn tokens_have_no_whitespace() {
        for c in [
            ExitCode::Internal,
            ExitCode::Usage,
            ExitCode::Network,
            ExitCode::Http,
            ExitCode::Robots,
            ExitCode::Empty,
            ExitCode::Render,
            ExitCode::Netguard,
        ] {
            assert!(!c.token().contains(char::is_whitespace));
        }
    }

    #[test]
    fn error_carries_code_and_detail() {
        let e = WebgrabError::new(ExitCode::Robots, "blocked").with_detail("rule: /private");
        assert_eq!(e.code, ExitCode::Robots);
        assert_eq!(e.detail.as_deref(), Some("rule: /private"));
    }

    #[test]
    fn token_moves_message_to_detail_line() {
        let e = WebgrabError::new(ExitCode::Empty, "empty body extracted")
            .with_token("hint", "--render/--raw");
        let lines = e.stderr_lines();
        assert_eq!(lines[0], "webgrab: error=empty hint=--render/--raw");
        assert_eq!(lines[1], "empty body extracted");
    }

    #[test]
    fn without_token_first_line_keeps_message() {
        let e = WebgrabError::new(ExitCode::Http, "HTTP 503 retryable=true");
        let lines = e.stderr_lines();
        assert_eq!(lines[0], "webgrab: error=http HTTP 503 retryable=true");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn detail_is_sanitized_to_one_line_and_capped() {
        let raw = format!("a\nb\u{1b}[31mc\u{2028}d{}", "é".repeat(600));
        let d = sanitize_detail(&raw);
        assert!(!d.contains('\n') && !d.contains('\u{1b}') && !d.contains('\u{2028}'));
        assert!(d.starts_with("a b c d"));
        assert!(d.len() <= 512 + '…'.len_utf8(), "len={}", d.len());
        assert!(d.ends_with('…'));
        assert!(d.is_char_boundary(d.len() - '…'.len_utf8()));
    }

    #[test]
    fn exactly_512_bytes_no_ellipsis() {
        let input = "x".repeat(512);
        let d = sanitize_detail(&input);
        assert_eq!(d, input);
        assert!(!d.ends_with('…'));
    }

    #[test]
    fn multibyte_char_at_boundary_truncates_cleanly() {
        // Build 511 ASCII chars + 1 multibyte char (é = 2 bytes in UTF-8)
        // Total: 511 + 2 = 513 bytes, so truncation should happen at char boundary
        let mut input = "x".repeat(511);
        input.push('é');
        let d = sanitize_detail(&input);
        assert!(d.ends_with('…'));
        assert!(d.is_char_boundary(d.len() - '…'.len_utf8()));
        // Should be exactly 511 bytes + ellipsis
        assert_eq!(d.len(), 511 + '…'.len_utf8());
    }

    #[test]
    fn unterminated_csi_preserves_following_text() {
        // 数字のみでCSI終端バイトが現れない。ESCだけ落とし `[` 以降は本文として残す。
        let d = sanitize_detail("\u{1b}[123456789");
        assert_eq!(d, "[123456789");
    }

    #[test]
    fn terminated_csi_with_final_byte_t_collapses_to_space() {
        // "\u{1b}[0;t" は終端バイト `t`(0x74) を持つ正当なCSI。単一スペースへ畳む。
        let d = sanitize_detail("\u{1b}[0;text");
        assert_eq!(d, " ext");
    }

    #[test]
    fn esc_not_followed_by_bracket_removed() {
        // ESC followed by 'm' (not '['), so just remove ESC (C0 control char)
        // 'm' is not a control char, so it remains
        let input = "\u{1b}mtext";
        let d = sanitize_detail(input);
        assert_eq!(d, "mtext");
    }
}
