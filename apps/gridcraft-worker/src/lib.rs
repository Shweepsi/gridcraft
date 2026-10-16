//! GridCraft in a Cloudflare Worker: one shared workbook (a template plus the cells edited on top
//! of it, see `gridcraft_engine::collab`) driven through the MCP server.
//!
//! The host (the Worker's JavaScript) owns storage and the network. It opens a [`Shift`] from the
//! template's bytes and the cells stored for it, applies the cells other devices write
//! ([`Shift::apply`]) and hands each MCP message to [`Shift::handle`]. A message reports the cells
//! it changed, whether it changed what the other copies would never get (sheets, sizes, merges,
//! conditional formats…) and whether the shared workbook is still open. The host then keeps the
//! edit ([`Shift::commit`]) or takes it back ([`Shift::rollback`]), so an edit it refuses never
//! lingers in this copy.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gridcraft_engine::collab::{self, CellChange};
use gridcraft_engine::model::{LineInfo, Sheet, Workbook};
use gridcraft_engine::{DocState, Session};
use gridcraft_mcp::{Backend, Headless, Server};
use serde_json::{Value, json};

#[cfg(target_arch = "wasm32")]
mod wasm;

const PENDING: &str = "the last message is still to be committed or rolled back";
const LOST: &str = "the shared workbook is no longer open";
const BUSY: &str = "the engine is busy";

/// Undo and redo steps kept. Each holds its own copy of the sheets an edit touched, a few
/// megabytes for a full sheet, and a Worker's isolate has 128 MB: the engine's 100 would fill it
/// after some forty edits.
const HISTORY: usize = 10;

/// The MCP tools, as `tools/list` lists them.
pub fn tools() -> Value {
    Value::Array(gridcraft_mcp::tool_definitions())
}

/// The engine, shared by the MCP server (which owns its backend) and the [`Shift`] that reads the
/// document after each message.
#[derive(Clone)]
struct Engine(Rc<RefCell<Headless>>);

impl Backend for Engine {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.0.try_borrow_mut().map_err(|_| BUSY.to_string())?.call(method, params)
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "a shared workbook in a Cloudflare Worker".to_string()
    }
}

/// What one MCP message did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The JSON-RPC reply; none for a notification.
    pub response: Option<String>,
    /// The cells whose content it changed, for the other devices.
    pub changes: Vec<CellChange>,
    /// It changed something cell changes don't carry: sheets, names, sizes, merges, conditional
    /// formats, validations, comments, objects or protection.
    pub structural: bool,
    /// The shared workbook was closed (`file.close`).
    pub lost: bool,
}

/// One shift's shared workbook, as this copy has it.
pub struct Shift {
    engine: Engine,
    server: Server,
    /// The shared document among the session's documents.
    uid: u64,
    /// The workbook as last committed: what the other devices have.
    last: Arc<Workbook>,
    /// The document before the last message, until it is committed or rolled back.
    before: Option<DocState>,
}

impl Shift {
    /// Opens `template` (an .xlsx file named `name`) with `cells`, the cells stored for the shift,
    /// on top.
    pub fn open(name: &str, template: &[u8], cells: &[CellChange]) -> Result<Shift, String> {
        let (wb, _warnings) = gridcraft_engine::io::open_bytes(name, template).map_err(|e| e.to_string())?;
        let mut doc = DocState::new(wb, None, name.to_string());
        collab::apply(&mut doc, cells).map_err(|e| e.to_string())?;
        doc.saved = doc.wb.clone();
        let uid = doc.uid;
        let last = doc.wb.clone();
        let mut session = Session::new();
        session.add_document(doc);
        let engine = Engine(Rc::new(RefCell::new(Headless::with_session(session))));
        let server = Server::new(Box::new(engine.clone()));
        Ok(Shift { engine, server, uid, last, before: None })
    }

