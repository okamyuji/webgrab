//! robots.txt の最小パーサと判定（設計§4 robots仕様、RFC 9309サブセット）。
//!
//! - User-agent行は製品トークン `webgrab`（大小無視）で照合、無ければ `*` グループ。
//! - Disallow/Allow のパスは前置一致に加え `*`（任意列）と行末 `$` をサポート。
//! - 最長一致で判定し、同長ならAllow優先。
//! - 解釈できないパターンに一致候補がある場合は安全側=disallowに倒す（呼び出し側で注記）。

const PRODUCT_TOKEN: &str = "webgrab";

#[derive(Debug, Clone)]
struct Rule {
    allow: bool,
    pattern: String,
}

/// robots.txtをパースし、対象UAグループのルール群を返す。
#[derive(Debug, Default)]
pub struct Robots {
    rules: Vec<Rule>,
}

impl Robots {
    pub fn parse(text: &str) -> Self {
        // グループ収集: 連続するUser-agent行が同一グループの複数UAを表す
        let mut groups: Vec<(Vec<String>, Vec<Rule>)> = Vec::new();
        let mut cur_agents: Vec<String> = Vec::new();
        let mut cur_rules: Vec<Rule> = Vec::new();
        let mut last_was_rule = false;

        let flush = |groups: &mut Vec<(Vec<String>, Vec<Rule>)>,
                     agents: &mut Vec<String>,
                     rules: &mut Vec<Rule>| {
            if !agents.is_empty() {
                groups.push((std::mem::take(agents), std::mem::take(rules)));
            }
        };

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            match key.as_str() {
                "user-agent" => {
                    if last_was_rule {
                        flush(&mut groups, &mut cur_agents, &mut cur_rules);
                        last_was_rule = false;
                    }
                    cur_agents.push(value.to_ascii_lowercase());
                }
                "disallow" | "allow" => {
                    last_was_rule = true;
                    // Disallow: 空 は「すべて許可」を意味する。ルールとして無視。
                    if key == "disallow" && value.is_empty() {
                        continue;
                    }
                    cur_rules.push(Rule {
                        allow: key == "allow",
                        pattern: value,
                    });
                }
                _ => {}
            }
        }
        flush(&mut groups, &mut cur_agents, &mut cur_rules);

        // 製品トークン一致グループを優先、無ければ `*` グループ
        let mut chosen: Option<Vec<Rule>> = None;
        for (agents, rules) in &groups {
            if agents.iter().any(|a| a == PRODUCT_TOKEN) {
                chosen = Some(rules.clone());
                break;
            }
        }
        if chosen.is_none() {
            for (agents, rules) in &groups {
                if agents.iter().any(|a| a == "*") {
                    chosen = Some(rules.clone());
                    break;
                }
            }
        }
        Robots {
            rules: chosen.unwrap_or_default(),
        }
    }

    /// パスが取得許可されているか。最長一致・同長Allow優先。
    pub fn allowed(&self, path: &str) -> bool {
        let mut best: Option<(&Rule, usize)> = None;
        for r in &self.rules {
            if let Some(len) = match_len(&r.pattern, path) {
                let better = match best {
                    None => true,
                    Some((cur, cur_len)) => {
                        len > cur_len || (len == cur_len && r.allow && !cur.allow)
                    }
                };
                if better {
                    best = Some((r, len));
                }
            }
        }
        match best {
            None => true,
            Some((r, _)) => r.allow,
        }
    }
}

/// パターンがpathの先頭にマッチするか。マッチすればマッチした「特異度」
/// （パターンのワイルドカードを除いた長さ）を返す。`*` と行末 `$` に対応。
fn match_len(pattern: &str, path: &str) -> Option<usize> {
    // 特異度 = パターンのリテラル文字数（* と $ を除く）
    let specificity = pattern.chars().filter(|&c| c != '*' && c != '$').count();
    if glob_match(pattern, path) {
        Some(specificity)
    } else {
        None
    }
}

/// robots.txt の `*`（任意の0文字以上）と末尾 `$`（終端アンカー）に対応した
/// 前方一致グロブ。`$`がなければ前置一致（prefix）。
fn glob_match(pattern: &str, path: &str) -> bool {
    let anchored_end = pattern.ends_with('$');
    let pat = if anchored_end {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };

    // `*` で分割したリテラル片を順に前から消費する。
    let segments: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    let bytes = path.as_bytes();

    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i == 0 {
            // 先頭片はpathの先頭に一致する必要がある（robotsはパス先頭からのマッチ）
            if !path[pos..].starts_with(seg) {
                return false;
            }
            pos += seg.len();
        } else {
            match path[pos..].find(seg) {
                Some(idx) => pos += idx + seg.len(),
                None => return false,
            }
        }
        let _ = bytes;
    }

    if anchored_end {
        // 最後の片の後に終端でなければならない。末尾が `*` なら残り任意。
        if pat.ends_with('*') {
            true
        } else {
            pos == path.len()
        }
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_disallow_allows_all() {
        let r = Robots::parse("User-agent: *\nDisallow:");
        assert!(r.allowed("/anything"));
    }

    #[test]
    fn prefix_disallow() {
        let r = Robots::parse("User-agent: *\nDisallow: /private");
        assert!(!r.allowed("/private/x"));
        assert!(r.allowed("/public"));
    }

    #[test]
    fn product_token_group_wins_over_star() {
        let txt = "User-agent: *\nDisallow: /\n\nUser-agent: webgrab\nDisallow: /admin";
        let r = Robots::parse(txt);
        assert!(r.allowed("/public")); // webgrabグループが選ばれ、/adminのみ禁止
        assert!(!r.allowed("/admin/panel"));
    }

    #[test]
    fn case_insensitive_agent() {
        let r = Robots::parse("User-agent: WebGrab\nDisallow: /x");
        assert!(!r.allowed("/x/y"));
    }

    #[test]
    fn star_group_fallback() {
        let r =
            Robots::parse("User-agent: googlebot\nDisallow: /\n\nUser-agent: *\nDisallow: /secret");
        assert!(r.allowed("/open"));
        assert!(!r.allowed("/secret/z"));
    }

    #[test]
    fn wildcard_star() {
        let r = Robots::parse("User-agent: *\nDisallow: /*/private");
        assert!(!r.allowed("/a/private"));
        assert!(!r.allowed("/foo/private/x"));
        assert!(r.allowed("/a/public"));
    }

    #[test]
    fn dollar_anchor() {
        let r = Robots::parse("User-agent: *\nDisallow: /*.pdf$");
        assert!(!r.allowed("/doc.pdf"));
        assert!(r.allowed("/doc.pdf.html"));
    }

    #[test]
    fn allow_overrides_disallow_same_length() {
        let r = Robots::parse("User-agent: *\nDisallow: /a\nAllow: /a");
        assert!(r.allowed("/a/b")); // 同特異度ならAllow優先
    }

    #[test]
    fn longer_disallow_wins() {
        let r = Robots::parse("User-agent: *\nAllow: /a\nDisallow: /a/secret");
        assert!(r.allowed("/a/public"));
        assert!(!r.allowed("/a/secret/x"));
    }

    #[test]
    fn no_matching_group_allows_all() {
        let r = Robots::parse("User-agent: googlebot\nDisallow: /");
        assert!(r.allowed("/anything")); // webgrabにも*にも該当グループなし
    }
}
