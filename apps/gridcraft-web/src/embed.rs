//! URL options for a page that embeds GridCraft in a frame (`?host=parent`) and presents it as one
//! piece of its own interface. All are optional and none changes the normal app:
//!
//! - `?titlebar=0`: no title bar (the page shows the file's name itself); same as
//!   `?chrome=notitlebar`. Undo and Redo then sit at the end of the ribbon's tab row.
//! - `?ribbon=collapsed`: start with the tabs row only, as after double-clicking a tab.
//! - `?tabs=home,data,view`: the ribbon tabs to show (contextual ones still appear).
//! - `?theme=light|dark|system`: the page's theme rather than the system's.
//! - `?lang=fr`: the page's language whatever the browser's. Never a CJK one: the web build has no
//!   system CJK fonts, so that interface would render as tofu.
//! - `?formulabar=0`: no formula bar; an operator types values, not formulas.
//! - `?animations=0`: the selection outline jumps instead of gliding.
//! - `?gridlines=0`, `?headings=0`: no gridlines / headings on the shared workbooks it opens.
//!
//! They are applied after the stored preferences, so the page's choice wins on every load.

use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::i18n::Language;

use crate::shared::Shared;

fn param(key: &str) -> Option<String> {
    let query = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&query).ok()?.get(key)
}

fn off(key: &str) -> bool {
    param(key).as_deref() == Some("0")
}

/// Applies the page's options to `app`; `shared` is told which view settings to apply to the
/// workbooks it opens.
pub fn apply(app: &mut SheetApp, shared: Option<&Shared>) {
    if off("titlebar") {
        app.chrome.title_bar = false;
    }
    if param("ribbon").as_deref() == Some("collapsed") {
        app.ui.ribbon_collapsed = true;
    }
    if let Some(tabs) = param("tabs") {
        let shown: Vec<String> = tabs.split(',').map(|t| t.trim().to_lowercase().replace(' ', "")).filter(|t| !t.is_empty()).collect();
        if let Some(first) = gridcraft_ui_egui::ribbon::TABS.iter().find(|t| shown.contains(&t.to_lowercase().replace(' ', ""))) {
            app.ui.ribbon_tab = (*first).to_string();
            app.ui.ribbon_tabs = Some(shown);
        }
    }
    if let Some(mode) = param("theme")
        && let Err(e) = app.run("view.theme", serde_json::json!({ "mode": mode }))
    {
        log::warn!("?theme={mode}: {e}");
    }
    if let Some(language) = param("lang").and_then(|code| Language::parse(&code))
        && !matches!(language, Language::Zh | Language::Ja | Language::Ko)
    {
        app.ui.language = language;
    }
    if off("formulabar") {
        app.session.prefs.formula_bar = false;
    }
    if off("animations") {
        app.ui.animations = false;
    }
    if let Some(shared) = shared {
        shared.hide_on_open(off("gridlines"), off("headings"));
    }
}
