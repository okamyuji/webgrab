//! 文字コード判定とデコード（設計§4 decode、3段判定）。
//! HTTPヘッダのcharset → HTML先頭のmeta → chardetng推定 の順。

use encoding_rs::Encoding;

/// 生バイトとContent-Typeヘッダ（あれば）からUTF-8文字列へデコードする。
/// 判定に用いたエンコーディング名も返す。
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, &'static str) {
    let enc = detect(bytes, content_type);
    let (cow, _, _) = enc.decode(bytes);
    (cow.into_owned(), enc.name())
}

fn detect(bytes: &[u8], content_type: Option<&str>) -> &'static Encoding {
    // 1. HTTPヘッダのcharset
    if let Some(enc) = content_type.and_then(charset_from_content_type) {
        return enc;
    }
    // 2. HTML先頭1024バイトのmeta
    if let Some(enc) = charset_from_meta(&bytes[..bytes.len().min(1024)]) {
        return enc;
    }
    // 3. chardetng推定
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    det.feed(bytes, true);
    det.guess(None, chardetng::Utf8Detection::Allow)
}

fn charset_from_content_type(ct: &str) -> Option<&'static Encoding> {
    let lower = ct.to_ascii_lowercase();
    let idx = lower.find("charset=")?;
    let raw = lower[idx + "charset=".len()..]
        .split(&[';', ' '][..])
        .next()?
        .trim()
        .trim_matches('"');
    Encoding::for_label(raw.as_bytes())
}

fn charset_from_meta(head: &[u8]) -> Option<&'static Encoding> {
    // ざっくりASCII化して meta charset を探す。
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    // <meta charset="...">
    if let Some(i) = text.find("charset=") {
        let rest = &text[i + "charset=".len()..];
        let val: String = rest
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !val.is_empty() {
            return Encoding::for_label(val.as_bytes());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_via_header() {
        let (s, enc) = decode("こんにちは".as_bytes(), Some("text/html; charset=utf-8"));
        assert_eq!(s, "こんにちは");
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn shift_jis_via_header() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語テスト");
        let (s, enc) = decode(&bytes, Some("text/html; charset=shift_jis"));
        assert_eq!(s, "日本語テスト");
        assert_eq!(enc, "Shift_JIS");
    }

    #[test]
    fn euc_jp_via_meta() {
        let (body, _, _) = encoding_rs::EUC_JP.encode("東京都");
        let mut page = b"<html><head><meta charset=\"euc-jp\"></head><body>".to_vec();
        page.extend_from_slice(&body);
        let (s, enc) = decode(&page, None);
        assert!(s.contains("東京都"));
        assert_eq!(enc, "EUC-JP");
    }

    #[test]
    fn no_charset_falls_back_to_detector() {
        // charset無し・Shift_JISバイト列 → chardetngが推定
        let (bytes, _, _) =
            encoding_rs::SHIFT_JIS.encode("これは日本語のテスト文章です。日本語日本語。");
        let (s, _enc) = decode(&bytes, None);
        assert!(s.contains("日本語"));
    }

    #[test]
    fn header_charset_priority_over_meta() {
        // ヘッダUTF-8が優先され、metaのeuc-jpは無視される
        let page = "<meta charset=\"euc-jp\">日本".as_bytes();
        let (_s, enc) = decode(page, Some("text/html; charset=utf-8"));
        assert_eq!(enc, "UTF-8");
    }
}
