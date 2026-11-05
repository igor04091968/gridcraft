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
