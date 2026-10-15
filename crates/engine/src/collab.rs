//! Cell-level collaboration: what changed between two snapshots of a workbook, and applying the
//! changes another copy of it made.
//!
//! A shared workbook travels as its cells' *content*: the formula text, or the constant for a cell
//! without one, and the cell's style. A formula's cached result is never part of it: every copy
//! computes it itself, so a recalculation is not a change. Changes are keyed by sheet name and A1
//! address, which is what two copies of the same file agree on (style ids and sheet indices are
//! local to each copy).
//!
//! What isn't carried: sheet structure (sheets added, removed or renamed), row heights, column
//! widths, merges, conditional formats, validations and the like. Those come from the file both
//! copies were opened from.

use std::sync::Arc;

use gridcraft_core::{CellRef, RangeRef, Value};
use gridcraft_model::{Cell, Formula, Sheet, Style, StyleId, Workbook};
use serde::{Deserialize, Serialize};

use crate::DocState;

/// A cell's content as it travels between copies of a workbook.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellContent {
    /// Formula text without the leading `=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
    /// Legacy array formula (Ctrl+Shift+Enter) range, with `f`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<String>,
    /// The constant, for a cell without a formula.
    #[serde(default, skip_serializing_if = "Value::is_empty")]
    pub v: Value,
    /// The style, when it isn't the workbook's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<Style>,
}

/// One cell's new content; `c: None` empties it (no value, no formula, default style).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellChange {
    pub sheet: String,
    pub cell: String,
    pub c: Option<CellContent>,
}

fn content(wb: &Workbook, cell: Option<&Cell>) -> Option<CellContent> {
    let cell = cell?;
    let s = (cell.style != StyleId::DEFAULT).then(|| wb.styles.get(cell.style).clone());
    let (f, a, v) = match &cell.formula {
        Some(f) => (Some(f.text.clone()), f.array.map(|r| r.a1()), Value::Empty),
        None => (None, None, cell.value.clone()),
    };
    if f.is_none() && v.is_empty() && s.is_none() {
        return None;
    }
    Some(CellContent { f, a, v, s })
}

fn diff_sheet(old_wb: &Workbook, old: Option<&Sheet>, new_wb: &Workbook, new: &Sheet, out: &mut Vec<CellChange>) {
    let mut push = |at: CellRef, c: Option<CellContent>| out.push(CellChange { sheet: new.name.clone(), cell: at.a1(), c });
    for (at, cell) in new.cells.iter() {
        let before = old.and_then(|o| o.cells.get(at));
        // Style ids keep their meaning from one snapshot of a document to the next (the table
        // only grows), so a cell equal to what it was needs no content built: that is nearly
        // every cell of the sheet, on every edit.
        if before == Some(cell) {
            continue;
        }
        let now = content(new_wb, Some(cell));
        let was = before.and_then(|b| content(old_wb, Some(b)));
        if now != was {
            push(at, now);
        }
    }
    if let Some(old) = old {
        for (at, cell) in old.cells.iter() {
            if new.cells.get(at).is_none() && content(old_wb, Some(cell)).is_some() {
                push(at, None);
            }
        }
    }
}

/// The cells whose content differs between `old` and `new`, two snapshots of one document
/// (style ids are compared as such), sheets matched by name. Sheets shared by both snapshots
/// (the same `Arc`) are skipped without looking at their cells, so the cost is the size of the
/// sheets an edit actually touched.
pub fn diff(old: &Workbook, new: &Workbook) -> Vec<CellChange> {
    let mut out = Vec::new();
    for sheet in &new.sheets {
        let before = old.sheets.iter().find(|s| s.name == sheet.name);
        if before.is_some_and(|b| Arc::ptr_eq(b, sheet)) {
            continue;
        }
        diff_sheet(old, before.map(|b| &**b), new, sheet, &mut out);
    }
    out
}

/// Writes `changes` into `wb` without recalculating. Returns the keys it wrote; changes naming
/// a sheet or cell this workbook doesn't have are skipped.
fn write(wb: &mut Workbook, changes: &[CellChange]) -> Vec<crate::calc::Key> {
    let mut touched = Vec::with_capacity(changes.len());
    for ch in changes {
        let (Some(si), Some(at)) = (wb.sheet_index(&ch.sheet), CellRef::parse(&ch.cell)) else { continue };
        let cell = ch.c.as_ref().map(|c| {
            let style = c.s.clone().map_or(StyleId::DEFAULT, |s| wb.styles.intern(s));
            match &c.f {
                Some(text) => {
                    let mut f = Formula::new(text);
                    f.array = c.a.as_deref().and_then(RangeRef::parse);
                    Cell { value: Value::Empty, formula: Some(Arc::new(f)), style }
                }
                None => Cell { value: c.v.clone(), formula: None, style },
            }
        });
        let Some(sheet) = wb.sheets.get_mut(si) else { continue };
        let sheet = Arc::make_mut(sheet);
        match cell {
            Some(c) => sheet.set_cell(at, c),
            None => {
                sheet.remove_cell(at);
            }
        }
        touched.push((si, at));
    }
    touched
}

/// Applies another copy's changes to `d` and recalculates what depends on them. Unlike a
/// command, this records no undo step: it isn't this user's edit. The undo and redo history is
/// rebased instead, so stepping back undoes this user's own edit and keeps the other's.
pub fn apply(d: &mut DocState, changes: &[CellChange]) -> crate::Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    // A large batch (the copy being opened onto its file, or a peer's sort or row insert) is
    // cheaper as one full recalculation than as a dependency walk from every cell.
    let structural = changes.len() > 2_000;
    crate::cmd::commit(d, |cx| {
        let touched = write(&mut cx.wb, changes);
        for (si, at) in &touched {
            cx.touch(*si, *at);
        }
        // Another copy's edit: it was checked there (protection, array bounds), and refusing it
        // here would leave the two copies disagreeing for good.
        cx.protection_checked = true;
        cx.arrays_checked = true;
        cx.structural = structural;
        Ok(())
    })?;
    for entry in d.undo.iter_mut().chain(d.redo.iter_mut()) {
        write(Arc::make_mut(&mut entry.wb), changes);
    }
    // Restored history steps now hold values computed before these changes.
    d.shared = true;
    d.revision += 1;
    Ok(())
}
