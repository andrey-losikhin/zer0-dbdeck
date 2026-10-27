use std::{cell::RefCell, rc::Rc};

use relm4::gtk;
use serde::Serialize;
use webkit6::prelude::*;
use webkit6::{UserContentManager, WebView};

const TABULATOR_JS: &str = include_str!("../assets/vendor/tabulator-6.6.1/js/tabulator.min.js");
const GRID_JS: &str = include_str!("../assets/grid/grid.js");
const GRID_LOGIC_JS: &str = include_str!("../assets/grid/logic.js");
const GRID_CSS: &str = include_str!("../assets/grid/grid.css");

const TABULATOR_CSS: &str = include_str!("../assets/vendor/tabulator-6.6.1/css/tabulator.min.css");

#[derive(Clone, Serialize)]
pub struct GridColumn {
    pub title: String,
    pub field: String,
    pub editable: bool,
}

#[derive(Clone, Serialize)]
pub struct GridRow {
    pub index: usize,
    pub values: Vec<Option<String>>,
    pub editable: Vec<bool>,
}

#[derive(Clone, Serialize)]
struct GridPayload {
    row_actions: bool,
    columns: Vec<GridColumn>,
    rows: Vec<GridRow>,
    server_sort: bool,
}

#[derive(Clone)]
pub struct TabulatorGrid {
    pub webview: WebView,
    state: Rc<GridState>,
}

struct GridState {
    ready: RefCell<bool>,
    pending: RefCell<Option<String>>,
    pending_sort: RefCell<Option<String>>,
    pending_theme: RefCell<Option<String>>,
}

impl TabulatorGrid {
    pub fn new<F>(on_message: F) -> Self
    where
        F: Fn(String) + 'static,
    {
        let manager = UserContentManager::new();
        manager.register_script_message_handler("dbdeck", None);
        let webview = WebView::builder()
            .user_content_manager(&manager)
            .hexpand(true)
            .vexpand(true)
            .build();
        let weak_view = webview.downgrade();
        manager.connect_script_message_received(Some("dbdeck"), move |_, value| {
            let Some(json) = value.to_json(0) else { return };
            let json = json.to_string();
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
                return;
            };
            match message.get("type").and_then(serde_json::Value::as_str) {
                Some("clipboardCopy") => {
                    if let (Some(view), Some(text)) = (
                        weak_view.upgrade(),
                        message.get("text").and_then(serde_json::Value::as_str),
                    ) {
                        view.clipboard().set_text(text);
                    }
                }
                Some("clipboardPaste") => {
                    if let Some(view) = weak_view.upgrade() {
                        let weak = view.downgrade();
                        let token = message
                            .get("token")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        view.clipboard().read_text_async(
                            None::<&gtk::gio::Cancellable>,
                            move |result| {
                                if let (Ok(Some(text)), Some(view)) = (result, weak.upgrade()) {
                                    if let Ok(json) = serde_json::to_string(text.as_str()) {
                                        view.evaluate_javascript(
                                            &format!("window.dbdeckPaste({json}, {token});"),
                                            None,
                                            None,
                                            None::<&gtk::gio::Cancellable>,
                                            |_| {},
                                        );
                                    }
                                }
                            },
                        );
                    }
                }
                _ => on_message(json),
            }
        });

