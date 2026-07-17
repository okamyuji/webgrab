//! 文字量制御（設計§3の文字定義、§5 --start-index/--max-chars、§6 フッタ）。
//!
//! 「文字」はUnicodeスカラー値（char）。スライスはchar単位で行い、
//! バイト境界パニックを起こさない。範囲は半開区間 [start, start+max)。

/// スライス結果と、続き取得に必要なメタ情報。
#[derive(Debug, PartialEq, Eq)]
pub struct Slice {
    /// 出力する本文スライス。
    pub content: String,
    /// スライスの開始char（= start_index、ただし総数でクランプ）。
    pub start: usize,
    /// スライスの終端char（半開）。
    pub end: usize,
    /// 本文全体のchar数。
    pub total: usize,
    /// 末尾に未取得部分が残る（切り詰め発生）。
    pub truncated: bool,
    /// start_indexが総char数以上だった（終端到達）。
    pub ended: bool,
}

/// 本文をstart_index/max_charsでスライスする。
///
/// - start_index >= total: 空スライス + ended=true
/// - max_chars == 0: 空スライス（メタのみ）、ended判定はstart次第
pub fn slice(body: &str, start_index: usize, max_chars: usize) -> Slice {
    // char_indices走査でstart/endのバイトオフセットを求め、Vec<char>への全コピーを避ける（設計§3）。
    let total = body.chars().count();

    if start_index >= total {
        return Slice {
            content: String::new(),
            start: total,
            end: total,
            total,
            truncated: false,
            ended: total > 0 || start_index > 0,
        };
    }

    let end = start_index.saturating_add(max_chars).min(total);
    let mut start_byte = body.len();
    let mut end_byte = body.len();
    for (char_idx, (byte_idx, _)) in body.char_indices().enumerate() {
        if char_idx == start_index {
            start_byte = byte_idx;
        }
        if char_idx == end {
            end_byte = byte_idx;
            break;
        }
    }
    if end == total {
        end_byte = body.len();
    }
    Slice {
        content: body[start_byte..end_byte].to_string(),
        start: start_index,
        end,
        total,
        truncated: end < total,
        ended: false,
    }
}

/// POSIXシェル向けに単一引用符でクォートする。埋め込みの `'` は `'\''` へ。
/// 継続コマンドはコピペ実行されうるため、URL中の `&`/`?`/空白等による
/// 誤動作・コマンド注入(A03)を防ぐ。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 継続コマンドを生成する（設計§6）。
/// - --start-indexは新オフセットに置換
/// - -o/--outputは除外
/// - それ以外の非デフォルトフラグ(extra_flags)を再現
pub fn continue_command(url: &str, next_start: usize, extra_flags: &[String]) -> String {
    let mut parts = vec![String::from("webgrab"), shell_quote(url)];
    parts.extend(extra_flags.iter().cloned());
    parts.push(format!("--start-index {next_start}"));
    parts.join(" ")
}

/// 切り詰めフッタ（設計§6）。
pub fn truncated_footer(url: &str, s: &Slice, extra_flags: &[String]) -> String {
    let cont = continue_command(url, s.end, extra_flags);
    format!(
        "[webgrab:truncated chars {}-{} of {} — continue: {}]",
        s.start, s.end, s.total, cont
    )
}

/// 終端フッタ（設計§6、start_index末尾超過時）。
pub fn end_footer(total: usize) -> String {
    format!("[webgrab:end total {total} chars]")
}

/// 短い本文の自己記述マーカー（設計§5、抽出後1〜199文字時）。
/// `suggest`は状況に応じた再試行フラグ（例: "--render or --raw" / "--raw"）。
pub fn short_content_marker(total: usize, suggest: &str) -> String {
    format!("[webgrab:short-content {total} chars — if unexpected, retry with {suggest}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_from_zero_truncates() {
        let s = slice("abcdefghij", 0, 4);
        assert_eq!(s.content, "abcd");
        assert_eq!((s.start, s.end, s.total), (0, 4, 10));
        assert!(s.truncated);
        assert!(!s.ended);
    }

    #[test]
    fn slice_middle() {
        let s = slice("abcdefghij", 4, 3);
        assert_eq!(s.content, "efg");
        assert!(s.truncated);
    }

    #[test]
    fn slice_last_page_no_truncation() {
        let s = slice("abcdefghij", 8, 5);
        assert_eq!(s.content, "ij");
        assert!(!s.truncated);
        assert!(!s.ended);
    }

    #[test]
    fn slice_start_beyond_total_is_end() {
        let s = slice("abc", 10, 100);
        assert_eq!(s.content, "");
        assert!(s.ended);
        assert!(!s.truncated);
        assert_eq!(s.total, 3);
    }

    #[test]
    fn slice_max_zero_is_empty_not_end() {
        let s = slice("abcdef", 0, 0);
        assert_eq!(s.content, "");
        assert!(!s.ended);
        // start=0 < total なので truncated（残りあり）
        assert!(s.truncated);
    }

    #[test]
    fn slice_multibyte_char_boundary_safe() {
        // 日本語（各3バイト）と絵文字（4バイト）を跨ぐスライスでパニックしない
        let body = "あい😀うえお";
        let s = slice(body, 2, 2); // char[2..4] = 😀う
        assert_eq!(s.content, "😀う");
        assert_eq!(s.total, 6);
    }

    #[test]
    fn continue_command_quotes_url_with_query_string() {
        // クエリ文字列中の & はシェルに貼ると誤動作するため、URLはクォートされるべき（A03）。
        let cmd = continue_command("https://x.test/p?a=1&b=2", 100, &[]);
        assert!(
            cmd.contains("'https://x.test/p?a=1&b=2'"),
            "URLが単一引用符で囲まれていない: {cmd}"
        );
    }

    #[test]
    fn continue_command_escapes_single_quote_in_url() {
        let cmd = continue_command("https://x.test/a'b", 0, &[]);
        // 埋め込みの ' は '\'' へエスケープされ、シェルインジェクションを防ぐ。
        assert!(cmd.contains(r"'\''"), "single quote未エスケープ: {cmd}");
    }

    #[test]
    fn continue_command_replaces_start_index_and_reproduces_flags() {
        let cmd = continue_command(
            "https://x.test",
            48000,
            &["--render".into(), "--format json".into()],
        );
        assert_eq!(
            cmd,
            "webgrab 'https://x.test' --render --format json --start-index 48000"
        );
    }

    #[test]
    fn truncated_footer_contains_range_and_continue() {
        let s = slice("abcdefghij", 0, 4);
        let f = truncated_footer("https://x.test", &s, &[]);
        assert!(f.contains("chars 0-4 of 10"));
        assert!(f.contains("--start-index 4"));
    }

    #[test]
    fn end_footer_format() {
        assert_eq!(end_footer(83000), "[webgrab:end total 83000 chars]");
    }
}
