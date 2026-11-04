//! Reviewed actions are checked and executed in the same UI pass on both native and WASM.
use std::sync::Arc;

use gridcraft_engine::model::Workbook;
use serde_json::{Value as Json, json};

use crate::SheetApp;

pub(super) struct AppliedAction {
    doc: u64,
    revision: u64,
    before: Arc<Workbook>,
    after: Arc<Workbook>,
}

impl SheetApp {
    fn agent_context(&self) -> Result<Json, String> {
        let d = self.session.doc().map_err(|e| e.to_string())?;
        if self.editor.is_some() || self.session.in_group(d.uid) {
            return Err("Finish the current edit or action before using the agent.".into());
        }
        Ok(json!({
            "doc": d.uid,
            "revision": d.revision,
            "window": self.session.active_window(),
            "sheet": d.wb.active_sheet(),
            "selection": d.selection,
        }))
    }

    pub(crate) fn agent_snapshot(&mut self, params: &Json) -> Result<Json, String> {
        let context = self.agent_context()?;
        // These two queries and the context run together; navigation cannot split the snapshot.
        let workbook = self.session.run("document.inspect", json!({})).map_err(|e| e.to_string())?;
        let mut read = json!({});
        if let Some(range) = params.get("range").and_then(Json::as_str).filter(|r| !r.trim().is_empty()) {
            read["range"] = json!(range);
        }
        let range = self.session.run("sheet.read", read).map_err(|e| e.to_string())?;
        Ok(json!({"context": context, "workbook": workbook, "range": range}))
    }

    pub(crate) fn agent_apply_reviewed(&mut self, action: &Json) -> Result<Json, String> {
        let context = self.agent_context()?;
        if action.get("context") != Some(&context) {
            return Err("The workbook, sheet or selection changed. Request a new proposal.".into());
        }
        let command = action.get("command").and_then(Json::as_str).ok_or("missing `command`")?;
        if !matches!(
            command,
            "cell.set" | "range.setValues" | "home.bold" | "home.italic" | "home.underline" | "home.mergeCells" | "sheet.moveColumns"
        ) {
            return Err("This command is not allowed for a reviewed agent action.".into());
        }
        let params = action.get("params").filter(|p| p.is_object()).ok_or("action params must be an object")?.clone();
        let before = self.session.doc().map_err(|e| e.to_string())?.wb.clone();
        let result = self.run(command, params)?;
        let d = self.session.doc().map_err(|e| e.to_string())?;
        let applied = !Arc::ptr_eq(&before, &d.wb) && d.undo.last().is_some_and(|step| Arc::ptr_eq(&step.wb, &before));
        if applied {
            self.agent.last_applied = Some(AppliedAction { doc: d.uid, revision: d.revision, before, after: d.wb.clone() });
        }
        Ok(json!({"applied": applied, "result": result}))
    }

