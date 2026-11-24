//! Shared workbooks: several copies of one workbook edited at once, the parent page carrying each
//! copy's edits to the others (`gridcraft_engine::collab`). This is GridCraft's own extension of
//! the host messages (`craft-web-host` knows `ready`, `open`, `save`, `dirty` and `failed`; the
//! page's `open` still arrives through it). It speaks only to a same-origin parent
//! (`?host=parent`), over `postMessage`:
//!
//! - `gridcraft:open` with `shared: true`: the workbook opened is followed cell by cell. Other
//!   workbooks without unsaved changes are closed, so the frame shows this one.
//! - `gridcraft:opened` (to the parent): `{ name, sheets }` once it is the active document;
//!   send its cells now.
//! - `gridcraft:changes` (to the parent): `{ changes }` after every edit of it, whatever made
//!   the edit (typing, paste, fill, undo…): `[{ sheet, cell, c }]`, `c` the cell's content
//!   (`f` formula, `a` array range, `v` constant, `s` style) or `null` for an emptied cell.
//! - `gridcraft:apply` (from the parent): `{ changes }` in the same shape, another copy's edits.
//!   They are recalculated, but are no undo step of this user's, and Undo keeps them.
//! - A shared workbook never has unsaved changes: the parent stores its cells as they change.
//! - `gridcraft:theme` (from the parent): `{ mode }`, `light`, `dark` or `system`, when the
//!   page's own theme changes (`?theme=` gives the one to start with).
//! - `gridcraft:select` (from the parent): `{ range }`, such as `"Suivi!N47:AA47"`: selects it
//!   like Go To (switching sheet if it names one) and brings it to the middle of the view, for
//!   a page that points at a cell of the workbook.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gridcraft_engine::collab::{self, CellChange};
use gridcraft_engine::model::Workbook;
use gridcraft_ui_egui::SheetApp;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

#[derive(Clone)]
pub struct Shared {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// The parent asked for a shared workbook: the uid of the document that was active then, so
    /// the one the open adds can be told apart from it.
    expect_shared: Rc<Cell<Option<u64>>>,
    /// The shared document (its uid) and the workbook as last reported to the parent.
    shared: Rc<RefCell<Option<(u64, Arc<Workbook>)>>>,
    /// Edits from other copies waiting for the next frame.
    incoming: Rc<RefCell<Vec<CellChange>>>,
    /// The active document's uid as of the last frame (0 for none).
    active_uid: Rc<Cell<u64>>,
    /// The theme the parent asked for, waiting for the next frame.
    theme: Rc<RefCell<Option<String>>>,
    /// The range the parent asked to select, waiting for the next frame.
    select: Rc<RefCell<Option<String>>>,
    /// Gridlines and headings to hide on every sheet of a shared workbook as it opens.
    hide_view: Rc<Cell<(bool, bool)>>,
}

impl Shared {
    /// `None` unless the page is framed by a same-origin parent with `?host=parent`.
    pub fn detect() -> Option<Self> {
        let window = web_sys::window()?;
        let query = window.location().search().ok()?;
        let wants_parent = web_sys::UrlSearchParams::new_with_str(&query).ok()?.get("host").as_deref() == Some("parent");
        if !wants_parent {
            return None;
        }
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        // Reading another origin's location throws, which is `None` too.
        if parent.location().origin().ok()? != origin {
            return None;
        }
        Some(Self {
            window,
            parent,
            origin,
            expect_shared: Rc::default(),
            shared: Rc::default(),
            incoming: Rc::default(),
            active_uid: Rc::default(),
            theme: Rc::default(),
            select: Rc::default(),
            hide_view: Rc::default(),
        })
    }

    /// Every sheet of the shared workbooks opened from now on shows no gridlines and/or no
    /// headings. A view setting of this copy only: it is never reported to the other copies, and
    /// View ▸ Gridlines and Headings still turn them back on.
    pub fn hide_on_open(&self, gridlines: bool, headings: bool) {
        self.hide_view.set((gridlines, headings));
    }

