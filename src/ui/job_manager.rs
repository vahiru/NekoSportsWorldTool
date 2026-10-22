//! 通用后台任务管理：每类任务一个槽位（当前请求号 / 忙碌 / 取消令牌 / 开始时间）。
//!
//! 用法：`let ticket = app.jobs.start(JobKind::X)` 发起请求，把票据交给工作线程；
//! 工作线程用票据标记结果事件；UI 收到结果时 `app.jobs.accept(&ticket)`，只有当前请求
//! （且会话代号一致）才返回 true 并清除忙碌。新请求自动作废并取消同类旧请求；
//! `reset_session()` 在登录 / 登出 / 会话失效时按各类任务的会话策略处理。

use crate::task_control::Control;
use std::sync::Arc;
use std::time::Instant;

/// 后台任务类型：每种一个槽位，同类同时只认一个请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Login,
    /// 跑步提交（可取消准备阶段；进入提交后不可取消）。
    Run,
    /// AI 成绩提交：单次或批量补签（批量可停止）。
    AiSubmit,
    AiList,
    Records,
    AiRecords,
    RunDetail,
    AiDetail,
    Semester,
    /// 违规自查。
    Cheat,
    Rank,
    User,
    UpdateCheck,
    UpdateDownload,
    /// 路网加载（手动 OSM / 学校路网 / 离线缓存）。
    RoadNetwork,
    /// 已保存学校路网列表读取。
    CacheEntries,
    /// GPX 读取 / 分析。
    Gpx,
    /// 跑步页路线检查与预览规划。
    Preview,
}

/// 会话切换（登录 / 登出 / 失效）时任务的处理方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionScope {
    /// 与会话无关：不受影响。
    Independent,
    /// 属于会话：旧结果作废、忙碌复位、取消令牌触发。
    Bound,
    /// 请求停止，但仍等待任务自己报告结束（结束消息照常接受，忙碌保持到那时）。
    StopOnChange,
}

impl JobKind {
    pub const ALL: [JobKind; 18] = [
        JobKind::Login,
        JobKind::Run,
        JobKind::AiSubmit,
        JobKind::AiList,
        JobKind::Records,
        JobKind::AiRecords,
        JobKind::RunDetail,
        JobKind::AiDetail,
        JobKind::Semester,
        JobKind::Cheat,
        JobKind::Rank,
        JobKind::User,
        JobKind::UpdateCheck,
        JobKind::UpdateDownload,
        JobKind::RoadNetwork,
        JobKind::CacheEntries,
        JobKind::Gpx,
        JobKind::Preview,
    ];

    pub fn session_scope(self) -> SessionScope {
        use JobKind::*;
        match self {
            AiList | Records | AiRecords | RunDetail | AiDetail | Semester | Cheat | Rank
            | User => SessionScope::Bound,
            AiSubmit => SessionScope::StopOnChange,
            Login | Run | UpdateCheck | UpdateDownload | RoadNetwork | CacheEntries | Gpx
            | Preview => SessionScope::Independent,
        }
    }

    /// 进行中时是否阻止开始下载更新（更新本身与短时的本地读取 / 详情加载不阻止）。
    pub fn blocks_update(self) -> bool {
        !matches!(
            self,
            JobKind::UpdateCheck
                | JobKind::UpdateDownload
                | JobKind::CacheEntries
                | JobKind::Gpx
                | JobKind::RunDetail
                | JobKind::AiDetail
        )
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// 一次请求的票据：交给工作线程，结果事件带回以便 UI 判断是否仍是当前请求。
#[derive(Clone)]
pub struct JobTicket {
    pub kind: JobKind,
    pub id: u64,
    /// 会话绑定任务发起时的会话代号；与会话无关（或会话切换后仍需收尾）的任务为 None。
    pub epoch: Option<u64>,
    cancel: Arc<Control>,
}

impl JobTicket {
    /// 本请求的取消令牌（工作线程在可取消阶段检查）。
    pub fn control(&self) -> &Control {
        &self.cancel
    }

    /// 已被取消（手动取消、被新请求取代或会话结束）时返回错误。
    pub fn check(&self) -> Result<(), String> {
        self.cancel.check()
    }
}

#[derive(Default)]
struct Slot {
    id: u64,
    busy: bool,
    cancel: Option<Arc<Control>>,
    started_at: Option<Instant>,
}

/// 各类后台任务的状态（App 持有）。
pub struct Jobs {
    epoch: u64,
    slots: [Slot; JobKind::ALL.len()],
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            epoch: 0,
            slots: std::array::from_fn(|_| Slot::default()),
        }
    }
}

