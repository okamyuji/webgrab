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
}

impl WebgrabError {
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// stderrへ機械可読書式で出力する。先頭行 `webgrab: error=<token> <message>`。
    pub fn print_stderr(&self) {
        eprintln!("webgrab: error={} {}", self.code.token(), self.message);
        if let Some(d) = &self.detail {
            eprintln!("{d}");
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
}