        let state = Rc::new(GridState {
            ready: RefCell::new(false),
            pending: RefCell::new(None),
            pending_sort: RefCell::new(None),
            pending_theme: RefCell::new(None),
        });
        let load_state = state.clone();
        let load_view = webview.clone();
        webview.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                *load_state.ready.borrow_mut() = true;
                if let Some(script) = load_state.pending_theme.borrow_mut().take() {
                    load_view.evaluate_javascript(
                        &script,
                        None,
                        None,
                        None::<&gtk::gio::Cancellable>,
                        |_| {},
                    );
                }
                if let Some(script) = load_state.pending.borrow_mut().take() {
                    load_view.evaluate_javascript(
                        &script,
                        None,
                        None,
                        None::<&gtk::gio::Cancellable>,
                        |_| {},
                    );
                }
                if let Some(script) = load_state.pending_sort.borrow_mut().take() {
                    load_view.evaluate_javascript(
                        &script,
                        None,
                        None,
                        None::<&gtk::gio::Cancellable>,
                        |_| {},
                    );
                }
            }
        });
        webview.load_html(&page_html(), Some("file:///"));
        Self { webview, state }
    }

    pub fn set_theme(&self, css: &str) {
        let css = serde_json::to_string(css).expect("CSS is serializable");
        let script = format!(
            "(() => {{ let style = document.getElementById('dbdeck-theme'); if (!style) {{ style = document.createElement('style'); style.id = 'dbdeck-theme'; document.head.appendChild(style); }} style.textContent = {css}; }})()"
        );
        if *self.state.ready.borrow() {
            self.webview.evaluate_javascript(
                &script,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        } else {
            *self.state.pending_theme.borrow_mut() = Some(script);
        }
    }

    pub fn set_data(
        &self,
        columns: &[String],
        rows: &[Vec<Option<String>>],
        editable: bool,
        has_more: bool,
        server_sort: bool,
        read_only_columns: &[String],
        blocked_cells: &[(usize, usize)],
    ) {
        let payload = GridPayload {
            row_actions: editable,
            server_sort,
            columns: columns
                .iter()
                .enumerate()
                .map(|(index, title)| GridColumn {
                    title: escape_html(title),
                    field: format!("c{index}"),
                    editable: editable && !read_only_columns.contains(title),
                })
                .collect(),
            rows: rows
                .iter()
                .enumerate()
                .map(|(index, values)| GridRow {
                    index,
                    values: values.clone(),
                    editable: (0..values.len())
                        .map(|column| !blocked_cells.contains(&(index, column)))
                        .collect(),
                })
                .collect(),
        };
        let Ok(json) = serde_json::to_string(&payload) else {
            return;
        };
        let script = format!("window.dbdeckSetData({json}, {has_more});");
        if *self.state.ready.borrow() {
            self.webview.evaluate_javascript(
                &script,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        } else {
            *self.state.pending.borrow_mut() = Some(script);
        }
    }

    pub fn set_sort(&self, sort: Option<(usize, bool)>) {
        let sort = sort.map(|(column, descending)| serde_json::json!({"field":format!("c{column}"), "dir":if descending {"desc"} else {"asc"}}));
        let script = format!(
            "window.dbdeckSetSort({});",
            serde_json::to_string(&sort).unwrap()
        );
        if *self.state.ready.borrow() {
            self.webview.evaluate_javascript(
                &script,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        } else {
            *self.state.pending_sort.borrow_mut() = Some(script);
        }
    }

    pub fn copy_selection(&self) {
        self.webview.evaluate_javascript(
            "window.dbdeckCopy();",
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    pub fn set_locked(&self, locked: bool) {
        self.webview.set_sensitive(!locked);
        self.webview.evaluate_javascript(
            &format!("window.dbdeckSetLocked({locked});"),
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    pub fn finish_edit(&self, discard: bool) {
        let action = if discard { "discard" } else { "save" };
        self.webview.evaluate_javascript(
            &format!("window.dbdeckFinishEdit(\"{action}\");"),
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    pub fn append_data(
        &self,
        start_index: usize,
        rows: &[Vec<Option<String>>],
        has_more: bool,
        blocked_cells: &[(usize, usize)],
    ) {
        let rows = rows
            .iter()
            .enumerate()
            .map(|(index, values)| GridRow {
                index: start_index + index,
                values: values.clone(),
                editable: (0..values.len())
                    .map(|column| !blocked_cells.contains(&(start_index + index, column)))
                    .collect(),
            })
            .collect::<Vec<_>>();
        let Ok(json) = serde_json::to_string(&rows) else {
            return;
        };
        let script = format!("window.dbdeckAppendData({json}, {has_more});");
        if *self.state.ready.borrow() {
            self.webview.evaluate_javascript(
                &script,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        } else {
            *self.state.pending.borrow_mut() = Some(script);
        }
    }
}

fn page_html() -> String {
    let colors = crate::theme::grid_css(crate::theme::load_mode());
    format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><style>{TABULATOR_CSS}
{GRID_CSS}
{colors}
</style><script>{TABULATOR_JS}</script></head><body><div id="grid"></div><script>
{GRID_LOGIC_JS}
{GRID_JS}
</script></body></html>"##
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_grid_is_local_and_shares_production_assets() {
        let html = page_html();
        assert!(html.contains(GRID_JS));
        assert!(html.contains(TABULATOR_JS));
        assert!(!html.contains("<script src="));
        assert!(html.contains("selectableRangeInitializeDefault: false"));
    }

    #[test]
    #[ignore = "exports the exact production HTML for GTK/WebKit integration tests"]
    fn export_grid_html() {
        std::fs::write("target/grid-test.html", page_html()).unwrap();
    }
    #[test]
    #[ignore = "requires GTK display; tests production Rust bridge and WebKitGTK"]
    fn webkit_grid_interactions() {
        gtk::init().expect("GTK display required");
        let loop_ = gtk::glib::MainLoop::new(None, false);
        let result = Rc::new(RefCell::new(None));
        let edits = Rc::new(RefCell::new(Vec::new()));
        let actions = Rc::new(RefCell::new(Vec::new()));
        let callback_actions = actions.clone();
        let shared = Rc::new(RefCell::new(None::<TabulatorGrid>));
        let callback_grid = shared.clone();
        let callback_result = result.clone();
        let callback_edits = edits.clone();
        let callback_loop = loop_.clone();
        let grid = TabulatorGrid::new(move |message| {
            let message: serde_json::Value = serde_json::from_str(&message).unwrap();
            match message["type"].as_str() {
                Some("ready") => {
                    callback_grid
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .webview
                        .evaluate_javascript(
                            include_str!("../tests/grid_webkit.js"),
                            None,
                            None,
                            None::<&gtk::gio::Cancellable>,
                            |_| {},
                        );
                }
                Some("edit") => callback_edits.borrow_mut().push(message),
                Some("testSnapshot") => {
                    let view = callback_grid.borrow().as_ref().unwrap().webview.clone();
                    let weak = view.downgrade();
                    let name = message["name"].as_str().unwrap().to_owned();
                    view.snapshot(
                        webkit6::SnapshotRegion::Visible,
                        webkit6::SnapshotOptions::NONE,
                        None::<&gtk::gio::Cancellable>,
                        move |result| {
                            result
                                .unwrap()
                                .save_to_png(format!("target/grid-{name}.png"))
                                .unwrap();
                            weak.upgrade().unwrap().evaluate_javascript(
                                "window.dbdeckSnapshotDone=true;",
                                None,
                                None,
                                None::<&gtk::gio::Cancellable>,
                                |_| {},
                            );
                        },
                    );
                }
                Some("testFinishEdit") => callback_grid
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .finish_edit(message["action"] == "discard"),
                Some("editAction") => {
                    if message["action"] == "save" {
                        assert_eq!(
                            callback_edits.borrow().last().unwrap()["value"],
                            "native-save",
                            "edit must arrive before save"
                        );
                    }
                    callback_actions
                        .borrow_mut()
                        .push(message["action"].clone());
                    callback_grid
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .webview
                        .evaluate_javascript(
                            &format!("window.dbdeckLastAction={};", message["action"]),
                            None,
                            None,
                            None::<&gtk::gio::Cancellable>,
                            |_| {},
                        );
                }
                Some("testAppend") => {
                    callback_grid.borrow().as_ref().unwrap().append_data(
                        240,
                        &(240..260)
                            .map(|row| (0..12).map(|col| Some(format!("{row}:{col}"))).collect())
                            .collect::<Vec<_>>(),
                        false,
                        &[],
                    );
                }
                Some("testResult") => {
                    *callback_result.borrow_mut() = Some(message);
                    callback_loop.quit();
                }
                _ => {}
            }
        });
        webkit6::prelude::WebViewExt::settings(&grid.webview)
            .unwrap()
            .set_enable_write_console_messages_to_stdout(true);
        *shared.borrow_mut() = Some(grid.clone());
        let window = gtk::Window::builder()
            .default_width(1000)
            .default_height(600)
            .child(&grid.webview)
            .build();
        window.present();
        grid.set_data(
            &(0..12).map(|i| format!("Column {i}")).collect::<Vec<_>>(),
            &(0..240)
                .map(|row| (0..12).map(|col| Some(format!("{row}:{col}"))).collect())
                .collect::<Vec<_>>(),
            true,
            false,
            true,
            &["Column 0".to_owned()],
            &[(2, 2)],
        );
        let timeout_loop = loop_.clone();
        let timeout =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(45), move || {
                timeout_loop.quit()
            });
        loop_.run();
        if result.borrow().is_some() {
            timeout.remove();
        }
        window.close();
        let report = result.borrow();
        let report = report.as_ref().expect("WebKit test timed out");
        println!("{}", serde_json::to_string_pretty(report).unwrap());
        assert!(
            report["errors"].as_array().unwrap().is_empty(),
            "WebKit failures: {report}"
        );
        assert!(
            edits.borrow().len() > 200,
            "paste must reach Rust staging bridge"
        );
        assert!(edits.borrow().iter().all(|edit| edit["column"] != "c0" && !(edit["row"] == 2 && edit["column"] == "c2")), "blocked cells emitted edits");
    }
}
