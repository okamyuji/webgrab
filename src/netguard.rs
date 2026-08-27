//! SSRF防止（設計§3.1）。IPアドレスの内部レンジ判定とIPv4-mapped正規化。
//!
//! ホスト名解決とIPピン留めの結線はfetch.rsが担う。本モジュールは
//! 「与えられたIPが拒否対象か」という純粋判定に集中し、単体テスト可能に保つ。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IPv4-mapped / IPv4-compatible なIPv6を対応するIPv4へ正規化する。
/// それ以外はそのまま返す。
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped().or_else(|| v4_compatible(v6)) {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// `::a.b.c.d` 形式（IPv4-compatible）をIPv4へ。
/// `::1` と `::` は正規化しない（IPv6として拒否判定に回す）。
fn v4_compatible(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let seg = v6.segments();
    if seg[0..6] == [0, 0, 0, 0, 0, 0] && seg[6] != 0 {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        Some(v4)
    } else {
        None
    }
}

fn seg_pair_to_v4(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::new(
        (hi >> 8) as u8,
        (hi & 0xff) as u8,
        (lo >> 8) as u8,
        (lo & 0xff) as u8,
    )
}

/// IPv6遷移アドレスに埋め込まれたIPv4を抽出する（SSRFバイパス対策）。
/// 6to4(2002::/16)・NAT64 well-known(64:ff9b::/96)・Teredo(2001:0::/32)。
/// Teredoのクライアントアドレスは0xffffでXOR難読化されているため復号する。
fn embedded_v4(v6: Ipv6Addr) -> Vec<Ipv4Addr> {
    let s = v6.segments();
    let mut out = Vec::new();
    if s[0] == 0x2002 {
        // 6to4: ビット16-47にIPv4
        out.push(seg_pair_to_v4(s[1], s[2]));
    }
    if s[0] == 0x0064 && s[1] == 0xff9b {
        // NAT64 well-known prefix: 末尾32ビットにIPv4
        out.push(seg_pair_to_v4(s[6], s[7]));
    }
    if s[0] == 0x2001 && s[1] == 0x0000 {
        // Teredo: サーバIPv4(seg2,3)とクライアントIPv4(seg6,7をXOR復号)
        out.push(seg_pair_to_v4(s[2], s[3]));
        out.push(seg_pair_to_v4(!s[6], !s[7]));
    }
    out
}

fn deny_range_v4(a: Ipv4Addr) -> Option<&'static str> {
    let o = a.octets();
    // 255.255.255.255（ブロードキャスト）は240.0.0.0/4に含まれるため`reserved`で拾う。
    if a.is_loopback() {
        Some("loopback") // 127.0.0.0/8
    } else if a.is_link_local() {
        Some("link-local") // 169.254.0.0/16（メタデータ含む）
    } else if a.is_private() {
        Some("private") // 10/8, 172.16/12, 192.168/16
    } else if a.is_unspecified() {
        Some("unspecified") // 0.0.0.0
    } else if a.is_multicast() {
        Some("multicast") // 224.0.0.0/4
    } else if o[0] == 0 {
        Some("this-network") // 0.0.0.0/8
    } else if o[0] >= 240 {
        Some("reserved") // 240.0.0.0/4
    } else if o[0] == 100 && (o[1] & 0xc0) == 64 {
        Some("cgn") // 100.64.0.0/10
    } else {
        None
    }
}

fn deny_range_v6(a: Ipv6Addr) -> Option<&'static str> {
    if a.is_loopback() {
        Some("loopback") // ::1
    } else if a.is_unspecified() {
        Some("unspecified") // ::
    } else if a.is_multicast() {
        Some("multicast") // ff00::/8
    } else if (a.segments()[0] & 0xffc0) == 0xfe80 {
        Some("link-local") // fe80::/10
    } else if (a.segments()[0] & 0xfe00) == 0xfc00 {
        Some("ula") // fc00::/7
    } else if embedded_v4(a)
        .into_iter()
        .any(|v| deny_range_v4(v).is_some())
    {
        Some("embedded-v4") // 6to4 / NAT64 / Teredo に埋め込まれた内部v4
    } else {
        None
    }
}

/// 最初に一致した拒否レンジのラベルを返す。許可なら`None`。
/// stderrの`range=`トークンに使う。IPv4-mapped等は先に正規化する。
pub fn deny_range(ip: IpAddr) -> Option<&'static str> {
    match normalize(ip) {
        IpAddr::V4(a) => deny_range_v4(a),
        IpAddr::V6(a) => deny_range_v6(a),
    }
}

/// 与えられたIPが内部アドレス（取得を拒否すべき）か判定する。
pub fn is_internal(ip: IpAddr) -> bool {
    deny_range(ip).is_some()
}