    /// Applies cells another device wrote. They aren't reported back by the next message, and
    /// this user's undo history keeps them (`collab::apply`).
    pub fn apply(&mut self, changes: &[CellChange]) -> Result<(), String> {
        if self.before.is_some() {
            return Err(PENDING.to_string());
        }
        if changes.is_empty() {
            return Ok(());
        }
        let mut engine = self.engine.0.try_borrow_mut().map_err(|_| BUSY.to_string())?;
        let d = engine.session.doc_by_uid_mut(self.uid).ok_or_else(|| LOST.to_string())?;
        collab::apply(d, changes).map_err(|e| e.to_string())?;
        d.saved = d.wb.clone();
        self.last = d.wb.clone();
        Ok(())
    }

    /// Handles one MCP message (a JSON-RPC line). Until [`Shift::commit`] or [`Shift::rollback`],
    /// no other message or [`Shift::apply`] is taken.
    pub fn handle(&mut self, line: &str) -> Result<Outcome, String> {
        if self.before.is_some() {
            return Err(PENDING.to_string());
        }
        let before = {
            let engine = self.engine.0.try_borrow().map_err(|_| BUSY.to_string())?;
            engine.session.documents().iter().find(|d| d.uid == self.uid).cloned().ok_or_else(|| LOST.to_string())?
        };
        let response = self.server.handle_line(line);
        let mut engine = self.engine.0.try_borrow_mut().map_err(|_| BUSY.to_string())?;
        keep_only(&mut engine.session, self.uid);
        let outcome = match engine.session.doc_by_uid_mut(self.uid) {
            None => Outcome { response, lost: true, ..Outcome::default() },
            Some(d) if Arc::ptr_eq(&self.last, &d.wb) => Outcome { response, ..Outcome::default() },
            Some(d) => Outcome { changes: collab::diff(&self.last, &d.wb), structural: !same_structure(&self.last, &d.wb), response, lost: false },
        };
        drop(engine);
        self.before = Some(before);
        Ok(outcome)
    }

    /// Keeps what the last message did: the other devices now have it.
    pub fn commit(&mut self) -> Result<(), String> {
        if self.before.is_none() {
            return Ok(());
        }
        let mut engine = self.engine.0.try_borrow_mut().map_err(|_| BUSY.to_string())?;
        let d = engine.session.doc_by_uid_mut(self.uid).ok_or_else(|| LOST.to_string())?;
        d.saved = d.wb.clone();
        trim_history(d);
        self.last = d.wb.clone();
        drop(engine);
        self.before = None;
        Ok(())
    }

    /// Takes back what the last message did, history and selection included.
    pub fn rollback(&mut self) -> Result<(), String> {
        let mut engine = self.engine.0.try_borrow_mut().map_err(|_| BUSY.to_string())?;
        let Some(before) = self.before.take() else { return Ok(()) };
        let s = &mut engine.session;
        while let Some(i) = s.documents().len().checked_sub(1) {
            s.close_document(i);
        }
        s.add_document(before);
        Ok(())
    }

    /// The workbook's defined names: `[{name, scope, formula}]`, `scope` null for the workbook's.
    pub fn names(&self) -> Value {
        Value::Array(self.last.names.iter().map(|n| json!({"name": n.name, "scope": n.scope, "formula": n.formula})).collect())
    }
}

/// Drops the oldest undo and redo steps past [`HISTORY`].
fn trim_history(d: &mut DocState) {
    for steps in [&mut d.undo, &mut d.redo] {
        let extra = steps.len().saturating_sub(HISTORY);
        steps.drain(..extra);
    }
}

/// Closes every document but the shared one (a file a message opened or created) and makes the
/// shared one active again: it is the one every tool works on.
fn keep_only(s: &mut Session, uid: u64) {
    for i in (0..s.documents().len()).rev() {
        if s.documents().get(i).is_some_and(|d| d.uid != uid) {
            s.close_document(i);
        }
    }
    if let Some(i) = s.documents().iter().position(|d| d.uid == uid) {
        s.set_active(i);
    }
}

