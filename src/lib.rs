//! Shared desktop and Android application.

mod api;
pub mod cli;
mod crypto;
// 诊断导出与路网错误说明只服务 GUI。
#[cfg(any(feature = "gui", test))]
mod diagnostics;
pub mod location;
pub mod platform;
// 自重启只被 GUI 更新流程调用
#[cfg(feature = "gui")]
mod relaunch;
#[cfg(any(feature = "gui", test))]
mod road_status;
mod start_time;
mod state;
mod task_control;
pub mod template;
mod textlog;
mod track;
mod update;

#[cfg(feature = "gui")]
mod ui;

#[cfg(all(target_os = "android", feature = "android"))]
mod android;

#[cfg(feature = "gui")]
pub fn run_gui(options: eframe::NativeOptions) -> eframe::Result<()> {
    eframe::run_native(
        "NekoSportsWorldTool",
        options,
        Box::new(|cc| {
            #[cfg(target_os = "android")]
            android::set_context(&cc.egui_ctx);
            Ok(Box::new(ui::App::new(cc)))
        }),
    )
}