    pub(crate) fn agent_undo_reviewed(&mut self) -> Result<Json, String> {
        let applied = self.agent.last_applied.as_ref().ok_or("No applied agent action to undo.")?;
        let d = self.session.doc().map_err(|e| e.to_string())?;
        if self.editor.is_some()
            || self.session.in_group(d.uid)
            || d.uid != applied.doc
            || d.revision != applied.revision
            || !Arc::ptr_eq(&d.wb, &applied.after)
            || !d.undo.last().is_some_and(|step| Arc::ptr_eq(&step.wb, &applied.before))
        {
            return Err("The workbook changed after the agent action. Its Undo is no longer available.".into());
        }
        let result = self.run("edit.undo", json!({"steps": 1}))?;
        self.agent.last_applied = None;
        Ok(json!({"undone": true, "result": result}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, control};

    fn app() -> SheetApp {
        let mut session = gridcraft_engine::Session::new();
        session.execute("file.new", json!({})).unwrap();
        SheetApp::new(session, Services::default())
    }

    fn action(app: &mut SheetApp) -> Json {
        let snapshot = request(app, "agent.snapshot", json!({}));
        json!({"context": snapshot["context"], "command": "cell.set", "params": {"cell": "A1", "input": "42"}})
    }

    fn request(app: &mut SheetApp, method: &str, params: Json) -> Json {
        let control::Outcome::Done(response) = control::handle(app, &egui::Context::default(), method, &params) else {
            panic!("unexpected screenshot")
        };
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn reviewed_action_applies_once_and_undo_preserves_earlier_manual_edit() {
        let mut app = app();
        app.run("cell.set", json!({"cell": "B1", "input": "7"})).unwrap();
        let action = action(&mut app);
        let before = app.session.doc().unwrap().wb.clone();
        assert_eq!(request(&mut app, "agent.applyReviewed", action.clone())["applied"], true);
        assert!(app.agent_apply_reviewed(&action).is_err());
        assert_eq!(request(&mut app, "agent.undoReviewed", json!({}))["undone"], true);
        assert_eq!(*app.session.doc().unwrap().wb, *before);
        assert_eq!(app.session.doc().unwrap().undo.len(), 1);
        assert!(app.agent_undo_reviewed().is_err());
    }

    #[test]
    fn manual_edit_and_manual_undo_invalidate_proposal_even_when_content_returns() {
        let mut app = app();
        let action = action(&mut app);
        app.run("cell.set", json!({"cell": "B1", "input": "7"})).unwrap();
        assert!(app.agent_apply_reviewed(&action).is_err());
        app.run("edit.undo", json!({})).unwrap();
        assert!(app.agent_apply_reviewed(&action).is_err());
    }

    #[test]
    fn navigation_to_selection_sheet_window_or_another_document_rejects_apply() {
        for (command, params) in [
            ("selection.set", json!({"range": "B2"})),
            ("sheet.activate", json!({"sheet": 0})),
            ("view.newWindow", json!({})),
            ("file.new", json!({})),
        ] {
            let mut app = app();
            app.run("home.insertSheet", json!({})).unwrap();
            let action = action(&mut app);
            app.run(command, params).unwrap();
            let before = app.session.doc().unwrap().wb.clone();
            assert!(app.agent_apply_reviewed(&action).is_err(), "{command}");
            assert_eq!(*app.session.doc().unwrap().wb, *before);
        }
    }

    #[test]
    fn agent_undo_never_undoes_later_manual_work_or_another_document() {
        for command in ["cell.set", "edit.undo", "file.new"] {
            let mut app = app();
            let action = action(&mut app);
            app.agent_apply_reviewed(&action).unwrap();
            app.run(command, json!({"cell": "B1", "input": "7"})).unwrap();
            let d = app.session.doc().unwrap();
            let before = d.wb.clone();
            let history = d.undo.len();
            assert!(app.agent_undo_reviewed().is_err(), "{command}");
            let d = app.session.doc().unwrap();
            assert_eq!(*d.wb, *before);
            assert_eq!(d.undo.len(), history);
            if command == "edit.undo" {
                app.run("edit.redo", json!({})).unwrap();
                let before = app.session.doc().unwrap().wb.clone();
                assert!(app.agent_undo_reviewed().is_err());
                assert_eq!(*app.session.doc().unwrap().wb, *before);
            }
        }
    }

    #[test]
    fn selection_navigation_after_apply_still_allows_undo() {
        let mut app = app();
        let action = action(&mut app);
        app.agent_apply_reviewed(&action).unwrap();
        app.run("selection.set", json!({"range": "B2"})).unwrap();
        app.agent_undo_reviewed().unwrap();
    }

    #[test]
    fn no_agent_action_and_failed_command_do_not_offer_manual_undo() {
        let mut app = app();
        app.run("cell.set", json!({"cell": "B1", "input": "7"})).unwrap();
        assert!(app.agent_undo_reviewed().is_err());
        let mut action = action(&mut app);
        action["params"]["cell"] = json!("invalid!");
        assert!(app.agent_apply_reviewed(&action).is_err());
        assert!(app.agent_undo_reviewed().is_err());
        assert_eq!(app.session.doc().unwrap().undo.len(), 1);
    }

    #[test]
    fn missing_context_and_open_undo_group_cannot_apply() {
        let mut app = app();
        let mut action = action(&mut app);
        action.as_object_mut().unwrap().remove("context");
        assert!(app.agent_apply_reviewed(&action).is_err());
        let action = self::action(&mut app);
        let group = app.session.group_begin("Manual action").unwrap();
        assert!(app.agent_apply_reviewed(&action).is_err());
        assert!(app.agent_snapshot(&json!({})).is_err());
        app.session.group_rollback(group).unwrap();
    }

    #[test]
    fn unfinished_cell_edit_is_not_committed_by_apply_or_undo() {
        let mut app = app();
        let action = action(&mut app);
        request(&mut app, "ui.edit.begin", json!({"text": "draft"}));
        assert!(app.agent_snapshot(&json!({})).is_err());
        assert!(app.agent_apply_reviewed(&action).is_err());
        assert_eq!(app.editor.as_ref().unwrap().text, "draft");
        request(&mut app, "ui.edit.cancel", json!({}));
        app.agent_apply_reviewed(&action).unwrap();
        request(&mut app, "ui.edit.begin", json!({"text": "another draft"}));
        assert!(app.agent_undo_reviewed().is_err());
        assert_eq!(app.editor.as_ref().unwrap().text, "another draft");
    }
}
