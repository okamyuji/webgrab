//! 200で返るログイン壁・bot判定画面の検出。

/// この字数以上の本文は判定しない。CAPTCHAやCloudflareを解説する記事を誤検出しないため。
pub const MAX_CHARS: usize = 2000;

const MARKERS: &[&str] = &[
    "just a moment...",
    "checking your browser",
    "attention required! | cloudflare",
    "verify you are human",
    "are you a robot",
    "complete the security check",
    "log in to continue",
    "sign in to continue",
    "please log in",
    "access denied",
    "you've been blocked",
    "you’ve been blocked",
];

/// 一致したマーカー語を返す。
pub fn detect(title: Option<&str>, body: &str) -> Option<&'static str> {
    if body.chars().count() >= MAX_CHARS {
        return None;
    }
    let hay = format!("{}\n{}", title.unwrap_or(""), body).to_lowercase();
    MARKERS.iter().copied().find(|m| hay.contains(m))
}

/// markdown末尾の印。
pub fn marker(word: &str) -> String {
    format!(
        "[webgrab:suspected-block-page marker=\"{word}\" — this may be a login wall or bot check, not the page content]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_marker_in_title_case_insensitive() {
        assert_eq!(
            detect(Some("JUST A MOMENT..."), "x"),
            Some("just a moment...")
        );
    }

    #[test]
    fn detects_marker_in_body() {
        assert_eq!(
            detect(None, "Please Log in to continue."),
            Some("log in to continue")
        );
    }

    #[test]
    fn detects_curly_apostrophe() {
        assert_eq!(
            detect(None, "You’ve been blocked by network security."),
            Some("you’ve been blocked")
        );
    }

    #[test]
    fn ignores_body_at_max_chars() {
        let b = format!("access denied{}", "a".repeat(MAX_CHARS - 13));
        assert_eq!(b.chars().count(), MAX_CHARS);
        assert_eq!(detect(None, &b), None);
    }

    #[test]
    fn checks_body_just_below_max_chars() {
        let b = format!("access denied{}", "a".repeat(MAX_CHARS - 14));
        assert_eq!(b.chars().count(), MAX_CHARS - 1);
        assert_eq!(detect(None, &b), Some("access denied"));
    }

    #[test]
    fn counts_chars_not_bytes() {
        let b = format!("access denied{}", "あ".repeat(1000));
        assert_eq!(detect(None, &b), Some("access denied"));
    }

    #[test]
    fn returns_none_without_marker() {
        assert_eq!(detect(Some("Article"), "Normal text about logins."), None);
    }

    #[test]
    fn marker_names_the_word() {
        assert!(
            marker("access denied")
                .starts_with("[webgrab:suspected-block-page marker=\"access denied\"")
        );
    }
}
