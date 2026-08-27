//! 分離ワールド（`Page.createIsolatedWorld`）での評価（設計 08 §4.2 手順3・6）。
//!
//! ページ側の`defineProperty`等による上書きが効かない文脈で数値・DOM HTMLを取得する。

use chromiumoxide::cdp::browser_protocol::page::{CreateIsolatedWorldParams, FrameId};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use chromiumoxide::page::Page;
use std::time::Duration;

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

    /// DOM HTML（doctype + outerHTML）を分離ワールドで取得する。失敗・タイムアウトはNone。
    pub(super) async fn dom_html(&mut self, limit: Duration) -> Option<String> {
        const EXPR: &str = "(function(){var s='';if(document.doctype){s=new XMLSerializer().serializeToString(document.doctype);}var d=document.documentElement;if(d){s+=d.outerHTML;}return s;})()";
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
                    return resp
                        .result
                        .result
                        .value
                        .as_ref()?
                        .as_str()
                        .map(|s| s.to_string());
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
