//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! GridCraft in an `<iframe>` (such as a Nextcloud app). Messages are plain objects with a `type`:
//!
//! - `gridcraft:ready` (to the parent): `{ version }`, the app is listening; send a file now.
//! - `gridcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a workbook picked with File › Open or dropped on the window.
//! - `gridcraft:save` (to the parent): `{ name, bytes }` (a transferred `ArrayBuffer`) for every
//!   file GridCraft would otherwise download: Save and Save As (an .xlsx workbook) and Export
//!   as PDF. The parent stores it and reports failures itself.
//! - `gridcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `gridcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//!
//! Shared workbooks, where several copies of one workbook are edited at once and the parent
//! carries each copy's edits to the others (`gridcraft_engine::collab`):
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
//!
//! Only the parent window on the page's own origin is heard or answered. In host mode the page
//! starts with an empty workbook instead of a sample, and the parent guards leaving the page.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use std::sync::Arc;

use gridcraft_engine::collab::{self, CellChange};
use gridcraft_engine::model::Workbook;
use gridcraft_ui_egui::{Inbox, Services, SheetApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

/// Saving one of these hands the whole workbook to the parent, which stores it: the workbook
/// counts as saved under that name, as it would after Save on the desktop.
const WORKBOOK_EXTS: &[&str] = &["xlsx"];

/// The longest user name taken from `?author=`.
const MAX_AUTHOR_CHARS: usize = 200;

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    /// Names of the files handed to the parent since the last frame.
    saved: Rc<RefCell<Vec<String>>>,
    /// The unsaved-changes state last reported to the parent.
    reported: Rc<Cell<Option<bool>>>,
    /// The parent asked for a shared workbook: the uid of the document that was active then, so
    /// the one the open adds can be told apart from it.
    expect_shared: Rc<Cell<Option<u64>>>,
    /// The shared document (its uid) and the workbook as last reported to the parent.
    shared: Rc<RefCell<Option<(u64, Arc<Workbook>)>>>,
    /// Edits from other copies waiting for the next frame.
    incoming: Rc<RefCell<Vec<CellChange>>>,
    /// The active document's uid as of the last frame (0 for none).
    active_uid: Rc<Cell<u64>>,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to).
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        Some(Self {
            window,
            parent,
            origin,
            saved: Rc::default(),
            reported: Rc::default(),
            expect_shared: Rc::default(),
            shared: Rc::default(),
            incoming: Rc::default(),
            active_uid: Rc::default(),
        })
    }

    /// Send saved and exported files to the parent instead of downloading them, and hear files
    /// from it.
    pub fn connect(&self, services: &mut Services, inbox: Inbox, ctx: egui::Context) {
        self.listen(inbox, ctx);
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = bridge.post_save(name, bytes) {
                log::error!("couldn't hand {name} to the page: {e}");
            }
        }));
    }

    /// `?author=`: the host's name for the user signs new notes and comments.
    pub fn set_author(app: &mut SheetApp, author: &str) {
        let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
        if !author.is_empty() {
            app.session.prefs.user_name = author;
        }
    }

    pub fn post_ready(&self) {
        self.send(&[("type", "gridcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]);
    }

    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "gridcraft:failed".into()), ("error", error.into())]);
    }

    /// After each frame: a workbook handed to the parent this frame counts as saved under the
    /// name it was saved as, and the parent hears when unsaved changes appear or go away.
    pub fn sync(&self, app: &mut SheetApp) {
        self.sync_shared(app);
        let saved = std::mem::take(&mut *self.saved.borrow_mut());
        if let Some(name) = saved.iter().rev().find(|name| is_workbook(name))
            && let Some(doc) = app.session.active_mut()
        {
            doc.saved = doc.wb.clone();
            if doc.path.is_none() {
                doc.title = name.clone();
            }
        }
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.reported.get() != Some(dirty) && self.send(&[("type", "gridcraft:dirty".into()), ("dirty", dirty.into())]) {
            self.reported.set(Some(dirty));
        }
    }

    /// After each frame, for a shared workbook: adopt the document an open added, report this
    /// user's edits, then apply the other copies' (in that order, so an edit made this frame is
    /// on its way before a peer's lands on top of it).
    fn sync_shared(&self, app: &mut SheetApp) {
        self.active_uid.set(app.session.active().map_or(0, |d| d.uid));
        if let Some(before) = self.expect_shared.get()
            && let Some(d) = app.session.active()
            && d.uid != before
        {
            self.expect_shared.set(None);
            let uid = d.uid;
            let name = d.display_title();
            let sheets: Vec<String> = d.wb.sheets.iter().map(|s| s.name.clone()).collect();
            *self.shared.borrow_mut() = Some((uid, d.wb.clone()));
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

    fn post_save(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let message = message(&[("type", "gridcraft:save".into()), ("name", name.as_str().into()), ("bytes", buffer.clone().into())])
            .ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))?;
        self.saved.borrow_mut().push(name);
        Ok(())
    }

    fn listen(&self, inbox: Inbox, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let expect_shared = self.expect_shared.clone();
        let incoming = self.incoming.clone();
        let active_uid = self.active_uid.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            match string(&data, "type").as_deref() {
                Some("gridcraft:open") => {}
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
                    return;
                }
                _ => return,
            }
            if js_sys::Reflect::get(&data, &JsValue::from_str("shared")).ok().and_then(|v| v.as_bool()) == Some(true) {
                expect_shared.set(Some(active_uid.get()));
            }
            let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Book.xlsx".to_string());
            let Some(bytes) = bytes(&data) else { return };
            inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
            ctx.request_repaint();
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn is_workbook(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| WORKBOOK_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
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

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}