/// URLのスキームがhttp/httpsか検証する。
pub fn is_allowed_scheme(scheme: &str) -> bool {
    scheme == "http" || scheme == "https"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn loopback_and_private_denied() {
        assert!(is_internal(ip("127.0.0.1")));
        assert!(is_internal(ip("10.0.0.1")));
        assert!(is_internal(ip("172.16.5.5")));
        assert!(is_internal(ip("192.168.1.1")));
    }

    #[test]
    fn metadata_endpoint_denied() {
        assert!(is_internal(ip("169.254.169.254")));
    }

    #[test]
    fn cgn_and_zero_denied() {
        assert!(is_internal(ip("100.64.0.1")));
        assert!(is_internal(ip("100.100.0.1")));
        assert!(is_internal(ip("0.0.0.0")));
    }

    #[test]
    fn public_v4_allowed() {
        assert!(!is_internal(ip("8.8.8.8")));
        assert!(!is_internal(ip("1.1.1.1")));
        assert!(!is_internal(ip("93.184.216.34"))); // example.com
        assert!(!is_internal(ip("100.128.0.1"))); // 100.64/10 の外
    }

    #[test]
    fn ipv6_loopback_unspecified_denied() {
        assert!(is_internal(ip("::1")));
        assert!(is_internal(ip("::")));
    }

    #[test]
    fn ipv6_link_local_and_ula_denied() {
        assert!(is_internal(ip("fe80::1")));
        assert!(is_internal(ip("fc00::1")));
        assert!(is_internal(ip("fd12:3456::1")));
    }

    #[test]
    fn ipv4_mapped_normalized_then_denied() {
        // ::ffff:10.0.0.1 は内部
        assert!(is_internal(ip("::ffff:10.0.0.1")));
        assert!(is_internal(ip("::ffff:127.0.0.1")));
        // ::ffff:8.8.8.8 は公開
        assert!(!is_internal(ip("::ffff:8.8.8.8")));
    }

    #[test]
    fn ipv4_compatible_normalized() {
        // ::93.184.216.34 → 公開v4
        assert!(!is_internal(ip("::93.184.216.34")));
        // ::10.0.0.1 → 内部v4
        assert!(is_internal(ip("::10.0.0.1")));
    }

    #[test]
    fn public_v6_allowed() {
        assert!(!is_internal(ip("2606:4700:4700::1111"))); // cloudflare
    }

    #[test]
    fn embedded_v4_transitions_denied() {
        // 6to4 (2002::/16) に 127.0.0.1 / 10.0.0.1 を埋め込む → 内部扱い
        assert!(is_internal(ip("2002:7f00:0001::"))); // 127.0.0.1
        assert!(is_internal(ip("2002:0a00:0001::"))); // 10.0.0.1
        // NAT64 well-known prefix 64:ff9b::/96 に内部v4を埋め込む
        assert!(is_internal(ip("64:ff9b::7f00:1"))); // 127.0.0.1
        assert!(is_internal(ip("64:ff9b::a00:1"))); // 10.0.0.1
        // 6to4 に公開v4 (8.8.8.8) → 許可
        assert!(!is_internal(ip("2002:0808:0808::"))); // 8.8.8.8
        // NAT64 に公開v4 → 許可
        assert!(!is_internal(ip("64:ff9b::808:808"))); // 8.8.8.8
    }

    #[test]
    fn multicast_and_reserved_v4_denied() {
        assert!(is_internal(ip("224.0.0.1"))); // マルチキャスト
        assert!(is_internal(ip("239.255.255.250"))); // SSDP マルチキャスト
        assert!(is_internal(ip("240.0.0.1"))); // 予約 240/4
        assert!(is_internal(ip("255.255.255.255"))); // ブロードキャスト(既存is_broadcast)
    }

    #[test]
    fn multicast_v6_denied() {
        assert!(is_internal(ip("ff02::1"))); // リンクローカル全ノードマルチキャスト
    }

    #[test]
    fn deny_range_labels_first_match() {
        assert_eq!(deny_range(ip("127.0.0.1")), Some("loopback"));
        assert_eq!(deny_range(ip("169.254.1.1")), Some("link-local"));
        assert_eq!(deny_range(ip("10.0.0.1")), Some("private"));
        assert_eq!(deny_range(ip("::1")), Some("loopback"));
        assert_eq!(deny_range(ip("fe80::1")), Some("link-local"));
        assert_eq!(deny_range(ip("fc00::1")), Some("ula"));
        assert_eq!(deny_range(ip("8.8.8.8")), None);
        assert_eq!(deny_range(ip("0.0.0.0")), Some("unspecified"));
        assert_eq!(deny_range(ip("224.0.0.1")), Some("multicast"));
        assert_eq!(deny_range(ip("0.1.2.3")), Some("this-network"));
        assert_eq!(deny_range(ip("240.0.0.1")), Some("reserved"));
        assert_eq!(deny_range(ip("255.255.255.255")), Some("reserved"));
        assert_eq!(deny_range(ip("100.64.0.1")), Some("cgn"));
        assert_eq!(deny_range(ip("::")), Some("unspecified"));
        assert_eq!(deny_range(ip("ff02::1")), Some("multicast"));
        assert_eq!(deny_range(ip("2002:7f00:0001::")), Some("embedded-v4"));
        // 正規化を経る経路: ::ffff:10.0.0.1 は v4 の private として拾う。
        assert_eq!(deny_range(ip("::ffff:10.0.0.1")), Some("private"));
    }

    #[test]
    fn scheme_check() {
        assert!(is_allowed_scheme("http"));
        assert!(is_allowed_scheme("https"));
        assert!(!is_allowed_scheme("file"));
        assert!(!is_allowed_scheme("ftp"));
    }
}
