use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

use relm4::gtk;
use serde::Serialize;
use uuid::Uuid;
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
    generation: Uuid,
    row_actions: bool,
    columns: Vec<GridColumn>,
    hidden_columns: Vec<usize>,
    rows: Vec<GridRow>,
    server_sort: bool,
}

#[derive(Clone)]
pub struct TabulatorGrid {
    pub webview: WebView,
    state: Rc<GridState>,
}

struct GridState {
    generation: Cell<Option<Uuid>>,
    ready: RefCell<bool>,
    pending: RefCell<Option<String>>,
    pending_sort: RefCell<Option<String>>,
    pending_theme: RefCell<Option<String>>,
    column_chooser: RefCell<Option<Rc<ColumnChooser>>>,
    view_change: RefCell<Option<ViewChange>>,
}

struct ViewChange {
    request: Uuid,
    completed: Box<dyn FnOnce(bool)>,
}

impl GridState {
    fn finish_view_change(&self, request: Uuid, ready: bool) {
        let action = self
            .view_change
            .borrow_mut()
            .take_if(|action| action.request == request);
        if let Some(action) = action {
            (action.completed)(ready);
        }
    }
}

struct ColumnChooser {
    button: gtk::MenuButton,
    list: gtk::Box,
    show_all: gtk::Button,
    columns: RefCell<Vec<gtk::CheckButton>>,
    hidden_titles: RefCell<HashSet<String>>,
    resetting: Cell<bool>,
}

impl ColumnChooser {
    fn hidden_columns(&self) -> Vec<usize> {
        self.columns
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(index, check)| (!check.is_active()).then_some(index))
            .collect()
    }

    fn update_controls(&self) {
        let columns = self.columns.borrow();
        let visible = columns.iter().filter(|check| check.is_active()).count();
        for check in columns.iter() {
            check.set_sensitive(visible > 1 || !check.is_active());
        }
        self.button.set_sensitive(columns.len() > 1);
        self.show_all.set_sensitive(visible < columns.len());
    }
}

impl TabulatorGrid {
    pub fn new<F>(cell_preview: bool, on_message: F) -> Self
    where
        F: Fn(String) + 'static,
    {
        Self::with_relations(cell_preview, false, on_message)
    }

