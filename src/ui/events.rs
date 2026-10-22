//! 后台任务 → UI 的类型化消息。
//!
//! 每条消息可带会话代号（epoch）：登录/登出/会话失效后旧代号的结果直接丢弃。
//! 任务结果另带发起时的票据（[`JobTicket`]），只有 `Jobs::accept` 认定为当前请求
//! 才会落到界面，避免慢请求覆盖新请求、或其它任务提前清除忙碌状态。

use super::job_manager::JobTicket;
use crate::api::ai::{AiRecordGroup, AiSport};
use crate::api::cheat::CheatReport;
use crate::api::model::Session;
use crate::api::rank::RankRow;
use crate::api::records::RecordRow;
use crate::api::semester::SemesterSummary;
use crate::update::ReleaseInfo;
use std::sync::Arc;

pub enum UiEvent {
    /// 普通日志行（√/×/⚠ 前缀用于着色）。
    Log(String),
    Ip(String),
    Login {
        ticket: JobTicket,
        result: Result<Session, String>,
    },
    RunStage(String),
    RunDone {
        ticket: JobTicket,
        result: serde_json::Value,
    },
    /// AI 单次提交或批量补签结束。
    AiDone {
        ticket: JobTicket,
        result: serde_json::Value,
    },
    /// 批量补签进度：票据用于丢弃旧批次的迟到消息。
    AiBatchProgress {
        ticket: JobTicket,
        done: i64,
        total: i64,
    },
    AiList {
        ticket: JobTicket,
        result: Result<Vec<AiSport>, String>,
    },
    Records {
        ticket: JobTicket,
        result: Result<Vec<RecordRow>, String>,
    },
    AiRecords {
        ticket: JobTicket,
        result: Result<(Vec<AiRecordGroup>, i64), String>,
    },
    /// 跑步详情（rrid 在 JSON 内，界面另按当前打开的记录过滤）。
    RunDetail {
        ticket: JobTicket,
        raw: serde_json::Value,
    },
    /// AI 记录详情：id 为请求的记录号。
    AiDetail {
        ticket: JobTicket,
        id: i64,
        raw: serde_json::Value,
    },
    Semester {
        ticket: JobTicket,
        result: Result<Option<SemesterSummary>, String>,
    },
    Cheat {
        ticket: JobTicket,
        result: Result<CheatReport, String>,
    },
    Rank {
        ticket: JobTicket,
        result: Result<Vec<RankRow>, String>,
    },
    User {
        ticket: JobTicket,
        info: crate::api::user::MyInfo,
    },
    RoadStage {
        ticket: JobTicket,
        stage: String,
    },
    UpdateCheck {
        ticket: JobTicket,
        result: Result<Option<ReleaseInfo>, String>,
    },
    UpdateProgress {
        ticket: JobTicket,
        done: u64,
        total: Option<u64>,
    },
    /// Ok(tag) / Err(message)
    UpdateDone {
        ticket: JobTicket,
        result: Result<String, String>,
    },
    /// 已保存的学校路网列表（后台读取）。
    CacheEntries {
        ticket: JobTicket,
        entries: Vec<crate::api::school_roads::Entry>,
    },
    /// GPX 读取/分析结果（后台进行）。
    Gpx {
        ticket: JobTicket,
        outcome: GpxOutcome,
    },
}

pub enum GpxOutcome {
    ReadFailed(String),
    Analyzed {
        /// 新读取的文件内容；重新分析时为 None（沿用已有文本）。
        text: Option<Arc<String>>,
        result: Result<crate::track::analysis::Analysis, String>,
    },
}

/// 通道上传递的消息：可选会话代号 + 事件。
pub struct UiMsg {
    pub epoch: Option<u64>,
    pub event: UiEvent,
}

impl From<UiEvent> for UiMsg {
    fn from(event: UiEvent) -> Self {
        UiMsg { epoch: None, event }
    }
}
