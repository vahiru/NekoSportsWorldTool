//! 运行日志：彩色等宽滚动，自动滚底，上限 2000 行。
//! 入站行的 √/×/⚠ 前缀仅用于着色，落盘展示时剥离。

use super::theme;
use crate::textlog::{classify, LogKind};
use egui::{Color32, RichText, ScrollArea};
use std::collections::VecDeque;

const MAX_LINES: usize = 2000;
const FONT_SIZE: f32 = 12.5;

#[derive(Debug, Clone)]
pub struct LogLine {
    pub text: String,
    pub kind: LogKind,
}

impl LogLine {
    fn color(&self) -> Color32 {
        match self.kind {
            LogKind::Plain => theme::plain(),
            LogKind::Ok => theme::ok(),
            LogKind::Err => theme::err(),
            LogKind::Warn => theme::warn(),
        }
    }
}

#[derive(Default)]
pub struct LogStore {
    lines: VecDeque<LogLine>,
}

impl LogStore {
    pub fn push(&mut self, msg: &str) {
        for line in msg.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if self.lines.len() >= MAX_LINES {
                self.lines.pop_front();
            }
            let (kind, text) = classify(t);
            self.lines.push_back(LogLine {
                text: text.to_string(),
                kind,
            });
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// 只布局可见行（最多 2000 行）；行不折行，过长的行可横向滚动查看。
    pub fn render(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(FONT_SIZE);
        let row_height = ui.fonts(|f| f.row_height(&font));
        ScrollArea::both()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, self.lines.len(), |ui, range| {
                for line in self.lines.range(range) {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&line.text)
                                .font(font.clone())
                                .color(line.color()),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            });
    }
}
