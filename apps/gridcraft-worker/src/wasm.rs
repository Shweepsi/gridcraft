//! The Worker's side of [`crate::Shift`]: `wasm-bindgen` exports, JSON strings in and out.

use gridcraft_engine::collab::CellChange;
use wasm_bindgen::prelude::*;

/// The MCP tools (`tools/list`), as JSON.
#[wasm_bindgen]
pub fn tools() -> String {
    crate::tools().to_string()
}

/// One shift's shared workbook (see [`crate::Shift`]).
#[wasm_bindgen(js_name = Shift)]
pub struct JsShift(crate::Shift);

#[wasm_bindgen(js_class = Shift)]
impl JsShift {
    /// Opens `template` (an .xlsx file named `name`) with `cells`, a JSON array of
    /// `{sheet, cell, c}`, on top.
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str, template: &[u8], cells: &str) -> Result<JsShift, JsError> {
        crate::Shift::open(name, template, &parse(cells)?).map(JsShift).map_err(|e| JsError::new(&e))
    }

    /// Another device's cells (a JSON array), applied without being reported back.
    pub fn apply(&mut self, cells: &str) -> Result<(), JsError> {
        self.0.apply(&parse(cells)?).map_err(|e| JsError::new(&e))
    }

    /// One MCP message (a JSON-RPC line) → `{response, changes, structural, lost}` as JSON.
    pub fn handle(&mut self, line: &str) -> Result<String, JsError> {
        let o = self.0.handle(line).map_err(|e| JsError::new(&e))?;
        Ok(serde_json::json!({"response": o.response, "changes": o.changes, "structural": o.structural, "lost": o.lost}).to_string())
    }

    /// Keeps what the last message did.
    pub fn commit(&mut self) -> Result<(), JsError> {
        self.0.commit().map_err(|e| JsError::new(&e))
    }

    /// Takes back what the last message did.
    pub fn rollback(&mut self) -> Result<(), JsError> {
        self.0.rollback().map_err(|e| JsError::new(&e))
    }

    /// The workbook's defined names, `[{name, scope, formula}]` as JSON.
    pub fn names(&self) -> String {
        self.0.names().to_string()
    }
}

fn parse(cells: &str) -> Result<Vec<CellChange>, JsError> {
    serde_json::from_str(cells).map_err(|e| JsError::new(&format!("bad cells: {e}")))
}
