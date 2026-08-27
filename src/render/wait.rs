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

/// 未完了要求の集合。挿入・削除とも冪等。削除済みIDは短命のtombstoneに残し、
/// 順序が入れ替わって後から届いた挿入を無視する。
pub struct InFlight {
    live: HashSet<String>,
    live_order: VecDeque<String>,
    tombstones: HashMap<String, Instant>,
}

impl Default for InFlight {
    fn default() -> Self {
        Self::new()
    }
}

impl InFlight {
    pub fn new() -> Self {
        Self { live: HashSet::new(), live_order: VecDeque::new(), tombstones: HashMap::new() }
    }

    pub fn on_request(&mut self, id: &str, _is_redirect: bool, now: Instant) {
        self.expire(now);
        if self.tombstones.contains_key(id) || self.live.contains(id) {
            return;
        }
        if self.live.len() >= MAX_TRACKED {
            self.live_order.pop_front().map(|old| self.live.remove(&old));
        }
        self.live.insert(id.to_string());
        self.live_order.push_back(id.to_string());
    }

    pub fn on_done(&mut self, id: &str, now: Instant) {
        self.expire(now);
        self.live.remove(id);
        if self.tombstones.len() >= MAX_TRACKED {
            self.tombstones.clear();
        }
        self.tombstones.insert(id.to_string(), now);
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

    fn expire(&mut self, now: Instant) {
        let ttl = Duration::from_millis(TOMBSTONE_MS);
        self.tombstones.retain(|_, t| now.duration_since(*t) < ttl);
        self.live_order.retain(|id| self.live.contains(id));
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
        Self { max, total: AtomicU64::new(0), exceeded: AtomicBool::new(false) }
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
pub fn should_stop(idle: bool, stable_polls: u32, text_len: usize, elapsed: Duration, cap: Duration) -> bool {
    if elapsed >= cap {
        return true;
    }
    idle && stable_polls >= STABLE_POLLS && text_len >= MIN_TEXT_CHARS
}

/// メインナビゲーション（メインフレームのDocument要求）か（設計§4.2 手順4）。
pub fn is_main_navigation(resource_type: &ResourceType, frame_id: &FrameId, main_frame_id: &FrameId) -> bool {
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
        assert!(f.is_idle(now + Duration::from_millis(10)), "tombstone中の遅延挿入は無視される");
        f.on_request("x", false, now + Duration::from_millis(TOMBSTONE_MS + 1));
        assert!(!f.is_idle(now + Duration::from_millis(TOMBSTONE_MS + 1)), "失効後は通常どおり挿入される");
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
        assert!(should_stop(false, 0, 0, cap, cap), "上限到達は他条件によらず停止");
    }

    #[test]
    fn main_navigation_requires_document_in_main_frame() {
        let main = FrameId::from("MAIN".to_string());
        let other = FrameId::from("SUB".to_string());
        assert!(is_main_navigation(&ResourceType::Document, &main, &main));
        assert!(!is_main_navigation(&ResourceType::Document, &other, &main));
        assert!(!is_main_navigation(&ResourceType::Image, &main, &main));
    }
}
