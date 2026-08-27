//! render経路の待機判定と計数（設計§4.2）。Chrome非依存の純関数群。

use chromiumoxide::cdp::browser_protocol::network::ResourceType;
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const POLL_MS: u64 = 250;
/// 「直前の観測との一致」がこの回数連続したら安定（観測はこの回数+1必要）。
pub const STABLE_POLLS: u32 = 2;
pub const TOMBSTONE_MS: u64 = 2000;
pub const MAX_TRACKED: usize = 4096;
/// 早期終了に必要な可視テキスト長。pipelineの短文閾値と同じ値。
pub const MIN_TEXT_CHARS: usize = 200;
/// 手順6（同期待ち + DOM長評価 + content取得）のために待機上限から差し引く予備。
pub const CONTENT_RESERVE: Duration = Duration::from_millis(2000);

/// `goto`直前に確定する実効待機上限: min(--wait-ms, deadline − now − 予備2000ms)。
pub fn effective_cap(wait_ms: u64, deadline: Instant, now: Instant) -> Duration {
    let remaining = deadline
        .saturating_duration_since(now)
        .saturating_sub(CONTENT_RESERVE);
    Duration::from_millis(wait_ms).min(remaining)
}

/// `--max-bytes`超過メッセージ。`what`が空なら層の接尾辞を付けない。
pub fn exceed_msg(n: u64, max_bytes_total: u64, what: &str) -> String {
    let base =
        format!("render download exceeds remaining --max-bytes budget ({n} of {max_bytes_total})");
    if what.is_empty() {
        base
    } else {
        format!("{base} [{what}]")
    }
}

/// 未完了要求の集合。挿入・削除とも冪等。削除済みIDは短命のtombstoneに残し、
/// 順序が入れ替わって後から届いた挿入を無視する。
pub struct InFlight {
    live: HashSet<String>,
    live_order: VecDeque<String>,
    tombstones: HashMap<String, Instant>,
    /// 挿入順（=時刻順）。失効は先頭からのpopだけで済ませ、毎回の全走査を避ける。
    tombstone_order: VecDeque<(String, Instant)>,
}

impl Default for InFlight {
    fn default() -> Self {
        Self::new()
    }
}

impl InFlight {
    pub fn new() -> Self {
        Self {
            live: HashSet::new(),
            live_order: VecDeque::new(),
            tombstones: HashMap::new(),
            tombstone_order: VecDeque::new(),
        }
    }

    pub fn on_request(&mut self, id: &str, _is_redirect: bool, now: Instant) {
        self.expire(now);
        if self.tombstones.contains_key(id) || self.live.contains(id) {
            return;
        }
        self.drain_dead_order();
        if self.live.len() >= MAX_TRACKED {
            while let Some(old) = self.live_order.pop_front() {
                if self.live.remove(&old) {
                    break;
                }
            }
        }
        self.live.insert(id.to_string());
        self.live_order.push_back(id.to_string());
    }

    pub fn on_done(&mut self, id: &str, now: Instant) {
        self.expire(now);
        self.live.remove(id);
        self.drain_dead_order();
        if self.tombstones.contains_key(id) {
            return;
        }
        if self.tombstones.len() >= MAX_TRACKED
            && let Some((oldest, _)) = self.tombstone_order.pop_front()
        {
            self.tombstones.remove(&oldest);
        }
        self.tombstones.insert(id.to_string(), now);
        self.tombstone_order.push_back((id.to_string(), now));
    }

    pub fn is_idle(&mut self, now: Instant) -> bool {
        self.expire(now);
        self.live.is_empty()
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// live_orderは遅延削除。先頭に溜まった完了済みIDを捨てて長さを有界に保つ。
    fn drain_dead_order(&mut self) {
        while let Some(front) = self.live_order.front() {
            if self.live.contains(front) {
                break;
            }
            self.live_order.pop_front();
        }
    }

    #[cfg(test)]
    fn order_len(&self) -> usize {
        self.live_order.len()
    }

    /// 先頭からのpopだけで失効させられるのは、`now`が単調非減少で
    /// tombstone_orderの挿入順＝時刻順になるため。
    fn expire(&mut self, now: Instant) {
        let ttl = Duration::from_millis(TOMBSTONE_MS);
        while let Some((_, t)) = self.tombstone_order.front() {
            if now.duration_since(*t) < ttl {
                break;
            }
            if let Some((id, _)) = self.tombstone_order.pop_front() {
                self.tombstones.remove(&id);
            }
        }
    }
}

/// 展開後バイトの累計と上限判定。監視タスクとポーリングが共有する。
pub struct DecodedBudget {
    max: u64,
    total: AtomicU64,
    exceeded: AtomicBool,
}

impl DecodedBudget {
    pub fn new(max: u64) -> Self {
        Self {
            max,
            total: AtomicU64::new(0),
            exceeded: AtomicBool::new(false),
        }
    }
    /// 加算し、上限超過ならtrue（以後もtrue）。
    pub fn on_data(&self, len: u64) -> bool {
        let t = self.total.fetch_add(len, Ordering::SeqCst) + len;
        if t > self.max {
            self.exceeded.store(true, Ordering::SeqCst);
        }
        self.exceeded()
    }
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::SeqCst)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::SeqCst)
    }
}

