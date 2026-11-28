//! 200で返るログイン壁・bot判定画面の検出。

/// この字数以上の本文は判定しない。CAPTCHAやCloudflareを解説する記事を誤検出しないため。
pub const MAX_CHARS: usize = 2000;

/// タイトルと本文のどちらで一致しても検出する語。
const MARKERS: &[&str] = &[
    "just a moment...",
    "checking your browser",
    "attention required! | cloudflare",
    "verify you are human",
    "are you a robot",
    "complete the security check",
    "log in to continue",
    "sign in to continue",
    "you've been blocked",
    "you’ve been blocked",
];

/// 短いリリースノートやFAQの本文にも普通に出る語なので、タイトルでだけ照合する。
const TITLE_ONLY: &[&str] = &["access denied", "please log in"];

/// 一致したマーカー語を返す。`text`は抽出本文の可視テキスト（タグと記法を除き空白を畳んだもの）。
pub fn detect(title: Option<&str>, text: &str) -> Option<&'static str> {
    if text.chars().count() >= MAX_CHARS {
        return None;
    }
    let title = title.unwrap_or("").to_lowercase();
    let text = text.to_lowercase();
    let anywhere = MARKERS
        .iter()
        .copied()
        .find(|m| title.contains(m) || text.contains(m));
    anywhere.or_else(|| TITLE_ONLY.iter().copied().find(|m| title.contains(m)))
}

/// 本文末尾の印。
pub fn marker(word: &str) -> String {
    format!(
        "[webgrab:suspected-block-page marker=\"{word}\" — this may be a login wall or bot check, not the page content]"
    )
}

/// stderrの警告行。語に空白を含むので、空白区切りで読む利用側のために引用符で囲む。
pub fn warn_line(word: &str) -> String {
    format!("webgrab: warn=suspected-block-page marker=\"{word}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORD: &str = "verify you are human";

    #[test]
    fn detects_marker_in_title_case_insensitive() {
        assert_eq!(
            detect(Some("JUST A MOMENT..."), "x"),
            Some("just a moment...")
        );
    }

    #[test]
    fn detects_every_body_marker() {
        let cases = [
            ("Just a moment...", "just a moment..."),
            (
                "Checking your browser before access",
                "checking your browser",
            ),
            (
                "Attention Required! | Cloudflare",
                "attention required! | cloudflare",
            ),
            ("Please verify you are human.", "verify you are human"),
            ("Are you a robot?", "are you a robot"),
            (
                "Complete the security check to proceed.",
                "complete the security check",
            ),
            ("Please Log in to continue.", "log in to continue"),
            ("Sign in to continue reading.", "sign in to continue"),
            (
                "You've been blocked by network security.",
                "you've been blocked",
            ),
            (
                "You’ve been blocked by network security.",
                "you’ve been blocked",
            ),
        ];
        for (body, want) in cases {
            assert_eq!(detect(None, body), Some(want), "{body}");
        }
    }

    #[test]
    fn title_only_words_match_title() {
        assert_eq!(detect(Some("Access Denied"), "x"), Some("access denied"));
        assert_eq!(
            detect(Some("Please log in | Example"), "x"),
            Some("please log in")
        );
    }

    #[test]
    fn title_only_words_ignored_in_body() {
        assert_eq!(
            detect(
                Some("Release notes"),
                "Fix: access denied on wildcard principals."
            ),
            None
        );
        assert_eq!(
            detect(
                Some("FAQ"),
                "If the dashboard asks you to please log in again, clear cookies."
            ),
            None
        );
    }

    #[test]
    fn ignores_body_at_max_chars() {
        let b = format!("{WORD}{}", "a".repeat(MAX_CHARS - WORD.len()));
        assert_eq!(b.chars().count(), MAX_CHARS);
        assert_eq!(detect(None, &b), None);
    }

    #[test]
    fn checks_body_just_below_max_chars() {
        let b = format!("{WORD}{}", "a".repeat(MAX_CHARS - WORD.len() - 1));
        assert_eq!(b.chars().count(), MAX_CHARS - 1);
        assert_eq!(detect(None, &b), Some(WORD));
    }

    #[test]
    fn counts_chars_not_bytes() {
        let b = format!("{WORD}{}", "あ".repeat(1000));
        assert_eq!(detect(None, &b), Some(WORD));
    }

    #[test]
    fn returns_none_without_marker() {
        assert_eq!(detect(Some("Article"), "Normal text about logins."), None);
    }

    #[test]
    fn marker_text_is_fixed() {
        assert_eq!(
            marker("access denied"),
            "[webgrab:suspected-block-page marker=\"access denied\" — this may be a login wall or bot check, not the page content]"
        );
    }

    #[test]
    fn warn_line_quotes_word() {
        assert_eq!(
            warn_line("just a moment..."),
            "webgrab: warn=suspected-block-page marker=\"just a moment...\""
        );
    }
}
