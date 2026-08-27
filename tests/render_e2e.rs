//! render経路のE2E（設計 08 §6）。実Chromeとローカルサーバで検証する。WEBGRAB_E2E=1で有効。
mod common;
use common::*;

macro_rules! e2e {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            if !e2e_enabled() { return; }
            let _g = E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            $body
        }
    };
}

fn all_routes() -> Vec<Route> {
    let mut v = vec![csr_fast(), csr_slow(), static_article(), short_static(), big_gzip(), dom_bomb()];
    v.extend(csr_xhr());
    v
}

e2e!(e1_render_csr_fast, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_FAST), "{out}");
});

e2e!(e2_render_csr_slow_waits_past_old_fixed_sleep, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_slow"), "--render", "--allow-private", "--no-robots", "--wait-ms", "8000"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_SLOW), "{out}");
});

e2e!(e3_render_xhr, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_xhr"), "--render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_XHR), "{out}");
});

e2e!(e4_auto_render_escalates_on_empty_shell, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_FAST), "{out}");
    assert!(err.contains("info=auto-render reason=empty"), "{err}");
    assert!(!out.contains("short-content"), "{out}");
});

e2e!(e5_auto_render_does_not_launch_for_static_article, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/static"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_STATIC));
    assert!(!err.contains("auto-render"), "{err}");
});

e2e!(e6_json_continue_command_uses_render, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--format", "json", "--max-chars", "50"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    let cc = v["continue_command"].as_str().unwrap();
    assert!(cc.contains("--render") && !cc.contains("--auto-render"), "{cc}");
});

e2e!(e7_wait_cap_is_enforced, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_slow"), "--render", "--allow-private", "--no-robots", "--wait-ms", "1500"]);
    assert!(!out.contains(SENTINEL_SLOW), "{out}");
    let ok = (code == 0 && out.contains("[webgrab:short-content")) || (code == 6 && err.contains("error=empty"));
    assert!(ok, "code={code} out={out} err={err}");
});

e2e!(e8_auto_render_falls_back_when_chrome_missing, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab_raw(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--chrome-path", "/nonexistent/chrome"]);
    assert_eq!(code, 6, "{err}");
    assert!(err.lines().any(|l| l.starts_with("webgrab: error=empty hint=--raw")), "{err}");
    assert!(err.lines().any(|l| l.starts_with("webgrab: warn=auto-render-failed reason=render")), "{err}");
});

e2e!(e9_render_refuses_internal_without_allow_private, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/csr_fast"), "--render", "--no-robots"]);
    assert_eq!(code, 8, "{err}");
    assert!(err.contains("error=netguard"), "{err}");
});

e2e!(e10a_no_gain_keeps_static_result, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/short"), "--auto-render", "--allow-private", "--no-robots"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(SENTINEL_SHORT), "{out}");
    assert!(out.contains("[webgrab:render-status no-gain reason=shorter]"), "{out}");
    assert!(err.contains("warn=auto-render-no-gain reason=shorter"), "{err}");
});

e2e!(e10b_no_gain_json_char_counts, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/short"), "--auto-render", "--allow-private", "--no-robots", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "no-gain");
    assert!(v["rendered_chars"].as_u64().unwrap() < v["static_chars"].as_u64().unwrap(), "{out}");
});

e2e!(e11_max_chars_zero_still_escalates, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--allow-private", "--no-robots", "--format", "json", "--max-chars", "0"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    assert_eq!(v["markdown"], "");
});

e2e!(e12_raw_escalates_on_visible_text, {
    let s = start(all_routes());
    let (code, out, err) = webgrab(&[&s.url("/csr_fast"), "--auto-render", "--raw", "--allow-private", "--no-robots", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["render_status"], "rendered");
    assert!(v["markdown"].as_str().unwrap().contains(SENTINEL_FAST));
});

e2e!(e13_render_max_bytes_counts_decoded_bytes, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/big_gzip"), "--render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});

e2e!(e14_static_phase_error_propagates_under_auto_render, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/big_gzip"), "--auto-render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});

e2e!(e15_dom_bomb_is_capped_before_content, {
    let s = start(all_routes());
    let (code, _out, err) = webgrab(&[&s.url("/dom_bomb"), "--render", "--allow-private", "--no-robots", "--max-bytes", "1048576"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error=http"), "{err}");
});