/// Whether `a` and `b` agree on everything but the cells' contents and the view: what
/// [`collab::diff`] doesn't carry, so the other copies would never see it.
fn same_structure(a: &Workbook, b: &Workbook) -> bool {
    a.names == b.names && a.sheets.len() == b.sheets.len() && a.sheets.iter().zip(&b.sheets).all(|(x, y)| Arc::ptr_eq(x, y) || same_sheet(x, y))
}

fn same_sheet(a: &Sheet, b: &Sheet) -> bool {
    a.name == b.name
        && a.visibility == b.visibility
        && a.tab_color == b.tab_color
        && a.cols == b.cols
        && set_rows(a) == set_rows(b)
        && a.default_col_width == b.default_col_width
        && a.default_row_height == b.default_row_height
        && a.merges == b.merges
        && a.cond_formats == b.cond_formats
        && a.validations == b.validations
        && a.tables == b.tables
        && a.autofilter == b.autofilter
        && a.comments == b.comments
        && a.hyperlinks == b.hyperlinks
        && a.charts == b.charts
        && a.images == b.images
        && a.shapes == b.shapes
        && a.sparklines == b.sparklines
        && a.slicers == b.slicers
        && a.pivots == b.pivots
        && a.cell_pictures == b.cell_pictures
        && a.protection == b.protection
        && a.edit_ranges == b.edit_ranges
}

