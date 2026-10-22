//! Job tickets, typed replies, AI batch cancellation and repaint scheduling.
use super::*;
use crate::api::cheat::{CheatReport, FRESH_MS};
use chrono::TimeZone;
use serde_json::json;
use std::time::{Duration, Instant};

fn app() -> App {
    App::from_state(
        &egui::Context::default(),
        HeaderIdentity::default(),
        Session::default(),
        Config::default(),
    )
}

fn rank_row(name: &str) -> crate::api::rank::RankRow {
    crate::api::rank::RankRow {
        sort: 1,
        name: name.into(),
        length: 1000.0,
        gender: 1,
    }
}

#[test]
fn ai_detail_reply_for_another_record_is_ignored() {
    let mut app = app();
    let old = app.jobs.start(JobKind::AiDetail);
    app.records_page.ai_detail_id = Some(2);
    let current = app.jobs.start(JobKind::AiDetail);
    app.tx
        .send(
            UiEvent::AiDetail {
                ticket: old,
                id: 1,
                raw: json!({"id": 1, "name": "old"}),
            }
            .into(),
        )
        .unwrap();
    // Current request, but a reply for a record that is no longer open.
    app.tx
        .send(
            UiEvent::AiDetail {
                ticket: current.clone(),
                id: 1,
                raw: json!({"id": 1, "name": "other record"}),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(app.records_page.ai_detail_raw.is_none());
    assert!(app.jobs.busy(JobKind::AiDetail));
    app.tx
        .send(
            UiEvent::AiDetail {
                ticket: current,
                id: 2,
                raw: json!({"id": 2, "name": "current"}),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert_eq!(
        app.records_page.ai_detail_raw.as_ref().unwrap()["name"],
        "current"
    );
    assert!(!app.jobs.busy(JobKind::AiDetail));
}

#[test]
fn semester_reply_does_not_end_a_running_violation_check() {
    let mut app = app();
    let semester = app.jobs.start(JobKind::Semester);
    let cheat = app.jobs.start(JobKind::Cheat);
    app.tx
        .send(
            UiEvent::Semester {
                ticket: semester,
                result: Ok(None),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(!app.jobs.busy(JobKind::Semester));
    assert!(
        app.jobs.busy(JobKind::Cheat),
        "立即检查 must stay disabled until the check itself replies"
    );
    app.tx
        .send(
            UiEvent::Cheat {
                ticket: cheat,
                result: Ok(CheatReport::default()),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(!app.jobs.busy(JobKind::Cheat));
    assert!(app.data_page.cheat.is_some());
}

#[test]
fn overlapping_rank_queries_only_show_the_latest_rows() {
    let mut app = app();
    let old = app.jobs.start(JobKind::Rank);
    let new = app.jobs.start(JobKind::Rank);
    app.tx
        .send(
            UiEvent::Rank {
                ticket: old,
                result: Ok(vec![rank_row("old query")]),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(app.data_page.rank_rows.is_empty());
    assert!(app.jobs.busy(JobKind::Rank));
    app.tx
        .send(
            UiEvent::Rank {
                ticket: new,
                result: Ok(vec![rank_row("new query")]),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert_eq!(app.data_page.rank_rows[0].name, "new query");
    assert!(!app.jobs.busy(JobKind::Rank));
}

#[test]
fn run_and_ai_record_refreshes_have_independent_busy_flags_and_no_done_marker() {
    let mut app = app();
    app.jobs.start(JobKind::Records);
    let ai_records = app.jobs.start(JobKind::AiRecords);
    app.tx
        .send(
            UiEvent::AiRecords {
                ticket: ai_records,
                result: Ok((vec![], 3)),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(!app.jobs.busy(JobKind::AiRecords));
    assert!(
        app.jobs.busy(JobKind::Records),
        "AI records must not clear the run-record refresh"
    );
    assert_eq!(app.records_page.ai_total, 3);
    assert_eq!(
        app.log.len(),
        0,
        "completion must not leak protocol text into the log"
    );
}

fn batch_plan() -> super::ai::AiBatchPlan {
    super::ai::AiBatchPlan {
        sport_ids: vec![1, 2],
        days: 2,
        per_day: 1,
        mode: crate::api::ai::AiMode::Count { reps: 10 },
    }
}

#[test]
fn batch_loop_stops_between_items_when_cancelled_or_session_expires() {
    let plan = batch_plan();
    let mut calls = 0;
    let mut waits = 0;
    let outcome = super::ai::run_batch_items(
        &plan,
        &mut |_, _| {
            calls += 1;
            Ok(json!({"error": 10000}))
        },
        &mut |_| {
            waits += 1;
            (waits > 2).then(|| "已取消".to_string())
        },
        &mut |_| {},
        &mut |_| {},
    );
    assert_eq!(calls, 1);
    assert_eq!((outcome.ok, outcome.done), (1, 1));
    assert_eq!(outcome.stopped.as_deref(), Some("已取消"));

    let mut calls = 0;
    let outcome = super::ai::run_batch_items(
        &plan,
        &mut |_, _| {
            calls += 1;
            Err(crate::api::client::SESSION_EXPIRED.into())
        },
        &mut |_| None,
        &mut |_| {},
        &mut |_| {},
    );
    assert_eq!(calls, 1, "an expired session must not keep submitting");
    assert!(outcome.stopped.is_some());

    let outcome = super::ai::run_batch_items(
        &plan,
        &mut |_, _| Ok(json!({"error": 10000})),
        &mut |_| None,
        &mut |_| {},
        &mut |_| {},
    );
    assert_eq!((outcome.ok, outcome.done, outcome.stopped), (4, 4, None));
}

#[test]
fn batch_stop_button_cancels_and_session_change_stops_the_worker() {
    let ctx = egui::Context::default();
    let mut app = App::from_state(
        &ctx,
        HeaderIdentity::default(),
        Session {
            uid: 42,
            token: "offline-only".into(),
            ..Default::default()
        },
        Config::default(),
    );
    // Closed loopback port: any upload attempt fails locally, never reaching a real server.
    app.route_fixture = Some(("http://127.0.0.1:9".into(), std::env::temp_dir()));
    app.tab = 1;
    app.run_ai_batch(batch_plan());
    assert!(app.jobs.busy(JobKind::AiSubmit));
    let at = scroll_to_label(&mut app, &ctx, 880., 900., "停止批量补签");
    click(&mut app, &ctx, 880., 900., at);
    assert!(!app.jobs.cancellable(JobKind::AiSubmit));
    assert!(app.status.contains("停止批量补签"));
    // A session change drops tagged progress, but the untagged completion still arrives.
    app.renew_session_jobs();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.jobs.busy(JobKind::AiSubmit) {
        app.poll_messages();
        assert!(Instant::now() < deadline, "batch worker did not stop");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.ai_batch.is_none());
    assert!(app.status.contains("中止"));
    let popup = app.popup.as_ref().unwrap().lines.join("\n");
    assert!(popup.contains("已中止"), "{popup}");
}

#[test]
fn logout_or_expiry_cancels_batch_and_stale_completion_is_ignored() {
    let mut app = app();
    let previous = app.jobs.start(JobKind::AiSubmit);
    let batch = app.jobs.start(JobKind::AiSubmit);
    app.ai_batch = Some(AiBatchState { done: 1, total: 4 });
    app.renew_session_jobs();
    assert!(batch.check().is_err(), "session change must stop the batch");
    assert!(
        !app.jobs.cancellable(JobKind::AiSubmit),
        "session change must stop the batch"
    );
    assert!(
        app.jobs.busy(JobKind::AiSubmit),
        "busy until the worker reports it stopped"
    );
    app.tx
        .send(
            UiEvent::AiDone {
                ticket: previous,
                result: json!({"ok": true, "batch": true}),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(
        app.jobs.busy(JobKind::AiSubmit),
        "completion of another batch is ignored"
    );
    assert!(app.ai_batch.is_some());
    app.tx
        .send(
            UiEvent::AiDone {
                ticket: batch,
                result: json!({
                    "ok": true, "batch": true,
                    "success": 1, "done": 1, "total": 4, "stopped": "已取消",
                }),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(!app.jobs.busy(JobKind::AiSubmit));
    assert!(app.ai_batch.is_none());
}

#[test]
fn pending_specified_time_warning_is_shown_once() {
    // Drawing reads the real clock. Today's 23:59 plus a run longer than a minute
    // cannot have ended at any wall-clock time today (and after midnight it is a
    // new "today"), so the warning is due regardless of when this test runs; the
    // time rule itself is covered with fixed instants in `crate::plan`.
    let ctx = egui::Context::default();
    let mut app = app();
    app.tab = 0;
    app.run_page.start_mode = 1;
    app.run_page.days_ago = 0;
    app.run_page.hour = 23;
    app.run_page.minute = 59;
    frame(&mut app, &ctx, 880., 2000., vec![]);
    assert!(app.run_page.dur_hint() > 60, "{}", app.run_page.dur_hint());
    let output = frame(&mut app, &ctx, 880., 2000., vec![]);
    let count = output
        .shapes
        .iter()
        .filter(|clipped| {
            matches!(&clipped.shape, egui::Shape::Text(text)
                if text.galley.text() == crate::ui::run::SPECIFIED_PENDING_WARNING)
        })
        .count();
    assert_eq!(count, 1);
}

#[test]
fn repaint_is_scheduled_for_the_next_visible_time_change() {
    let mut app = app();
    app.tab = 3;
    // Fixed instant (2026-10-22 08:00 local): the repaint rules only depend on `now`.
    let now = chrono::Local
        .with_ymd_and_hms(2026, 10, 22, 8, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    assert_eq!(app.next_repaint_delay(now), IDLE_REPAINT);

    // Violation result turns stale in 3 s: repaint exactly then instead of every second.
    app.data_page.cheat =
        Some(CheatReport::from_data(&json!({"self": null}), now - FRESH_MS + 3_000).unwrap());
    let delay = app.next_repaint_delay(now);
    assert!(
        (Duration::from_millis(3_000)..=Duration::from_millis(3_100)).contains(&delay),
        "{delay:?}"
    );
    // Already stale: nothing left to change.
    app.data_page.cheat =
        Some(CheatReport::from_data(&json!({"self": null}), now - FRESH_MS - 1).unwrap());
    assert_eq!(app.next_repaint_delay(now), IDLE_REPAINT);

    // Any running job keeps the 1 s fallback.
    let rank = app.jobs.start(JobKind::Rank);
    assert_eq!(app.next_repaint_delay(now), BUSY_REPAINT);
    assert!(app.jobs.accept(&rank));
    assert_eq!(app.next_repaint_delay(now), IDLE_REPAINT);
    // Including jobs outside the old hand-written list (e.g. update check).
    let update = app.jobs.start(JobKind::UpdateCheck);
    assert_eq!(app.next_repaint_delay(now), BUSY_REPAINT);
    assert!(app.jobs.accept(&update));

    // Run page: the "specified time" warning disappears once the whole run
    // (start + duration + jitter margin) would have ended.
    app.tab = 0;
    app.data_page.cheat = None;
    app.run_page.start_mode = 1;
    app.run_page.days_ago = 0;
    app.run_page.hour = 10;
    app.run_page.minute = 1;
    let ten = chrono::Local
        .with_ymd_and_hms(2026, 10, 22, 10, 1, 0)
        .single()
        .unwrap()
        .timestamp_millis()
        + app.run_page.dur_hint() * 1000;
    let delay = app.next_repaint_delay(ten);
    assert!(
        (Duration::from_millis(5_000)..=Duration::from_millis(5_100)).contains(&delay),
        "{delay:?}"
    );
    // Otherwise the next run-page change is midnight (date labels / random window).
    app.run_page.days_ago = 1;
    let midnight = chrono::Local
        .with_ymd_and_hms(2026, 10, 23, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    assert_eq!(app.run_page.next_time_transition(ten), Some(midnight));
}

#[test]
fn session_bound_reply_from_before_a_session_change_is_ignored_even_untagged() {
    let mut app = app();
    let records = app.jobs.start(JobKind::Records);
    let update = app.jobs.start(JobKind::UpdateCheck);
    app.renew_session_jobs();
    assert!(!app.jobs.busy(JobKind::Records));
    assert!(
        app.jobs.busy(JobKind::UpdateCheck),
        "session-independent jobs survive a session change"
    );
    app.status = "new session".into();
    // Untagged on the channel: only the ticket's own epoch/id can reject it.
    app.tx
        .send(
            UiEvent::Records {
                ticket: records,
                result: Ok(vec![]),
            }
            .into(),
        )
        .unwrap();
    app.tx
        .send(
            UiEvent::UpdateCheck {
                ticket: update,
                result: Ok(None),
            }
            .into(),
        )
        .unwrap();
    app.poll_messages();
    assert!(app.records_page.rows.is_empty());
    assert!(!app.jobs.busy(JobKind::UpdateCheck));
    assert!(app.update.up_to_date);
    assert_eq!(app.status, "已是最新版本");
}

#[test]
fn corrupt_config_on_startup_falls_back_to_defaults_and_is_shown_to_the_user() {
    let root = std::env::temp_dir().join(format!("startup-state-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = Some(root.clone()));
    std::fs::write(root.join("config.json"), "{ not json").unwrap();

    let state = StartupState::load();
    assert_eq!(state.config.username, Config::default().username);
    assert!(!state.session.is_logged_in());
    assert_eq!(state.warnings.len(), 1, "{:?}", state.warnings);
    let backups: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.corrupt-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "the corrupt file is kept as a backup");
    let backup = backups[0].path().display().to_string();

    let ctx = egui::Context::default();
    let mut app = App::from_state(&ctx, state.identity, state.session, state.config);
    app.report_startup_warnings(&state.warnings);
    let expected = format!("配置文件损坏，已备份为 {backup}，已使用默认设置");
    assert_eq!(app.status, expected);
    assert_eq!(app.startup_notice.as_deref(), Some(expected.as_str()));
    assert_eq!(app.log.len(), 1, "the warning is written to the UI log");
    // The persistent notice stays visible in the status bar (also on phones) until dismissed.
    for w in [360., 1100.] {
        // First frame at a new width only measures the status bar.
        frame(&mut app, &ctx, w, 800., vec![]);
        let output = frame(&mut app, &ctx, w, 800., vec![]);
        assert!(
            label_position(&output, &expected).is_some(),
            "notice hidden at {w}"
        );
    }
    let output = frame(&mut app, &ctx, 1100., 800., vec![]);
    click(
        &mut app,
        &ctx,
        1100.,
        800.,
        label_position(&output, "知道了").unwrap(),
    );
    assert!(app.startup_notice.is_none());

    // The corrupt file was moved aside, so the next start is clean and reports nothing.
    assert!(!root.join("config.json").exists());
    assert!(StartupState::load().warnings.is_empty());
    crate::platform::TEST_DATA_DIR.with(|p| *p.borrow_mut() = None);
    std::fs::remove_dir_all(root).unwrap();
}
