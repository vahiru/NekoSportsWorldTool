//! 后台任务 → UI 的类型化消息。
//!
//! 每条消息可带会话代号（epoch）：登录/登出/会话失效后旧代号的结果直接丢弃。
//! 刷新类结果另带请求号（req），只有与当前请求号一致才会落到界面，
//! 避免慢请求覆盖新请求、或共享忙碌标记被其它任务提前清除。

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
    Login(Result<Session, String>),
    RunStage(String),
    RunDone(serde_json::Value),
    AiDone(serde_json::Value),
    /// 批量补签进度：batch 为批次号，用于丢弃旧批次的迟到消息。
    AiBatchProgress {
        batch: u64,
        done: i64,
        total: i64,
    },
    AiList {
        req: u64,
        result: Result<Vec<AiSport>, String>,
    },
    Records {
        req: u64,
        result: Result<Vec<RecordRow>, String>,
    },
    AiRecords {
        req: u64,
        result: Result<(Vec<AiRecordGroup>, i64), String>,
    },
    /// 跑步详情（rrid 在 JSON 内，界面按当前打开的记录过滤）。
    RunDetail(serde_json::Value),
    /// AI 记录详情：id 为请求的记录号。
    AiDetail {
        id: i64,
        raw: serde_json::Value,
    },
    Semester {
        req: u64,
        result: Result<Option<SemesterSummary>, String>,
    },
    Cheat {
        req: u64,
        result: Result<CheatReport, String>,
    },
    Rank {
        req: u64,
        result: Result<Vec<RankRow>, String>,
    },
    User {
        req: u64,
        info: crate::api::user::MyInfo,
    },
    RoadStage {
        req: u64,
        stage: String,
    },
    UpdateCheck(Result<Option<ReleaseInfo>, String>),
    UpdateProgress {
        done: u64,
        total: Option<u64>,
    },
    /// Ok(tag) / Err(message)
    UpdateDone(Result<String, String>),
    /// 已保存的学校路网列表（后台读取）。
    CacheEntries {
        req: u64,
        entries: Vec<crate::api::school_roads::Entry>,
    },
    /// GPX 读取/分析结果（后台进行）。
    Gpx {
        req: u64,
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