/// The rows' settings, less automatic heights: each copy works those out from the cells.
fn set_rows(s: &Sheet) -> Vec<(u32, LineInfo)> {
    s.rows
        .iter()
        .filter_map(|(&r, &i)| {
            let i = if i.custom { i } else { LineInfo { size: None, ..i } };
            (i != LineInfo::default()).then_some((r, i))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sheet1: A1 = 1, A2 = A1*2.
    fn template() -> Vec<u8> {
        let mut s = Session::new();
        s.new_workbook();
        s.execute("cell.set", json!({"cell": "A1", "input": "1"})).unwrap();
        s.execute("cell.set", json!({"cell": "A2", "input": "=A1*2"})).unwrap();
        gridcraft_engine::io::save_bytes(&s.doc().unwrap().wb, "t.xlsx").unwrap()
    }

    fn open(cells: &[CellChange]) -> Shift {
        Shift::open("t.xlsx", &template(), cells).unwrap()
    }

    fn call(tool: &str, args: Value) -> String {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string()
    }

    fn number(cell: &str, v: f64) -> CellChange {
        serde_json::from_value(json!({"sheet": "Sheet1", "cell": cell, "c": {"v": {"t": "Number", "v": v}}})).unwrap()
    }

    /// The tool's JSON result, after committing the message.
    fn result(s: &mut Shift, tool: &str, args: Value) -> Value {
        let o = s.handle(&call(tool, args)).unwrap();
        s.commit().unwrap();
        let reply: Value = serde_json::from_str(&o.response.unwrap()).unwrap();
        assert_eq!(reply["result"]["isError"], json!(false), "{reply}");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn read(s: &mut Shift, cell: &str) -> f64 {
        result(s, "read_range", json!({"range": cell}))["values"][0][0].as_f64().unwrap()
    }

    #[test]
    fn stored_cells_are_applied_and_computed() {
        let mut s = open(&[number("A1", 5.0)]);
        assert_eq!(read(&mut s, "A2"), 10.0);
    }

    #[test]
    fn an_edit_reports_its_cells_once() {
        let mut s = open(&[]);
        let o = s.handle(&call("set_cell", json!({"cell": "A1", "input": "7"}))).unwrap();
        assert!(!o.structural && !o.lost);
        // A2's new result isn't a change: every copy computes it.
        assert_eq!(o.changes.len(), 1, "{:?}", o.changes);
        assert_eq!((o.changes[0].sheet.as_str(), o.changes[0].cell.as_str()), ("Sheet1", "A1"));
        s.commit().unwrap();
        let o = s.handle(&call("read_range", json!({"range": "A1:A2"}))).unwrap();
        assert!(o.changes.is_empty());
        s.commit().unwrap();
        assert_eq!(read(&mut s, "A2"), 14.0);
    }

    #[test]
    fn rollback_takes_the_edit_back() {
        let mut s = open(&[]);
        s.handle(&call("set_cell", json!({"cell": "A1", "input": "3"}))).unwrap();
        s.rollback().unwrap();
        assert_eq!(read(&mut s, "A2"), 2.0);
        // Nothing left to undo: the edit is gone from the history too.
        let o = s.handle(&call("undo", json!({}))).unwrap();
        assert!(o.changes.is_empty());
        s.commit().unwrap();
    }

    #[test]
    fn peer_cells_are_not_reported_back() {
        let mut s = open(&[]);
        s.apply(&[number("A1", 4.0)]).unwrap();
        let o = s.handle(&call("read_range", json!({"range": "A2"}))).unwrap();
        assert!(o.changes.is_empty());
        s.commit().unwrap();
        assert_eq!(read(&mut s, "A2"), 8.0);
    }

    #[test]
    fn sheets_and_merges_are_structure() {
        let mut s = open(&[]);
        let o = s.handle(&call("execute_command", json!({"command": "home.insertSheet"}))).unwrap();
        assert!(o.structural);
        s.rollback().unwrap();
        assert_eq!(s.engine.0.borrow().session.documents()[0].wb.sheets.len(), 1);
        let o = s.handle(&call("execute_command", json!({"command": "home.mergeCells", "params": {"range": "C1:D1"}}))).unwrap();
        assert!(o.structural);
        s.rollback().unwrap();
        let o = s.handle(&call("set_cell", json!({"cell": "C1", "input": "x"}))).unwrap();
        assert!(!o.structural);
        s.commit().unwrap();
    }

    #[test]
    fn automatic_row_heights_are_not_structure() {
        let mut s = open(&[]);
        let o = s.handle(&call("set_cell", json!({"cell": "B5", "input": "one\ntwo\nthree"}))).unwrap();
        assert!(!o.structural);
        assert_eq!(o.changes.len(), 1);
        s.commit().unwrap();
    }

    #[test]
    fn closing_the_shared_workbook_is_lost_and_rolled_back() {
        let mut s = open(&[]);
        let o = s.handle(&call("execute_command", json!({"command": "file.close", "params": {"force": true}}))).unwrap();
        assert!(o.lost);
        assert!(s.commit().is_err());
        s.rollback().unwrap();
        assert_eq!(read(&mut s, "A2"), 2.0);
    }

    #[test]
    fn other_files_are_closed() {
        let mut s = open(&[]);
        let o = s.handle(&call("execute_command", json!({"command": "file.new"}))).unwrap();
        assert!(!o.lost && o.changes.is_empty());
        s.commit().unwrap();
        assert_eq!(s.engine.0.borrow().session.documents().len(), 1);
        assert_eq!(read(&mut s, "A2"), 2.0);
    }

    #[test]
    fn history_is_kept_short() {
        let mut s = open(&[]);
        for i in 0..HISTORY + 5 {
            result(&mut s, "set_cell", json!({"cell": "B1", "input": i.to_string()}));
        }
        assert_eq!(s.engine.0.borrow().session.documents()[0].undo.len(), HISTORY);
        // The newest steps are the ones kept.
        result(&mut s, "undo", json!({}));
        assert_eq!(read(&mut s, "B1"), (HISTORY + 3) as f64);
    }

    #[test]
    fn one_message_at_a_time() {
        let mut s = open(&[]);
        s.handle(&call("read_range", json!({"range": "A1"}))).unwrap();
        assert!(s.handle(&call("read_range", json!({"range": "A1"}))).is_err());
        assert!(s.apply(&[number("A1", 2.0)]).is_err());
        s.commit().unwrap();
        s.apply(&[number("A1", 2.0)]).unwrap();
    }

    #[test]
    fn names_are_listed() {
        let mut s = open(&[]);
        result(&mut s, "execute_command", json!({"command": "formulas.defineName", "params": {"name": "Date", "refersTo": "=Sheet1!$A$1"}}));
        assert!(s.names().to_string().contains("Date"));
    }
}
