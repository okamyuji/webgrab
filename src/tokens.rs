//! トークン概算（設計§3 決定表、tiktoken-rs o200k_base）。

use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

fn bpe() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    // o200k_baseのBPEランク表はcrateにバンドルされた静的データ。
    // ロード失敗はビルド構成の破損時のみで、通常運用では発生しない。
    BPE.get_or_init(|| tiktoken_rs::o200k_base().expect("bundled o200k_base must load"))
}

/// テキストの概算トークン数（o200k_base）。
pub fn count(text: &str) -> usize {
    bpe().encode_with_special_tokens(text).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_tokens_reasonable() {
        // "hello world" は数トークン程度
        let n = count("hello world");
        assert!((1..=5).contains(&n), "got {n}");
    }

    #[test]
    fn japanese_costs_more_than_char_div_4() {
        // 日本語は文字数/4近似より多いことを確認（設計の採用理由の裏付け）
        let text = "これは日本語のトークン数を確認するための文章です。";
        let n = count(text);
        let approx = text.chars().count() / 4;
        assert!(n > approx, "tokens {n} should exceed char/4 {approx}");
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(count(""), 0);
    }
}
