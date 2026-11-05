//! Apply feedback must reflect the reply, and navigation must preserve guarded Undo.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use super::*;
use crate::Services;

fn app() -> (SheetApp, Arc<Mutex<Vec<Json>>>) {
    let inbox = Arc::new(Mutex::new(Vec::new()));
    let mut session = gridcraft_engine::Session::new();
    session.execute("file.new", json!({})).unwrap();
    let app = SheetApp::new(session, Services { agent_inbox: Some(inbox.clone()), agent_send: Some(Box::new(|_| {})), ..Default::default() });
    (app, inbox)
}

fn action(app: &mut SheetApp, params: Json) -> Json {
    json!({"command":"cell.set", "params":params, "context":app.agent_snapshot(&json!({})).unwrap()["context"]})
}

fn draft_app() -> (SheetApp, Arc<Mutex<Vec<Json>>>) {
    let (mut app, _) = app();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let requests = sent.clone();
    app.services.agent_send = Some(Box::new(move |request| requests.lock().unwrap().push(request)));
    app.agent.task = "Запиши 42 в B1".into();
    app.agent.open = true;
    (app, sent)
}

#[test]
fn labeled_run_saves_cell_before_snapshot_and_agent_undo_keeps_manual_input() {
    use egui_kittest::kittest::Queryable as _;
    let (mut app, sent) = draft_app();
    app.run("cell.set", json!({"cell":"A1","input":"old"})).unwrap();
    app.begin_edit(Some("manual draft".into()), false);
    assert!(app.agent_snapshot(&json!({})).is_err());
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 1000.0)).build_ui_state(|ui, app| show(app, &ui.ctx().clone()), app);
    harness.run_steps(3);
    assert!(sent.lock().unwrap().is_empty());
    harness.get_by_label("Сохранить ввод и предложить").click();
    harness.run_steps(3);
    let app = harness.state_mut();
    assert!(app.editor.is_none());
    assert_eq!(app.session.run("cell.get", json!({"cell":"A1"})).unwrap()["value"], "manual draft");
    assert_eq!(app.session.doc().unwrap().selection.active.a1(), "A1");
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(sent.lock().unwrap()[0]["method"], "agent.run");
    assert_eq!(sent.lock().unwrap()[0]["params"]["task"], "Запиши 42 в B1");
    let action = action(app, json!({"cell":"B1","input":"42"}));
    app.agent_apply_reviewed(&action).unwrap();
    app.agent_undo_reviewed().unwrap();
    assert_eq!(app.session.run("cell.get", json!({"cell":"A1"})).unwrap()["value"], "manual draft");
    assert_eq!(app.session.run("cell.get", json!({"cell":"B1"})).unwrap()["value"], Json::Null);
    assert_eq!(app.session.doc().unwrap().undo.len(), 2);
}

#[test]
fn cancel_cell_draft_keeps_task_and_does_not_send_or_write() {
    use egui_kittest::kittest::Queryable as _;
    let (mut app, sent) = draft_app();
    app.begin_edit(Some("discarded draft".into()), false);
    let before = app.session.doc().unwrap().wb.clone();
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 1000.0)).build_ui_state(|ui, app| show(app, &ui.ctx().clone()), app);
    harness.run_steps(3);
    harness.get_by_label("Отменить ввод ячейки").click();
    harness.run_steps(3);
    let app = harness.state_mut();
    assert!(app.editor.is_none());
    assert_eq!(app.agent.task, "Запиши 42 в B1");
    assert!(Arc::ptr_eq(&before, &app.session.doc().unwrap().wb));
    assert!(sent.lock().unwrap().is_empty());
    assert!(app.agent_snapshot(&json!({})).is_ok());
    harness.get_by_label("Выполнить / предложить");
}

#[test]
fn rejected_manual_input_keeps_draft_and_task_without_contacting_ai() {
    let (mut app, sent) = draft_app();
    app.run("data.validation", json!({"range":"A1","type":"whole","operator":"between","formula1":"1","formula2":"10"})).unwrap();
    app.begin_edit(Some("50".into()), false);
    let before = app.session.doc().unwrap().wb.clone();
    app.start_agent_task();
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(app.editor.as_ref().unwrap().text, "50");
    assert_eq!(app.agent.task, "Запиши 42 в B1");
    assert!(Arc::ptr_eq(&before, &app.session.doc().unwrap().wb));
    assert!(app.agent.status.contains("Задача агенту не отправлена"));
}

#[test]
fn open_operation_and_disconnection_do_not_save_draft_or_send_task() {
    let (mut app, sent) = draft_app();
    app.begin_edit(Some("draft".into()), false);
    let group = app.session.group_begin("Manual operation").unwrap();
    let uid = app.session.doc().unwrap().uid;
    app.start_agent_task();
    assert!(app.session.in_group(uid));
    assert_eq!(app.editor.as_ref().unwrap().text, "draft");
    assert!(sent.lock().unwrap().is_empty());
    app.session.group_rollback(group).unwrap();
    app.services.agent_send = None;
    app.start_agent_task();
    assert_eq!(app.editor.as_ref().unwrap().text, "draft");
    assert!(app.agent.status.contains("Подключитесь"));
    app.services.agent_send = Some(Box::new(|_| {}));
    app.services.agent_connect = Some(Box::new(|_, _| {}));
    app.start_agent_task();
    assert_eq!(app.editor.as_ref().unwrap().text, "draft");
    assert!(app.agent.status.contains("Подключитесь"));
}

