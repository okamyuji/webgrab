//! 文字コード判定とデコード（設計§4 decode、3段判定）。
//! HTTPヘッダのcharset → HTML先頭のmeta → chardetng推定 の順。

use encoding_rs::Encoding;

/// 生バイトとContent-Typeヘッダ（あれば）からUTF-8文字列へデコードする。
/// 戻り値は (デコード文字列, 実際に使ったエンコーディング名, 置換文字が挿入されたか)。
/// 第3要素が真のとき、判定エンコーディングと実バイトが不一致で文字化けが起きた可能性がある
/// （設計§7: 呼び出し側でstderr警告を出す）。
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, &'static str, bool) {
    let enc = detect(bytes, content_type);
    // 第2戻り値はBOM等を考慮して実際に使われたエンコーディング。
    // 事前判定した enc.name() ではなくこちらを返し、ラベルと実態を一致させる。
    let (cow, used, had_errors) = enc.decode(bytes);
    (cow.into_owned(), used.name(), had_errors)
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
    // ざっくりASCII化して <meta> タグ内の charset を探す。
    // <link>/<a> 等のクエリ文字列中の "charset=" を誤採用しないよう、
    // 必ず <meta ...> タグの内側に限定して走査する。
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let mut rest = text.as_str();
    while let Some(mpos) = rest.find("<meta") {
        let after = &rest[mpos..];
        let tag_end = after.find('>').unwrap_or(after.len());
        let tag = &after[..tag_end];
        if let Some(ci) = tag.find("charset=") {
            let val: String = tag[ci + "charset=".len()..]
                .trim_start_matches(['"', '\''])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !val.is_empty()
                && let Some(enc) = Encoding::for_label(val.as_bytes())
            {
                return Some(enc);
            }
        }
        rest = &after[tag_end.min(after.len())..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_via_header() {
        let (s, enc, _) = decode("こんにちは".as_bytes(), Some("text/html; charset=utf-8"));
        assert_eq!(s, "こんにちは");
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn shift_jis_via_header() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語テスト");
        let (s, enc, _) = decode(&bytes, Some("text/html; charset=shift_jis"));
        assert_eq!(s, "日本語テスト");
        assert_eq!(enc, "Shift_JIS");
    }

    #[test]
    fn euc_jp_via_meta() {
        let (body, _, _) = encoding_rs::EUC_JP.encode("東京都");
        let mut page = b"<html><head><meta charset=\"euc-jp\"></head><body>".to_vec();
        page.extend_from_slice(&body);
        let (s, enc, _) = decode(&page, None);
        assert!(s.contains("東京都"));
        assert_eq!(enc, "EUC-JP");
    }

    #[test]
    fn no_charset_falls_back_to_detector() {
        // charset無し・Shift_JISバイト列 → chardetngが推定
        let (bytes, _, _) =
            encoding_rs::SHIFT_JIS.encode("これは日本語のテスト文章です。日本語日本語。");
        let (s, _enc, _) = decode(&bytes, None);
        assert!(s.contains("日本語"));
    }

    #[test]
    fn header_charset_priority_over_meta() {
        // ヘッダUTF-8が優先され、metaのeuc-jpは無視される
        let page = "<meta charset=\"euc-jp\">日本".as_bytes();
        let (_s, enc, _) = decode(page, Some("text/html; charset=utf-8"));
        assert_eq!(enc, "UTF-8");
    }

    #[test]
    fn charset_in_non_meta_tag_is_ignored() {
        // <meta>外の "charset=" (canonicalリンク等)を誤採用しないこと（データ整合性）。
        let mut page =
            b"<html><head><link rel=\"canonical\" href=\"https://e.test/?charset=iso-8859-1\">"
                .to_vec();
        page.extend_from_slice(b"<meta charset=\"utf-8\">");
        page.extend_from_slice(b"</head><body>");
        page.extend_from_slice("日本語のテスト記事本文です。".as_bytes());
        page.extend_from_slice(b"</body></html>");
        let (s, enc, _) = decode(&page, None);
        assert_eq!(enc, "UTF-8", "meta外のcharset=を誤検出");
        assert!(s.contains("日本語"), "本文が文字化けした: {s}");
    }

    #[test]
    fn invalid_bytes_report_had_errors() {
        // UTF-8宣言だが不正バイト列 → 置換文字が挿入され had_errors=true（設計§7）。
        let (_s, _enc, had_errors) = decode(b"valid\xff\xfetext", Some("charset=utf-8"));
        assert!(had_errors, "不正バイトでhad_errorsが立たない");
    }

    #[test]
    fn clean_utf8_has_no_errors() {
        let (_s, _enc, had_errors) = decode("正常なテキスト".as_bytes(), Some("charset=utf-8"));
        assert!(!had_errors);
    }

    #[test]
    fn bom_overrides_declared_encoding_and_label_matches() {
        // UTF-8 BOM付きだがヘッダはshift_jis。BOMが優先され、返すラベルも実態(UTF-8)に一致すべき。
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("日本語のテスト".as_bytes());
        let (s, enc, _) = decode(&bytes, Some("text/html; charset=shift_jis"));
        assert!(s.contains("日本語のテスト"));
        assert_eq!(enc, "UTF-8", "BOM上書き後の実エンコーディングと不一致");
    }
}
