//! Typing in a cell with a drop-down list (list validation): the entries the text points to are
//! suggested under the editor, and the cell gets the entry meant ("flaking" → "A6 - Flaking")
//! instead of the validation refusing what was typed.

use egui::{Event, Key};
use egui_kittest::kittest::Queryable;
use gridcraft_engine::Session;
use gridcraft_engine::core::{CellRef, Value};
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::editor::ListCommit;
use serde_json::json;

type Harness = egui_kittest::Harness<'static, SheetApp>;

const FLAKING: &str = "A6 - Flaking";
const FLOAT: &str = "D2 - All Raw Glass - Float Defects";
const LAMI: &str = "D3 - All Raw Glass - Lami Defects";

/// A1:A9 take one of three entries (a literal list, so no cell on screen shows them).
fn session() -> Session {
    let mut s = Session::new();
    s.new_workbook();
    let list = format!("\"{FLAKING},{FLOAT},{LAMI}\"");
    s.execute("data.validation", json!({"range": "A1:A9", "type": "list", "formula1": list})).unwrap();
    s.execute("selection.set", json!({"cell": "A1"})).unwrap();
    s
}

fn app() -> SheetApp {
    SheetApp::new(session(), Default::default())
}

fn value(app: &SheetApp, cell: &str) -> Value {
    app.session.active().unwrap().wb.active().unwrap().value(CellRef::parse(cell).unwrap())
}

/// Starts typing `text` in the active cell and presses Enter.
fn enter(app: &mut SheetApp, text: &str) -> bool {
    app.begin_edit(Some(text.into()), false);
    app.commit_edit(0, 0, false, false)
}

#[test]
fn the_one_entry_meant_is_entered() {
    for typed in ["flaking", "Flak", "a6", "A6 - FLAKING", "  flaking "] {
        let mut app = app();
        assert!(enter(&mut app, typed), "{typed:?} is accepted");
        assert_eq!(value(&app, "A1"), Value::text(FLAKING), "{typed:?} enters the entry");
    }
    let mut app = app();
    assert!(enter(&mut app, "raw lami"));
    assert_eq!(value(&app, "A1"), Value::text(LAMI));
}

#[test]
fn several_entries_wait_for_one_to_be_picked() {
    let mut app = app();
    assert!(!enter(&mut app, "glass"), "two entries match: nothing is entered yet");
    assert_eq!(value(&app, "A1"), Value::Empty);
    let ed = app.editor.as_mut().expect("still editing");
    assert_eq!(ed.autocomplete, [FLOAT, LAMI], "both are suggested");
    ed.ac_index = 1;
    ed.ac_moved = true;
    assert!(app.commit_edit(0, 0, false, false));
    assert_eq!(value(&app, "A1"), Value::text(LAMI));
}

#[test]
fn text_matching_no_entry_is_still_refused() {
    let mut app = app();
    assert!(!enter(&mut app, "zzz"));
    assert!(app.message.is_some(), "the validation's error shows");
    assert_eq!(value(&app, "A1"), Value::Empty);
}

#[test]
fn formulas_and_cells_without_a_list_are_left_alone() {
    let mut app = app();
    app.begin_edit(Some("=UPPER(\"flaking\")".into()), false);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.autocomplete.iter().all(|s| s != FLAKING), "no entry is suggested for a formula");
    assert_eq!(ed.list_commit(), ListCommit::Typed, "a formula is entered as typed");
    app.cancel_edit();
    app.run("selection.set", json!({"cell": "B1"})).unwrap();
    assert!(enter(&mut app, "flak"));
    assert_eq!(value(&app, "B1"), Value::text("flak"), "no list on B1: the text as typed");
}

fn harness() -> Harness {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SheetApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app(),
    );
    h.run_steps(4);
    h
}

/// One keystroke per frame, as a person types.
fn type_text(h: &mut Harness, text: &str) {
    for c in text.chars() {
        h.input_mut().events.push(Event::Text(c.to_string()));
        h.run_steps(2);
    }
}

#[test]
fn typing_shows_the_entries_and_enter_takes_the_one() {
    let mut h = harness();
    type_text(&mut h, "flak");
    assert!(h.query_by_label(FLAKING).is_some(), "the entry is suggested under the editor");
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(h.state().editor.is_none(), "the edit is committed");
    assert_eq!(value(h.state(), "A1"), Value::text(FLAKING));
}

#[test]
fn arrows_pick_among_several_entries() {
    let mut h = harness();
    type_text(&mut h, "glass");
    assert!(h.query_by_label(FLOAT).is_some() && h.query_by_label(LAMI).is_some(), "both entries are suggested");
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(h.state().editor.is_some(), "Enter alone doesn't choose between two entries");
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(h.state().editor.is_none());
    assert_eq!(value(h.state(), "A1"), Value::text(LAMI));
}