impl Jobs {
    fn slot(&self, kind: JobKind) -> &Slot {
        &self.slots[kind.index()]
    }

    fn slot_mut(&mut self, kind: JobKind) -> &mut Slot {
        &mut self.slots[kind.index()]
    }

    /// 当前会话代号（登录 / 登出 / 会话失效时递增）。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// 发起新请求：取消并作废同类旧请求，置忙碌。
    pub fn start(&mut self, kind: JobKind) -> JobTicket {
        let epoch = (kind.session_scope() == SessionScope::Bound).then_some(self.epoch);
        let slot = self.slot_mut(kind);
        if let Some(old) = &slot.cancel {
            old.cancel();
        }
        let cancel = Arc::new(Control::default());
        slot.id = slot.id.wrapping_add(1);
        slot.busy = true;
        slot.cancel = Some(cancel.clone());
        slot.started_at = Some(Instant::now());
        JobTicket {
            kind,
            id: slot.id,
            epoch,
            cancel,
        }
    }

    /// 票据是否仍是该类任务的当前请求（同一会话）；不改变状态，用于中间进度消息。
    pub fn is_current(&self, ticket: &JobTicket) -> bool {
        self.slot(ticket.kind).id == ticket.id
            && ticket.epoch.is_none_or(|epoch| epoch == self.epoch)
    }

    /// 收到结果：是当前请求则清除忙碌并返回 true；过期结果返回 false 且不改变状态。
    pub fn accept(&mut self, ticket: &JobTicket) -> bool {
        if !self.is_current(ticket) {
            return false;
        }
        let slot = self.slot_mut(ticket.kind);
        slot.busy = false;
        slot.started_at = None;
        true
    }

    /// 请求取消当前任务（协作式）：忙碌保持，直到任务报告结束。
    /// 返回是否成功进入取消状态（已进入不可取消阶段时为 false）。
    pub fn cancel(&mut self, kind: JobKind) -> bool {
        self.slot(kind).cancel.as_ref().is_some_and(|c| c.cancel())
    }

    /// 放弃当前任务：取消令牌、作废其结果并立即清除忙碌。
    pub fn abandon(&mut self, kind: JobKind) {
        let slot = self.slot_mut(kind);
        if let Some(c) = &slot.cancel {
            c.cancel();
        }
        slot.id = slot.id.wrapping_add(1);
        slot.busy = false;
        slot.started_at = None;
    }

    /// 当前任务仍处于可取消阶段。
    pub fn cancellable(&self, kind: JobKind) -> bool {
        self.slot(kind)
            .cancel
            .as_ref()
            .is_some_and(|c| c.cancellable())
    }

    pub fn busy(&self, kind: JobKind) -> bool {
        self.slot(kind).busy
    }

    pub fn any_busy(&self) -> bool {
        self.slots.iter().any(|s| s.busy)
    }

    /// 是否有阻止开始下载更新的任务进行中。
    pub fn any_blocking_update(&self) -> bool {
        JobKind::ALL
            .iter()
            .any(|&kind| kind.blocks_update() && self.busy(kind))
    }

    /// 当前任务已进行的时间（空闲时为 None）。
    pub fn elapsed(&self, kind: JobKind) -> Option<std::time::Duration> {
        self.slot(kind).started_at.map(|t| t.elapsed())
    }

    /// 该类任务最近一次请求号（发起或放弃时变化）；可作为"数据来源已变化"的键。
    pub fn current_id(&self, kind: JobKind) -> u64 {
        self.slot(kind).id
    }

    /// 会话切换：递增会话代号，并按各类任务的 [`SessionScope`] 处理。
    pub fn reset_session(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        for kind in JobKind::ALL {
            match kind.session_scope() {
                SessionScope::Bound => self.abandon(kind),
                SessionScope::StopOnChange => {
                    self.cancel(kind);
                }
                SessionScope::Independent => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_listed_once_in_index_order() {
        for (index, kind) in JobKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), index, "{kind:?}");
        }
    }