    pub fn with_relations<F>(cell_preview: bool, row_relations: bool, on_message: F) -> Self
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
        let state = Rc::new(GridState {
            generation: Cell::new(None),
            ready: RefCell::new(false),
            pending: RefCell::new(None),
            pending_sort: RefCell::new(None),
            pending_theme: RefCell::new(None),
            column_chooser: RefCell::new(None),
            view_change: RefCell::new(None),
        });
        let message_state = Rc::downgrade(&state);
        manager.connect_script_message_received(Some("dbdeck"), move |_, value| {
            let Some(json) = value.to_json(0) else { return };
            let json = json.to_string();
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
                return;
            };
            match message.get("type").and_then(serde_json::Value::as_str) {
                Some("viewChangeReady") => {
                    if let Some(state) = message_state.upgrade()
                        && let Some(request) = message["request"]
                            .as_str()
                            .and_then(|id| Uuid::parse_str(id).ok())
                    {
                        state.finish_view_change(request, message["ready"].as_bool() == Some(true));
                    }
                }
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

        let load_state = state.clone();
        webview.connect_load_changed(move |load_view, event| {
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
                if let Some(chooser) = load_state.column_chooser.borrow().as_ref() {
                    Self::send_column_visibility(load_view, &load_state, chooser);
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
        webview.load_html(&page_html(cell_preview, row_relations), Some("file:///"));
        Self { webview, state }
    }

    pub fn column_chooser(&self) -> gtk::MenuButton {
        if let Some(chooser) = self.state.column_chooser.borrow().as_ref() {
            return chooser.button.clone();
        }
        let button = gtk::MenuButton::builder()
            .label("Столбцы")
            .tooltip_text("Выбрать видимые столбцы таблицы")
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(280)
            .child(&list)
            .build();
        let show_all = gtk::Button::with_label("Показать все");
        show_all.add_css_class("flat");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.append(&scroll);
        content.append(&show_all);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        button.set_popover(Some(&popover));
        let chooser = Rc::new(ColumnChooser {
            button: button.clone(),
            list,
            show_all,
            columns: RefCell::new(Vec::new()),
            hidden_titles: RefCell::new(HashSet::new()),
            resetting: Cell::new(false),
        });
        let weak_chooser = Rc::downgrade(&chooser);
        let weak_state = Rc::downgrade(&self.state);
        let weak_view = self.webview.downgrade();
        chooser.show_all.connect_clicked(move |_| {
            if let (Some(chooser), Some(state), Some(view)) = (
                weak_chooser.upgrade(),
                weak_state.upgrade(),
                weak_view.upgrade(),
            ) {
                chooser.hidden_titles.borrow_mut().clear();
                chooser.resetting.set(true);
                for check in chooser.columns.borrow().iter() {
                    check.set_active(true);
                }
                chooser.resetting.set(false);
                chooser.update_controls();
                Self::send_column_visibility(&view, &state, &chooser);
            }
        });
        *self.state.column_chooser.borrow_mut() = Some(chooser);
        button
    }

    pub fn hidden_column_names(&self) -> Vec<String> {
        let chooser = self.state.column_chooser.borrow();
        let Some(chooser) = chooser.as_ref() else {
            return Vec::new();
        };
        chooser
            .columns
            .borrow()
            .iter()
            .filter(|check| !check.is_active())
            .filter_map(|check| check.label().map(|label| label.to_string()))
            .collect()
    }

    /// Set the visibility used by the next grid payload, without updating the old rows.
    pub fn restore_column_visibility(&self, hidden: Vec<String>) {
        let chooser = self.state.column_chooser.borrow();
        let Some(chooser) = chooser.as_ref() else {
            return;
        };
        *chooser.hidden_titles.borrow_mut() = hidden.into_iter().collect();
        for check in chooser.columns.borrow_mut().drain(..) {
            chooser.list.remove(&check);
        }
    }

    fn configure_column_chooser(&self, chooser: &Rc<ColumnChooser>, titles: &[String]) {
        let mut columns = chooser.columns.borrow_mut();
        if columns.len() == titles.len()
            && columns
                .iter()
                .zip(titles)
                .all(|(check, title)| check.label().as_deref() == Some(title.as_str()))
        {
            return;
        }
        for check in columns.drain(..) {
            chooser.list.remove(&check);
        }
        let hidden_titles = chooser.hidden_titles.borrow();
        let needs_visible_column = titles.iter().all(|title| hidden_titles.contains(title));
        for (index, title) in titles.iter().enumerate() {
            let check = gtk::CheckButton::with_label(title);
            check
                .set_active(!hidden_titles.contains(title) || (needs_visible_column && index == 0));
            let weak_chooser = Rc::downgrade(chooser);
            let weak_state = Rc::downgrade(&self.state);
            let weak_view = self.webview.downgrade();
            let title = title.clone();
            check.connect_toggled(move |check| {
                if let (Some(chooser), Some(state), Some(view)) = (
                    weak_chooser.upgrade(),
                    weak_state.upgrade(),
                    weak_view.upgrade(),
                ) && !chooser.resetting.get()
                {
                    if check.is_active() {
                        chooser.hidden_titles.borrow_mut().remove(&title);
                    } else {
                        chooser.hidden_titles.borrow_mut().insert(title.clone());
                    }
                    chooser.update_controls();
                    Self::send_column_visibility(&view, &state, &chooser);
                }
            });
            chooser.list.append(&check);
            columns.push(check);
        }
        drop(columns);
        chooser.update_controls();
    }

    fn send_column_visibility(view: &WebView, state: &GridState, chooser: &ColumnChooser) {
        if *state.ready.borrow()
            && let Some(generation) = state.generation.get()
        {
            let hidden = serde_json::to_string(&chooser.hidden_columns()).unwrap();
            view.evaluate_javascript(
                &format!("window.dbdeckSetHiddenColumns({hidden}, \"{generation}\");"),
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        }
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
        let generation = Uuid::new_v4();
        let hidden_columns =
            self.state
                .column_chooser
                .borrow()
                .as_ref()
                .map_or_else(Vec::new, |chooser| {
                    self.configure_column_chooser(chooser, columns);
                    chooser.hidden_columns()
                });
        let payload = GridPayload {
            generation,
            row_actions: editable,
            server_sort,
            hidden_columns,
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
        self.state.generation.set(Some(generation));
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

    pub fn generation(&self) -> Option<Uuid> {
        self.state.generation.get()
    }

    pub fn reset_page_request(&self) {
        self.webview.evaluate_javascript(
            "window.dbdeckResetPageRequest();",
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
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

    /// Wait for earlier edit messages from the WebView before locking GTK input.
    pub fn prepare_view_change(&self, completed: impl FnOnce(bool) + 'static) {
        if !self.webview.is_sensitive() || self.state.view_change.borrow().is_some() {
            completed(false);
            return;
        }
        let request = Uuid::new_v4();
        self.state.view_change.replace(Some(ViewChange {
            request,
            completed: Box::new(completed),
        }));
        let weak_state = Rc::downgrade(&self.state);
        self.webview.evaluate_javascript(
            &format!("window.dbdeckPrepareViewChange(\"{request}\");"),
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |result| {
                if result.is_err()
                    && let Some(state) = weak_state.upgrade()
                {
                    state.finish_view_change(request, false);
                }
            },
        );
    }

    pub fn append_data(
        &self,
        start_index: usize,
        rows: &[Vec<Option<String>>],
        has_more: bool,
        blocked_cells: &[(usize, usize)],
    ) {
        let blocked_cells: HashSet<_> = blocked_cells.iter().copied().collect();
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

fn page_html(cell_preview: bool, row_relations: bool) -> String {
    let colors = crate::theme::grid_css(crate::theme::load_mode());
    format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><style>{TABULATOR_CSS}
{GRID_CSS}
{colors}
</style><script>{TABULATOR_JS}</script></head><body><div id="grid"></div><script>
{GRID_LOGIC_JS}
const cellPreview = {cell_preview};
const rowRelations = {row_relations};
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
        let html = page_html(true, true);
        assert!(html.contains(GRID_JS));
        assert!(html.contains(TABULATOR_JS));
        assert!(!html.contains("<script src="));
        assert!(html.contains("selectableRangeInitializeDefault: false"));
    }

    #[test]
    #[ignore = "exports the exact production HTML for GTK/WebKit integration tests"]
    fn export_grid_html() {
        std::fs::write("target/grid-test.html", page_html(true, true)).unwrap();
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
        let grid = TabulatorGrid::new(true, move |message| {
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
                Some("viewCell") => {
                    assert_eq!(message["row"], 1);
                    assert_eq!(message["column"], "c1");
                    assert_eq!(
                        message["generation"],
                        callback_grid
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .generation()
                            .unwrap()
                            .to_string()
                    );
                    callback_grid
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .webview
                        .evaluate_javascript(
                            "window.dbdeckPreviewReceived = true;",
                            None,
                            None,
                            None::<&gtk::gio::Cancellable>,
                            |_| {},
                        );
                }
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
                Some("testColumns") => {
                    let shared = callback_grid.borrow();
                    let grid = shared.as_ref().unwrap();
                    let chooser = grid.state.column_chooser.borrow();
                    let chooser = chooser.as_ref().unwrap();
                    let popup = chooser.button.popover().unwrap();
                    match message["action"].as_str().unwrap() {
                        "hide" => {
                            chooser.button.popup();
                            assert!(popup.is_visible());
                            let check = chooser.columns.borrow()[2].clone();
                            check.set_active(false);
                            chooser.button.popdown();
                        }
                        "reload" => grid.set_data(
                            &["Column 2".into(), "Column 0".into(), "new".into()],
                            &[vec![Some("hidden".into()), Some("visible".into()), None]],
                            true,
                            false,
                            true,
                            &["Column 0".into()],
                            &[],
                        ),
                        "fallback" | "missing" => {
                            let title = if message["action"] == "fallback" {
                                "Column 2"
                            } else {
                                "Column 0"
                            };
                            grid.set_data(
                                &[title.into()],
                                &vec![vec![Some(title.into())]; 2],
                                false,
                                false,
                                true,
                                &[],
                                &[],
                            );
                            assert!(grid.hidden_column_names().is_empty());
                        }
                        "last" => {
                            let (other, last) = {
                                let columns = chooser.columns.borrow();
                                (columns[2].clone(), columns[1].clone())
                            };
                            other.set_active(false);
                            assert!(!last.is_sensitive());
                            assert!(last.is_active());
                        }
                        "showAll" => chooser.show_all.emit_clicked(),
                        _ => unreachable!(),
                    }
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
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&grid.webview);
        content.append(&grid.column_chooser());
        let window = gtk::Window::builder()
            .default_width(1000)
            .default_height(600)
            .child(&content)
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
