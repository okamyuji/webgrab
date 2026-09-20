//! 分離ワールド（`Page.createIsolatedWorld`）での評価（設計 08 §4.2 手順3・6）。
//!
//! ページ側の`defineProperty`等による上書きが効かない文脈で数値・DOM HTMLを取得する。

use super::wait::CONTENT_RESERVE;
use chromiumoxide::cdp::browser_protocol::page::{CreateIsolatedWorldParams, FrameId};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use chromiumoxide::page::Page;
use std::time::Duration;

/// 1回のevaluateに与える上限。deadline残余だけで丸めると、ハングした1回のevaluateが
/// 手順6の予備（CONTENT_RESERVE）まで食い潰し、DOM長評価とcontent取得の時間が残らない。
/// 待機上限（cap）の残余に予備を足した値でも丸め、どの1回も予備を越えて延びないようにする。
pub(super) fn eval_limit(deadline_remaining: Duration, cap_remaining: Duration) -> Duration {
    deadline_remaining.min(cap_remaining.saturating_add(CONTENT_RESERVE))
}

/// 分離ワールドでの評価。ページ側のdefineProperty等の上書きが効かない。
pub(super) struct IsolatedWorld {
    pub(super) page: Page,
    pub(super) frame: FrameId,
    pub(super) ctx: Option<ExecutionContextId>,
}

impl IsolatedWorld {
    async fn ensure_ctx(&mut self) -> Option<ExecutionContextId> {
        if let Some(c) = self.ctx {
            return Some(c);
        }
        let r = self
            .page
            .execute(
                CreateIsolatedWorldParams::builder()
                    .frame_id(self.frame.clone())
                    .world_name("webgrab")
                    .build()
                    .ok()?,
            )
            .await
            .ok()?;
        self.ctx = Some(r.execution_context_id);
        self.ctx
    }

    /// 式を評価してu64配列で返す。失敗・タイムアウト・非数値はNone（条件未達扱い）。
    async fn eval_numbers(&mut self, expr: &str, limit: Duration) -> Option<Vec<u64>> {
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder()
                .expression(expr)
                .context_id(ctx)
                .return_by_value(true)
                .build()
                .ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => {
                    let v = resp.result.result.value.clone()?;
                    let arr = v.as_array()?;
                    return arr
                        .iter()
                        .map(|x| x.as_f64().map(|f| f.max(0.0) as u64))
                        .collect();
                }
                Ok(Err(_)) if attempt == 0 => {
                    self.ctx = None;
                    continue;
                } // 文脈破棄→作り直して1回だけ再試行
                _ => return None,
            }
        }
        None
    }

    pub(super) async fn measure(&mut self, limit: Duration) -> Option<[u64; 2]> {
        let v = self.eval_numbers(
            "(function(){var b=document.body;return [document.getElementsByTagName('*').length,(b&&b.innerText||'').trim().length];})()",
            limit,
        ).await?;
        Some([*v.first()?, *v.get(1)?])
    }

    pub(super) async fn dom_length(&mut self, limit: Duration) -> Option<u64> {
        let v = self
            .eval_numbers(
                "(function(){var d=document.documentElement;return [d?d.outerHTML.length:0];})()",
                limit,
            )
            .await?;
        v.first().copied()
    }

    /// `location.href`とDOM HTML（doctype + outerHTML）を1回の評価で取得する（設計10 §4.2）。
    /// URLとDOMが同じ時点の値になる。失敗・タイムアウト・DOM HTMLが文字列でない場合はNone。
    /// `location.href`が文字列でない場合は、URLだけNoneでDOM HTMLは返す。
    pub(super) async fn dom_html(&mut self, limit: Duration) -> Option<(Option<String>, String)> {
        // slice(0, 8193)は転送量を抑える。上限超えは採用側（resolve_final_url）が捨てる。
        const EXPR: &str = "(function(){var s='';if(document.doctype){s=new XMLSerializer().serializeToString(document.doctype);}var d=document.documentElement;if(d){s+=d.outerHTML;}return [location.href.slice(0, 8193), s];})()";
        for attempt in 0..2 {
            let ctx = self.ensure_ctx().await?;
            let params = EvaluateParams::builder()
                .expression(EXPR)
                .context_id(ctx)
                .return_by_value(true)
                .build()
                .ok()?;
            match tokio::time::timeout(limit, self.page.execute(params)).await {
                Ok(Ok(resp)) => {
                    let arr = resp.result.result.value.as_ref()?.as_array()?;
                    let html = arr.get(1)?.as_str()?.to_string();
                    let url = arr.first().and_then(|v| v.as_str()).map(str::to_string);
                    return Some((url, html));
                }
                Ok(Err(_)) if attempt == 0 => {
                    self.ctx = None;
                    continue;
                }
                _ => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_limit_is_clamped_by_both_deadline_and_reserve() {
        let ms = Duration::from_millis;
        // cap残余3000 + 予備2000 = 5000 > deadline残余4000 なのでdeadlineが効く
        assert_eq!(eval_limit(ms(4000), ms(3000)), ms(4000));
        // deadline残余10000 > cap残余1000 + 予備2000 なので予備側が効く
        assert_eq!(eval_limit(ms(10000), ms(1000)), ms(3000));
        // 手順6（cap到達後）は予備の2000msが上限になる
        assert_eq!(eval_limit(ms(10000), Duration::ZERO), ms(2000));
        // deadlineが尽きていれば0（負にはならない）
        assert_eq!(eval_limit(Duration::ZERO, ms(5000)), Duration::ZERO);
    }
}