    #[test]
    fn accept_takes_only_the_current_request_and_clears_busy() {
        let mut jobs = Jobs::default();
        assert!(!jobs.busy(JobKind::Rank) && !jobs.any_busy());
        let old = jobs.start(JobKind::Rank);
        let new = jobs.start(JobKind::Rank);
        assert!(old.check().is_err(), "a newer request cancels the old one");
        assert!(new.check().is_ok());
        assert!(jobs.busy(JobKind::Rank) && jobs.any_busy());
        assert!(jobs.elapsed(JobKind::Rank).is_some());
        assert!(!jobs.is_current(&old));
        assert!(!jobs.accept(&old), "stale reply");
        assert!(
            jobs.busy(JobKind::Rank),
            "stale reply keeps the newer request busy"
        );
        assert!(jobs.accept(&new));
        assert!(!jobs.busy(JobKind::Rank) && !jobs.any_busy());
        assert!(jobs.elapsed(JobKind::Rank).is_none());
        assert_eq!(jobs.current_id(JobKind::Rank), new.id);
    }

    #[test]
    fn kinds_are_independent() {
        let mut jobs = Jobs::default();
        let semester = jobs.start(JobKind::Semester);
        let cheat = jobs.start(JobKind::Cheat);
        assert!(jobs.accept(&semester));
        assert!(
            jobs.busy(JobKind::Cheat),
            "another kind's reply cannot end it"
        );
        assert!(cheat.check().is_ok());
        assert!(jobs.accept(&cheat));
    }

    #[test]
    fn session_reset_drops_bound_jobs_stops_batch_and_keeps_independent_ones() {
        let mut jobs = Jobs::default();
        let records = jobs.start(JobKind::Records);
        let ai = jobs.start(JobKind::AiSubmit);
        let run = jobs.start(JobKind::Run);
        let update = jobs.start(JobKind::UpdateCheck);
        assert_eq!(records.epoch, Some(0));
        assert_eq!(ai.epoch, None);
        jobs.reset_session();
        assert_eq!(jobs.epoch(), 1);
        // Bound: invalidated, not busy, token cancelled.
        assert!(!jobs.busy(JobKind::Records));
        assert!(records.check().is_err());
        assert!(!jobs.accept(&records));
        // StopOnChange: told to stop, still busy until it reports.
        assert!(ai.check().is_err());
        assert!(!jobs.cancellable(JobKind::AiSubmit));
        assert!(jobs.busy(JobKind::AiSubmit));
        assert!(jobs.accept(&ai));
        // Independent: untouched.
        assert!(run.check().is_ok() && jobs.cancellable(JobKind::Run));
        assert!(jobs.accept(&run));
        assert!(jobs.accept(&update));
    }

    #[test]
    fn bound_ticket_from_an_old_epoch_is_rejected_even_with_a_matching_id() {
        let mut jobs = Jobs::default();
        let ticket = jobs.start(JobKind::User);
        // Same id but issued for the previous session.
        jobs.epoch = jobs.epoch.wrapping_add(1);
        assert!(!jobs.is_current(&ticket));
        assert!(!jobs.accept(&ticket));
        assert!(jobs.busy(JobKind::User));
    }

    #[test]
    fn cancel_is_cooperative_and_abandon_drops_the_result() {
        let mut jobs = Jobs::default();
        assert!(!jobs.cancel(JobKind::Run), "nothing to cancel");
        assert!(!jobs.cancellable(JobKind::Run));
        let run = jobs.start(JobKind::Run);
        assert!(jobs.cancellable(JobKind::Run));
        assert!(jobs.cancel(JobKind::Run));
        assert!(!jobs.cancel(JobKind::Run), "already cancelled");
        assert!(run.check().is_err());
        assert!(jobs.busy(JobKind::Run), "busy until the worker reports");
        assert!(
            jobs.accept(&run),
            "the cancelled worker's own reply still ends it"
        );

        // Submission already started: cancellation is refused (task_control semantics).
        let run = jobs.start(JobKind::Run);
        run.control().begin_submit().unwrap();
        assert!(!jobs.cancellable(JobKind::Run));
        assert!(!jobs.cancel(JobKind::Run));
        assert!(jobs.accept(&run));

        let road = jobs.start(JobKind::RoadNetwork);
        jobs.abandon(JobKind::RoadNetwork);
        assert!(road.check().is_err());
        assert!(!jobs.busy(JobKind::RoadNetwork));
        assert_ne!(jobs.current_id(JobKind::RoadNetwork), road.id);
        assert!(!jobs.accept(&road));
    }

    #[test]
    fn update_blocking_ignores_update_and_short_local_jobs() {
        let mut jobs = Jobs::default();
        jobs.start(JobKind::UpdateDownload);
        jobs.start(JobKind::Gpx);
        assert!(jobs.any_busy());
        assert!(!jobs.any_blocking_update());
        jobs.start(JobKind::Login);
        assert!(jobs.any_blocking_update());
    }
}