#[test]
fn apply_keeps_proposal_until_matching_success_and_reports_failures() {
    let (mut app, inbox) = app();
    let action = action(&mut app, json!({"cell":"A1", "input":"42"}));
    let proposal = json!({"action":action});
    app.agent.proposal = Some(proposal.clone());
    for result in [json!({"error":{"message":"invalid params"}}), json!({"result":{"applied":false}})] {
        app.apply_agent_proposal(action.clone());
        assert_eq!(app.agent.proposal, Some(proposal.clone()));
        let mut reply = result;
        reply["id"] = json!(format!("agent-{}", app.agent.next_id));
        inbox.lock().unwrap().push(reply);
        app.poll_agent();
        assert_eq!(app.agent.proposal, Some(proposal.clone()));
        assert!(app.agent.last_change.is_none());
        assert!(app.agent.status.contains("Не удалось") || app.agent.status.contains("не изменилась"));
    }
    app.apply_agent_proposal(action.clone());
    let id = format!("agent-{}", app.agent.next_id);
    inbox.lock().unwrap().push(json!({"id":"other", "result":{"applied":true}}));
    app.poll_agent();
    assert_eq!(app.agent.applying.as_ref().unwrap().0, id);
    assert_eq!(app.agent.proposal, Some(proposal));
    inbox.lock().unwrap().push(json!({"id":id, "result":{"applied":true,"result":{"cell":"A1"}}}));
    app.poll_agent();
    assert!(app.agent.proposal.is_none());
    assert_eq!(app.agent.last_change, Some(action));
    assert_eq!(app.agent.status, "Изменения применены: A1.");
}

#[test]
fn show_change_reveals_target_sheet_and_offscreen_cell_without_breaking_undo() {
    let (mut app, _) = app();
    app.run("home.insertSheet", json!({})).unwrap();
    app.run("sheet.activate", json!({"sheet":0})).unwrap();
    let action = action(&mut app, json!({"cell":"Z500", "sheet":1,"input":"42"}));
    assert_eq!(app.agent_apply_reviewed(&action).unwrap()["applied"], true);
    app.agent.last_change = Some(action);
    app.agent.open = true;
    app.show_agent_change().unwrap();
    assert_eq!(app.session.doc().unwrap().wb.active_sheet(), 1);
    assert_eq!(app.session.doc().unwrap().selection.active.a1(), "Z500");
    assert!(app.grid.ensure_visible);
    assert!(!app.agent.open);
    assert_eq!(app.agent_undo_reviewed().unwrap()["undone"], true);
}

#[test]
fn show_change_uses_snapshot_sheet_and_refuses_another_document() {
    let (mut app, _) = app();
    let action = action(&mut app, json!({"cell":"Z500", "input":"42"}));
    app.agent_apply_reviewed(&action).unwrap();
    app.agent.last_change = Some(action);
    app.show_agent_change().unwrap();
    assert_eq!(app.session.doc().unwrap().selection.active.a1(), "Z500");
    app.run("file.new", json!({})).unwrap();
    app.agent.open = true;
    let before = app.session.doc().unwrap().wb.clone();
    let selection = app.session.doc().unwrap().selection.clone();
    assert!(app.show_agent_change().is_err());
    assert!(Arc::ptr_eq(&before, &app.session.doc().unwrap().wb));
    assert_eq!(selection, app.session.doc().unwrap().selection);
    assert!(app.agent.open);
}

#[test]
fn show_changes_button_closes_the_agent_window() {
    use egui_kittest::kittest::Queryable as _;
    let (mut app, _) = app();
    let action = action(&mut app, json!({"cell":"Z500", "input":"42"}));
    app.agent_apply_reviewed(&action).unwrap();
    app.agent.last_change = Some(action);
    app.agent.open = true;
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui_state(|ui, app| show(app, &ui.ctx().clone()), app);
    harness.run_steps(3);
    harness.get_by_label("Показать изменения").click();
    harness.run_steps(3);
    assert!(!harness.state().agent.open);
    assert_eq!(harness.state().session.doc().unwrap().selection.active.a1(), "Z500");
}

#[test]
fn real_no_op_move_keeps_proposal_and_does_not_claim_a_write() {
    let (mut app, inbox) = app();
    let mut action = action(&mut app, json!({"cols":"B:C", "before":1}));
    action["command"] = json!("sheet.moveColumns");
    let proposal = json!({"action":action});
    app.agent.proposal = Some(proposal.clone());
    let before = app.session.doc().unwrap().wb.clone();
    app.apply_agent_proposal(action.clone());
    let result = app.agent_apply_reviewed(&action).unwrap();
    assert_eq!(result["applied"], false);
    inbox.lock().unwrap().push(json!({"id":format!("agent-{}",app.agent.next_id), "result":result}));
    app.poll_agent();
    assert_eq!(app.agent.proposal, Some(proposal));
    assert!(app.agent.status.contains("не изменилась"));
    assert!(Arc::ptr_eq(&before, &app.session.doc().unwrap().wb));
    assert!(app.agent.last_change.is_none());
}