    /// Hear the parent's shared-workbook messages.
    pub fn listen(&self, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let expect_shared = self.expect_shared.clone();
        let incoming = self.incoming.clone();
        let active_uid = self.active_uid.clone();
        let theme = self.theme.clone();
        let select = self.select.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            match string(&data, "type").as_deref() {
                // The file itself is `craft-web-host`'s; only whether it is a shared workbook is ours.
                Some("gridcraft:open") => {
                    if js_sys::Reflect::get(&data, &JsValue::from_str("shared")).ok().and_then(|v| v.as_bool()) == Some(true) {
                        expect_shared.set(Some(active_uid.get()));
                    }
                }
                Some("gridcraft:apply") => {
                    let changes = js_sys::Reflect::get(&data, &JsValue::from_str("changes"))
                        .ok()
                        .and_then(|c| js_sys::JSON::stringify(&c).ok())
                        .and_then(|s| s.as_string())
                        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).map_err(|e| log::error!("bad shared edits: {e}")).ok())
                        // One cell this version can't read (written by another one, say) costs that
                        // cell only, not the whole batch with it.
                        .map(|list| {
                            list.into_iter()
                                .filter_map(|c| serde_json::from_value::<CellChange>(c).map_err(|e| log::error!("bad shared edit: {e}")).ok())
                                .collect::<Vec<_>>()
                        });
                    if let Some(changes) = changes {
                        incoming.borrow_mut().extend(changes);
                        ctx.request_repaint();
                    }
                }
                Some("gridcraft:theme") => {
                    if let Some(mode) = string(&data, "mode") {
                        *theme.borrow_mut() = Some(mode);
                        ctx.request_repaint();
                    }
                }
                Some("gridcraft:select") => {
                    if let Some(range) = string(&data, "range").filter(|r| !r.trim().is_empty()) {
                        *select.borrow_mut() = Some(range);
                        ctx.request_repaint();
                    }
                }
                _ => {}
            }
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Before each frame: the theme and the selection the parent asked for since the last one.
    pub fn before_frame(&self, app: &mut SheetApp) {
        if let Some(mode) = self.theme.borrow_mut().take()
            && let Err(e) = app.run("view.theme", serde_json::json!({ "mode": mode }))
        {
            log::warn!("gridcraft:theme {mode}: {e}");
        }
        if let Some(range) = self.select.borrow_mut().take() {
            match app.run("edit.goTo", serde_json::json!({ "reference": range })) {
                Ok(_) => {
                    app.grid.ensure_visible = true;
                    app.grid.reveal_centered = true;
                }
                Err(e) => log::warn!("gridcraft:select {range}: {e}"),
            }
        }
    }

    /// After each frame, for a shared workbook: adopt the document an open added, report this
    /// user's edits, then apply the other copies' (in that order, so an edit made this frame is
    /// on its way before a peer's lands on top of it).
    pub fn sync(&self, app: &mut SheetApp) {
        self.active_uid.set(app.session.active().map_or(0, |d| d.uid));
        if let Some(before) = self.expect_shared.get()
            && let Some(d) = app.session.active()
            && d.uid != before
        {
            self.expect_shared.set(None);
            let uid = d.uid;
            let name = d.display_title();
            let sheets: Vec<String> = d.wb.sheets.iter().map(|s| s.name.clone()).collect();
            let (no_gridlines, no_headings) = self.hide_view.get();
            if let Some(d) = app.session.doc_by_uid_mut(uid) {
                if no_gridlines || no_headings {
                    for s in &mut Arc::make_mut(&mut d.wb).sheets {
                        let s = Arc::make_mut(s);
                        s.show_gridlines &= !no_gridlines;
                        s.show_headings &= !no_headings;
                    }
                }
                *self.shared.borrow_mut() = Some((uid, d.wb.clone()));
            }
            // One shared workbook at a time: what the frame shows is what the parent follows.
            // The previous one is stored already, and nothing with unsaved changes is closed.
            for i in (0..app.session.documents().len()).rev() {
                let d = &app.session.documents()[i];
                if d.uid != uid && !d.is_dirty() {
                    app.session.close_document(i);
                }
            }
            if let Some(i) = app.session.documents().iter().position(|d| d.uid == uid) {
                app.session.set_active(i);
            }
            self.active_uid.set(uid);
            // Import warnings stay in the info bar. As a message box they would catch the first
            // tap or keystroke on the workbook, every time a shift opens it.
            app.message = None;
            let sheets = js_sys::Array::from_iter(sheets.iter().map(|s| JsValue::from_str(s)));
            self.send(&[("type", "gridcraft:opened".into()), ("name", name.as_str().into()), ("sheets", sheets.into())]);
        }

        let mut shared = self.shared.borrow_mut();
        let Some((uid, last)) = shared.as_mut() else { return };
        let Some(d) = app.session.doc_by_uid_mut(*uid) else {
            // Closed by the user: nothing left to follow.
            *shared = None;
            return;
        };
        if !Arc::ptr_eq(last, &d.wb) {
            let changes = collab::diff(last, &d.wb);
            *last = d.wb.clone();
            if !changes.is_empty() {
                self.post_changes(&changes);
            }
        }
        let incoming = std::mem::take(&mut *self.incoming.borrow_mut());
        if !incoming.is_empty() {
            if let Err(e) = collab::apply(d, &incoming) {
                log::error!("couldn't apply {} shared edits: {e}", incoming.len());
            }
            *last = d.wb.clone();
        }
        d.saved = d.wb.clone();
    }

    fn post_changes(&self, changes: &[CellChange]) {
        let Ok(json) = serde_json::to_string(changes) else { return };
        let Ok(value) = js_sys::JSON::parse(&json) else { return };
        self.send(&[("type", "gridcraft:changes".into()), ("changes", value)]);
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}