/// 待機終了判定（設計§4.2 手順3・5）。
pub fn should_stop(
    idle: bool,
    stable_polls: u32,
    text_len: usize,
    elapsed: Duration,
    cap: Duration,
) -> bool {
    if elapsed >= cap {
        return true;
    }
    idle && stable_polls >= STABLE_POLLS && text_len >= MIN_TEXT_CHARS
}

/// メインナビゲーション（メインフレームのDocument要求）か（設計§4.2 手順4）。
pub fn is_main_navigation(
    resource_type: &ResourceType,
    frame_id: &FrameId,
    main_frame_id: &FrameId,
) -> bool {
    *resource_type == ResourceType::Document && frame_id == main_frame_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::network::ResourceType;
    use chromiumoxide::cdp::browser_protocol::page::FrameId;
    use std::time::{Duration, Instant};

    #[test]
    fn inflight_is_idempotent_on_redirect_and_unknown_done() {
        let now = Instant::now();
        let mut f = InFlight::new();
        f.on_request("a", false, now);
        f.on_request("a", true, now); // リダイレクト再送
        assert_eq!(f.len(), 1);
        f.on_done("zzz", now); // 未知ID
        assert_eq!(f.len(), 1);
        f.on_done("a", now);
        assert!(f.is_idle(now));
    }

    #[test]
    fn tombstone_ignores_late_request_until_expiry() {
        let now = Instant::now();
        let mut f = InFlight::new();
        f.on_done("x", now);
        f.on_request("x", false, now + Duration::from_millis(10));
        assert!(
            f.is_idle(now + Duration::from_millis(10)),
            "tombstone中の遅延挿入は無視される"
        );
        f.on_request("x", false, now + Duration::from_millis(TOMBSTONE_MS + 1));
        assert!(
            !f.is_idle(now + Duration::from_millis(TOMBSTONE_MS + 1)),
            "失効後は通常どおり挿入される"
        );
    }

    #[test]
    fn inflight_caps_tracked_ids() {
        let now = Instant::now();
        let mut f = InFlight::new();
        for i in 0..(MAX_TRACKED + 10) {
            f.on_request(&i.to_string(), false, now);
        }
        assert_eq!(f.len(), MAX_TRACKED);
    }

    #[test]
    fn live_order_stays_bounded_under_fifo_completion() {
        let now = Instant::now();
        let mut f = InFlight::new();
        for i in 0..1000 {
            let id = i.to_string();
            f.on_request(&id, false, now);
            f.on_done(&id, now);
            assert!(f.order_len() <= 1, "i={i} order_len={}", f.order_len());
        }
        assert!(f.is_idle(now));
    }

    #[test]
    fn decoded_budget_flags_first_exceed_and_stays() {
        let b = DecodedBudget::new(100);
        assert!(!b.on_data(60));
        assert!(b.on_data(50));
        assert!(b.on_data(1));
        assert!(b.exceeded());
        assert_eq!(b.total(), 111);
    }

    #[test]
    fn should_stop_matrix() {
        let cap = Duration::from_millis(5000);
        let t = Duration::from_millis(1000);
        assert!(should_stop(true, 2, 200, t, cap));
        assert!(!should_stop(false, 2, 200, t, cap));
        assert!(!should_stop(true, 1, 200, t, cap));
        assert!(!should_stop(true, 2, 199, t, cap));
        assert!(
            should_stop(false, 0, 0, cap, cap),
            "上限到達は他条件によらず停止"
        );
    }

    #[test]
    fn main_navigation_requires_document_in_main_frame() {
        let main = FrameId::from("MAIN".to_string());
        let other = FrameId::from("SUB".to_string());
        assert!(is_main_navigation(&ResourceType::Document, &main, &main));
        assert!(!is_main_navigation(&ResourceType::Document, &other, &main));
        assert!(!is_main_navigation(&ResourceType::Image, &main, &main));
    }

    #[test]
    fn tombstone_cap_evicts_oldest_only() {
        let now = Instant::now();
        let mut f = InFlight::new();
        for i in 0..MAX_TRACKED {
            f.on_done(&i.to_string(), now);
        }
        let len_before = f.len();
        f.on_done("new", now);
        let len_after = f.len();
        assert_eq!(len_before, 0);
        assert_eq!(len_after, 0);
        f.on_request("0", false, now);
        assert_eq!(f.len(), 1, "oldest tombstone evicted, new request accepted");
        f.on_request("1", false, now);
        assert_eq!(
            f.len(),
            1,
            "next oldest still in tombstone, request ignored"
        );
    }

    #[test]
    fn effective_cap_is_clamped_by_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(3500);
        assert_eq!(
            effective_cap(5000, deadline, now),
            Duration::from_millis(1500)
        );
        assert_eq!(
            effective_cap(1000, deadline, now),
            Duration::from_millis(1000)
        );
        assert_eq!(effective_cap(5000, now, now), Duration::ZERO);
    }

    #[test]
    fn exceed_msg_uses_measured_value() {
        assert_eq!(
            exceed_msg(3_000, 1_000, "dom"),
            "render download exceeds remaining --max-bytes budget (3000 of 1000) [dom]"
        );
        assert_eq!(
            exceed_msg(12, 10, ""),
            "render download exceeds remaining --max-bytes budget (12 of 10)"
        );
    }
}
