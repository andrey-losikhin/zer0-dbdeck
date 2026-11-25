mod activity;
mod app_instance;
mod background_tasks;
mod catalog_tree;
mod csv;
mod csv_import_ui;
mod csv_input;
use csv::{write_csv_header, write_csv_row};
mod backup_ui;
mod connection_tls_ui;
mod database;
mod database_structure;
mod database_target_ui;
mod db_error;
mod dialogs;
mod icons;
mod loading_bar;
mod markdown_export;
mod namespace_ui;
mod native_backup;
mod noctalia;
mod object_search;
mod output_file;
mod preferences_ui;
mod production_ui;
mod saved_table_views;
mod secret_store;
mod settings;
mod shortcuts;
mod sql_batch;
mod sql_batch_ui;
mod sql_completion;
mod sql_editor;
mod sql_explain;
mod sql_format;
mod sql_history_ui;
mod sql_risk;
mod sql_session;
mod sql_workspace;
mod storage_ui;
mod structure_ui;
mod tab_strip;
mod table_relations_ui;
mod tabulator;
mod theme;
mod transfer;
mod ui_layout;
mod workspace_ui;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use background_tasks::{BackgroundTasks, Outcome, TRANSFER};
use database::{
    CellValue, DbObject, DbPool, FilterLogic, FilterOperator, TableCatalog, TableCellEdit,
    TableData, TableFilter, TableSort, TableViewState,
};
use relm4::adw::prelude::*;
use relm4::{adw, gtk};
use settings::{
    ConnectionProfile, DatabaseKind, EnvironmentTag, ProfileSaveError, SavedViewLayout,
    SettingsStore, TlsMode,
};
use sourceview5::prelude::*;
use tabulator::TabulatorGrid;
use theme::ThemeMode;
use uuid::Uuid;

#[derive(Debug)]
enum Event {
    VaultAutoUnlock(Result<secret_store::Secret, String>),
    KeyringChanged(bool, Result<(), String>),
    GopassImported(Result<ConnectionProfile, String>),
    Tested(u64, u64, Result<(), String>),
    Connected(String, ConnectionProfile, Result<DbPool, String>),
    ServerIcon(String, &'static str),
    /// A DB call proved the connection of this session key is gone. Always sent
    /// before the regular result event of the same call.
    ConnectionLost(String),
    /// Fresh pools for every session key of a profile; applied all at once or not at all.
    Reconnected(String, Result<Vec<(String, DbPool)>, String>),
    TablesLoaded(u64, u64, Result<TableCatalog, String>),
    DatabasesLoaded(u64, u64, Result<Vec<String>, String>),
    NamespaceChoices(String, u64, Result<Vec<String>, String>),
    TableLoaded(
        String,
        String,
        Option<(SavedViewLayout, DbPool)>,
        Result<TableData, String>,
    ),
    TablePageLoaded(String, String, usize, Result<TableData, String>),
    TableEditsSaved(String, Result<u64, String>),
    TableRowChanged(String, String, Result<u64, String>),
    SqlQueryFinished(
        String,
        String,
        Result<database::QueryResult, String>,
        sql_session::TransactionState,
        u128,
        bool,
    ),
    SqlTransactionFinished(String, Result<sql_session::TransactionState, String>),
    SqlCompletionsLoaded(
        String,
        DbPool,
        Result<database::SqlCompletionCatalog, String>,
    ),
    CsvExported(usize, Result<(), String>),
    ExportProgress(usize),
    SqlBatchProgress(usize, usize),
    SqlBatchFinished(PathBuf, Result<sql_batch::Summary, String>),
    CsvImportProgress(u64),
    CsvImported(ConnectionProfile, DbObject, Result<u64, String>),
    RestoreFinished(ConnectionProfile, Result<String, String>),
    DumpProgress(u64),
    DumpFinished(PathBuf, DatabaseKind, Result<String, String>),
}

#[derive(Clone)]
enum ProfileStatus {
    Connecting,
    Verified,
    Connected,
    /// The main pool of the profile lost its connection; tabs stay open.
    Broken,
    Error(String),
}

/// A read interrupted by a lost connection that can be offered again after reconnect.
#[derive(Clone, Debug)]
enum RetryAction {
    Sql {
        tab_id: String,
        dialect: sql_risk::Dialect,
        needed_approval: bool,
        in_transaction: bool,
    },
    LoadTable(String),
    ExpandNode(ConnectionProfile),
}

impl RetryAction {
    fn id(&self) -> String {
        match self {
            Self::Sql { tab_id, .. } => format!("sql:{tab_id}"),
            Self::LoadTable(key) => format!("table:{key}"),
            Self::ExpandNode(profile) => format!("node:{}", profile_session_key(profile)),
        }
    }
}

/// PostgreSQL editor SQL is never replayed. MySQL keeps its existing retry policy.
fn retry_allowed(action: &RetryAction) -> bool {
    match action {
        RetryAction::Sql {
            dialect,
            needed_approval,
            in_transaction,
            ..
        } => *dialect == sql_risk::Dialect::MySql && !needed_approval && !in_transaction,
        RetryAction::LoadTable(_) | RetryAction::ExpandNode(_) => true,
    }
}

const IDLE_CHECK_AFTER: Duration = Duration::from_secs(60);
const BROKEN_ICON_NAME: &str = "connection-broken-icon";

fn idle_check_due(last: std::time::Instant, now: std::time::Instant) -> bool {
    now.saturating_duration_since(last) > IDLE_CHECK_AFTER
}

struct Ui {
    connection_banner: adw::Banner,
    broken_sessions: RefCell<HashSet<String>>,
    reconnecting: RefCell<HashSet<String>>,
    banner_profile: RefCell<Option<String>>,
    pending_retry: RefCell<HashMap<String, Vec<RetryAction>>>,
    last_active: RefCell<HashMap<String, std::time::Instant>>,
    recovered_at: RefCell<HashMap<String, std::time::Instant>>,
    remember_unlock_button: gtk::Button,
    unlock_provider: gtk::ComboBoxText,
    storage_path_label: gtk::Label,
    transfer_cancel: RefCell<Option<Arc<transfer::Cancellation>>>,
    background_tasks: BackgroundTasks,
    sql_directory_label: gtk::Label,
    csv_directory_label: gtk::Label,
    unlock_busy: Cell<bool>,
    keyring_busy: Cell<bool>,
    after_auto_unlock: RefCell<Option<Box<dyn FnOnce()>>>,
    window: adw::ApplicationWindow,
    active_theme: Rc<Cell<ThemeMode>>,
    preferences: adw::Window,
    application_menu: gtk::MenuButton,
    sidebar_stack: gtk::Stack,
    sidebar_toggle: gtk::ToggleButton,
    layout: Rc<RefCell<ui_layout::Layout>>,
    connections_nav: gtk::Button,
    tables_nav: gtk::Button,
    tables_hint: gtk::Label,
    profiles: gtk::Box,
    profile_search: gtk::SearchEntry,
    collapsed_environments: RefCell<HashSet<&'static str>>,
    objects: gtk::Box,
    table_filter: gtk::SearchEntry,
    object_search_branches: RefCell<Vec<object_search::Branch>>,
    system_schemas_toggle: gtk::CheckButton,
    /// Set while the toggle is changed programmatically, so `toggled` does not save it.
    system_schemas_syncing: Cell<bool>,
    namespace_row: gtk::Box,
    namespace_label: gtk::Label,
    namespace_selector: gtk::DropDown,
    namespace_spinner: adw::Spinner,
    namespace_state: RefCell<namespace_ui::State>,
    table_list_items: RefCell<Vec<(gtk::Button, DbObject)>>,
    landing: gtk::Box,
    empty_session: gtk::Box,
    empty_session_hint: gtk::Label,
    empty_session_sql: gtk::Button,
    active_header: gtk::Box,
    active_details: gtk::Label,
    session_tabs: gtk::Notebook,
    tab_strip: tab_strip::TabStrip,
    connection_block: tab_strip::ConnectionBlock,
    table_tabs: gtk::Stack,
    session_table_tabs: RefCell<HashMap<String, gtk::Notebook>>,
    workspace_groups: RefCell<HashMap<String, workspace_ui::Group>>,
    active_group_by_session: RefCell<HashMap<String, String>>,
    status: gtk::Label,
    active: gtk::Label,
    test_feedback: RefCell<Option<gtk::Label>>,
    last_test: RefCell<Option<Result<(), String>>>,
    profile_status: RefCell<HashMap<String, ProfileStatus>>,
    server_icons: RefCell<HashMap<String, &'static str>>,
    store: RefCell<Option<SettingsStore>>,
    sql_queries_directory: RefCell<Option<PathBuf>>,
    current_profile: RefCell<Option<ConnectionProfile>>,
    sessions: RefCell<HashMap<String, (ConnectionProfile, DbPool)>>,
    catalog_tree: RefCell<catalog_tree::State>,
    pending_database_opens: RefCell<HashSet<String>>,
    desired_database_session: RefCell<Option<String>>,
    connect_requests: RefCell<HashMap<String, String>>,
    table_grids: RefCell<HashMap<String, gtk::Grid>>,
    tabulator_grids: RefCell<HashMap<String, TabulatorGrid>>,
    table_profiles: RefCell<HashMap<String, ConnectionProfile>>,
    table_objects: RefCell<HashMap<String, DbObject>>,
    table_view_states: RefCell<HashMap<String, TableViewState>>,
    table_filter_panels: RefCell<HashMap<String, Rc<TableFilterPanel>>>,
    active_table_by_session: RefCell<HashMap<String, String>>,
    table_requests: RefCell<HashMap<String, String>>,
    table_more_loading: RefCell<HashSet<String>>,
    table_data: RefCell<HashMap<String, Arc<TableData>>>,
    table_edit_states: RefCell<HashMap<String, Rc<TableEditState>>>,
    table_spinners: RefCell<HashMap<String, adw::Spinner>>,
    sql_tabs: RefCell<HashMap<String, SqlTab>>,
    sql_draft_timers: RefCell<HashMap<String, gtk::glib::SourceId>>,
    restoring_drafts: Cell<bool>,
    selected_table_button: RefCell<Option<gtk::Button>>,
    objects_available: Cell<bool>,
    tab_switching: Cell<bool>,
    pool: RefCell<Option<DbPool>>,
    sender: mpsc::Sender<Event>,
    events: Mutex<mpsc::Receiver<Event>>,
    runtime: Arc<tokio::runtime::Runtime>,
    connection_generation: Cell<u64>,
    view_generation: Cell<u64>,
    test_generation: Cell<u64>,
    activity: activity::ActivityCounter,
}

struct SqlTab {
    session_key: String,
    group_key: String,
    schema: Option<String>,
    profile_id: String,
    session: sql_session::SqlSession,
    running_plan: Option<sql_session::SqlSession>,
    transaction_state: sql_session::TransactionState,
    transaction_label: gtk::Label,
    auto_commit: gtk::Switch,
    transaction_buttons: Vec<gtk::Button>,
    actions_menu: gtk::MenuButton,
    format_button: gtk::Button,
    page: gtk::Box,
    buffer: sourceview5::Buffer,
    editor: sourceview5::View,
    grid: gtk::Grid,
    tabulator: TabulatorGrid,
    status: gtk::Label,
    export_button: gtk::Button,
    copy_button: gtk::Button,
    result_stack: gtk::Stack,
    plan_view: gtk::TextView,
    completion_provider: sql_editor::SqlProvider,
    diagnostics: Rc<sql_editor::Diagnostics>,
    style_schemes: sourceview5::StyleSchemeManager,
    risk_dialect: sql_risk::Dialect,
    style_manager: adw::StyleManager,
    style_notify: Option<gtk::glib::SignalHandlerId>,
    result: Option<Arc<TableData>>,
    executed_sql: Option<String>,
    read_only_columns: Vec<String>,
    request_id: String,
    run_button: gtk::Button,
    stop_button: gtk::Button,
    timeout: gtk::SpinButton,
    title: gtk::Label,
    query_name: String,
    saved_path: Option<PathBuf>,
    risk_indicator: gtk::Label,
    error_message: gtk::Label,
    spinner: adw::Spinner,
    started: Option<std::time::Instant>,
    elapsed_timer: Option<gtk::glib::SourceId>,
    /// Whether the last sent statement needed write approval (such SQL is never retried).
    sent_needing_approval: bool,
    /// Last status-bar summary "N строк · мс · подключение"; reshown when the tab is selected.
    last_status: Option<String>,
}

impl SqlTab {
    fn result_focus_widget(&self) -> Option<&gtk::Widget> {
        match self.result_stack.visible_child_name().as_deref() {
            Some("plan") => Some(self.plan_view.upcast_ref()),
            Some("data") if self.tabulator.webview.is_sensitive() => {
                Some(self.tabulator.webview.upcast_ref())
            }
            _ => None,
        }
    }
}

struct FilterRowWidgets {
    row: gtk::Box,
    column: gtk::ComboBoxText,
    operator: gtk::ComboBoxText,
    logic: gtk::ComboBoxText,
    value: gtk::Entry,
    remove: gtk::Button,
}

struct TableFilterPanel {
    toggle: gtk::ToggleButton,
    summary: gtk::Label,
    rows: gtk::Box,
    controls: RefCell<Vec<FilterRowWidgets>>,
    columns: RefCell<Vec<String>>,
    add: gtk::Button,
    apply: gtk::Button,
    clear: gtk::Button,
    sql_mode: gtk::ToggleButton,
    raw: gtk::Entry,
    logic_width: gtk::SizeGroup,
    column_width: gtk::SizeGroup,
    operator_width: gtk::SizeGroup,
    row_height: gtk::SizeGroup,
}

impl TableFilterPanel {
    fn set_summary(&self, summary: &str) {
        self.summary.set_text(summary);
        self.summary.set_visible(!summary.is_empty());
        self.toggle
            .set_tooltip_text((!summary.is_empty()).then_some(summary));
    }
}

struct TableEditState {
    pending: RefCell<HashMap<(usize, usize), Option<String>>>,
    bulk_column: RefCell<Option<(usize, Option<String>)>>,
    selected_column: Cell<Option<usize>>,
    selected_cell: Cell<Option<(usize, usize)>>,
    selection_anchor: Cell<Option<(usize, usize)>>,
    selection_extent: Cell<Option<(usize, usize)>>,
    copied_bounds: Cell<Option<((usize, usize), (usize, usize))>>,
    editing_cell: Cell<Option<(usize, usize)>>,
    editing_column: Cell<Option<usize>>,
    bulk_allowed: Cell<bool>,
    saving: Cell<bool>,
    grid_editing: Cell<bool>,
    cell_edit_allowed: Cell<bool>,
    toolbar: gtk::Box,
    save_button: gtk::Button,
    discard_button: gtk::Button,
    bulk_button: gtk::Button,
    status: gtk::Label,
    /// Always visible: "N строк · есть ещё · мс".
    summary: gtk::Label,
    add_row_button: gtk::Button,
    load_started: Cell<Option<std::time::Instant>>,
    load_ms: Cell<Option<u128>>,
    loaded_view: RefCell<Option<TableViewState>>,
}

fn main() {
    let instance = app_instance::AppInstance::current();
    gtk::init().expect("GTK initialization");
    adw::init().expect("libadwaita initialization");
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_icon_theme_name(Some("Adwaita"));
    }
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);

    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Tokio runtime"),
    );
    let app = adw::Application::builder()
        .application_id(instance.app_id())
        .build();
    app.connect_activate(move |app| {
        build_window(app, runtime.clone());
    });
    app.run();
}

fn build_window(app: &adw::Application, runtime: Arc<tokio::runtime::Runtime>) -> Rc<Ui> {
    let layout = Rc::new(RefCell::new(ui_layout::Layout::load()));
    let app_title = app_instance::AppInstance::current().title();
    dialogs::watch_toplevels();
    let (sender, receiver) = mpsc::channel();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(app_title)
        .default_width(1280)
        .default_height(820)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.add_css_class("titlebar");
    header.add_css_class("workspace-header");
    let start_controls = gtk::WindowControls::new(gtk::PackType::Start);
    start_controls.set_valign(gtk::Align::Center);
    header.append(&start_controls);
    let tab_strip = tab_strip::TabStrip::new(app_title);
    header.append(&tab_strip.root);
    // Keep native titlebar gestures without reserving space for a centered title.
    let header_handle = gtk::WindowHandle::new();
    header_handle.set_child(Some(&header));
    let connection_block = tab_strip::ConnectionBlock::new();
    let application_menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Меню приложения (Alt+M)")
        .valign(gtk::Align::Center)
        .build();
    let application_popover = gtk::Popover::new();
    let application_actions = gtk::Box::new(gtk::Orientation::Vertical, 2);
    application_popover.set_child(Some(&application_actions));
    let commands_button = sql_menu_action(
        &application_actions,
        &application_popover,
        "Палитра команд… (Ctrl+Shift+P)",
        "system-search-symbolic",
    );
    let settings_menu_button = sql_menu_action(
        &application_actions,
        &application_popover,
        "Настройки… (Ctrl+,)",
        "preferences-system-symbolic",
    );
    let history_menu_button = sql_menu_action(
        &application_actions,
        &application_popover,
        "История SQL… (Ctrl+Alt+H)",
        "document-open-recent-symbolic",
    );
    let keyboard_help = sql_menu_action(
        &application_actions,
        &application_popover,
        "Сочетания клавиш… (Ctrl+?)",
        "preferences-desktop-keyboard-shortcuts-symbolic",
    );
    application_menu.set_popover(Some(&application_popover));
    header.append(&application_menu);
    let end_controls = gtk::WindowControls::new(gtk::PackType::End);
    end_controls.set_valign(gtk::Align::Center);
    header.append(&end_controls);
    let preferences_ui::PreferencesUi {
        window: preferences,
        export_backup,
        import_backup,
        theme_selector,
        remember_unlock,
        unlock_provider,
        import_gopass,
        storage_path,
        sql_directory,
        sql_directory_label,
        csv_directory,
        csv_directory_label,
    } = preferences_ui::build(&window);
    shortcuts::button(&preferences, &export_backup, "<Control><Shift>e");
    shortcuts::button(&preferences, &import_backup, "<Control>o");
    dialogs::install(&preferences);
    root.append(&header_handle);
    let activity_bar = loading_bar::new();
    root.append(&activity_bar);
    let connection_banner = adw::Banner::new("Соединение потеряно");
    connection_banner.set_use_markup(false);
    connection_banner.set_button_label(Some("Переподключиться"));
    connection_banner.set_revealed(false);
    root.append(&connection_banner);

    let body = gtk::Paned::new(gtk::Orientation::Horizontal);
    body.set_vexpand(true);
    body.set_position(layout.borrow().sidebar_width);
    body.set_resize_start_child(false);
    // Иначе Paned сжимает левую часть и обрезает полосу иконок.
    body.set_shrink_start_child(false);
    let left_area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let navigation_rail = gtk::Box::new(gtk::Orientation::Vertical, 12);
    navigation_rail.set_size_request(54, -1);
    navigation_rail.add_css_class("navigation-rail");
    let connections_nav = navigation_rail_button("database", "Подключения");
    connections_nav.add_css_class("sidebar-nav-button");
    connections_nav.add_css_class("sidebar-nav-active");
    update_navigation_rail_icon(&connections_nav, "database");
    let tables_nav = navigation_rail_button("tables", "Таблицы");
    tables_nav.add_css_class("sidebar-nav-button");
    update_navigation_rail_icon(&tables_nav, "tables");
    let settings_button = navigation_rail_button("settings", "Общие настройки");
    settings_button.add_css_class("sidebar-nav-button");
    update_navigation_rail_icon(&settings_button, "settings");
    navigation_rail.append(&connections_nav);
    navigation_rail.append(&tables_nav);
    navigation_rail.append(&settings_button);
    let sidebar_toggle = gtk::ToggleButton::new();
    sidebar_toggle.set_icon_name("sidebar-show-symbolic");
    sidebar_toggle.set_tooltip_text(Some("Боковая панель (Ctrl+B)"));
    sidebar_toggle.set_widget_name("sidebar-toggle");
    sidebar_toggle.set_active(!layout.borrow().sidebar_collapsed);
    navigation_rail.append(&sidebar_toggle);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let connections_toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let connections_title = gtk::Label::new(Some("Подключения"));
    connections_title.set_xalign(0.0);
    connections_title.set_hexpand(true);
    connections_title.add_css_class("sidebar-title");
    connections_toolbar.append(&connections_title);
    let add_button = gtk::Button::new();
    let add_icon = penpot_icon_image("add");
    add_button.set_child(Some(&add_icon));
    add_button.set_tooltip_text(Some("Создать подключение"));
    add_button.add_css_class("flat");
    add_button.add_css_class("add-connection-button");
    set_penpot_icon_hover(&add_button, &add_icon, "add");
    connections_toolbar.append(&add_button);
    sidebar.append(&connections_toolbar);
    sidebar.add_css_class("sidebar-panel");
    let profile_search = gtk::SearchEntry::new();
    profile_search.set_placeholder_text(Some("Поиск подключений"));
    sidebar.append(&profile_search);
    let profiles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let profile_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&profiles)
        .build();
    sidebar.append(&profile_scroll);
    let objects_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    objects_page.add_css_class("sidebar-panel");
    let tables_hint = gtk::Label::new(Some(
        "Таблицы появятся после подключения к базе данных. Откройте раздел «Подключения» и выберите сервер.",
    ));
    tables_hint.set_widget_name("tables-hint");
    tables_hint.set_wrap(true);
    tables_hint.set_xalign(0.0);
    tables_hint.add_css_class("dim-label");
    objects_page.append(&tables_hint);
    let (namespace_row, namespace_label, namespace_selector, namespace_spinner) =
        namespace_ui::widgets();
    objects_page.append(&namespace_row);
    let table_filter = gtk::SearchEntry::new();
    table_filter.set_placeholder_text(Some("Поиск баз, схем и таблиц"));
    table_filter.set_tooltip_text(Some(
        "Поиск в загруженном каталоге. Раскройте базу, чтобы загрузить её объекты.",
    ));
    table_filter.set_visible(false);
    objects_page.append(&table_filter);
    let system_schemas_toggle = gtk::CheckButton::with_label("Системные схемы");
    system_schemas_toggle.set_widget_name("system-schemas-toggle");
    system_schemas_toggle.set_tooltip_text(Some(
        "Показывать служебные схемы: information_schema, pg_catalog, mysql, sys",
    ));
    table_filter
        .bind_property("visible", &system_schemas_toggle, "visible")
        .sync_create()
        .build();
    objects_page.append(&system_schemas_toggle);
    let objects = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let objects_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&objects)
        .build();
    objects_page.append(&objects_scroll);
    let sidebar_stack = gtk::Stack::new();
    sidebar_stack.add_css_class("sidebar-stack");
    sidebar_stack.set_transition_type(gtk::StackTransitionType::None);
    sidebar_stack.set_hexpand(true);
    sidebar_stack.set_vexpand(true);
    sidebar_stack.add_named(&sidebar, Some("connections"));
    sidebar_stack.add_named(&objects_page, Some("tables"));
    let sidebar_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_content.set_size_request(264, -1);
    sidebar_content.append(&sidebar_stack);
    left_area.append(&navigation_rail);
    left_area.append(&sidebar_content);
    body.set_start_child(Some(&left_area));
    sidebar_content.set_visible(sidebar_toggle.is_active());
    if !sidebar_toggle.is_active() {
        body.set_position(54);
    }
    {
        let body = body.clone();
        let content = sidebar_content.clone();
        let layout = layout.clone();
        sidebar_toggle.connect_toggled(move |toggle| {
            let expanded = toggle.is_active();
            if !expanded {
                layout.borrow_mut().sidebar_width = body.position().max(318);
            }
            layout.borrow_mut().sidebar_collapsed = !expanded;
            content.set_visible(expanded);
            body.set_position(if expanded {
                layout.borrow().sidebar_width
            } else {
                54
            });
        });
    }
    {
        let layout = layout.clone();
        let toggle = sidebar_toggle.clone();
        body.connect_position_notify(move |body| {
            if toggle.is_active() && body.position() >= 318 {
                layout.borrow_mut().sidebar_width = body.position();
            }
        });
    }

    let workspace = gtk::Box::new(gtk::Orientation::Vertical, 4);
    workspace.set_margin_start(12);
    workspace.set_margin_end(14);
    workspace.set_margin_top(4);
    // Pages stay here for selection logic; the header tab strip is the visible tab bar.
    let session_tabs = gtk::Notebook::new();
    session_tabs.set_show_tabs(false);
    session_tabs.set_show_border(false);
    session_tabs.set_visible(false);
    session_tabs.add_css_class("app-header-tabs");
    workspace.append(&session_tabs);
    let active_header = gtk::Box::new(gtk::Orientation::Vertical, 1);
    active_header.set_margin_bottom(2);
    active_header.add_css_class("active-connection-header");
    let active_header_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let active_labels = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let active = gtk::Label::new(Some(""));
    active.set_xalign(0.0);
    active.add_css_class("title-3");
    active_labels.append(&active);
    let active_details = gtk::Label::new(Some(""));
    active_details.set_xalign(0.0);
    active_details.add_css_class("dim-label");
    active_details.add_css_class("connection-details");
    active_labels.append(&active_details);
    active_labels.set_hexpand(true);
    active_header_row.append(&active_labels);
    active_header.append(&active_header_row);
    // Kept for the connection list; the status bar shows this data instead.
    active_header.set_visible(false);

    let table_tabs = gtk::Stack::new();
    table_tabs.set_transition_type(gtk::StackTransitionType::None);
    table_tabs.set_hexpand(true);
    table_tabs.set_vexpand(true);
    table_tabs.set_visible(false);
    let landing = gtk::Box::new(gtk::Orientation::Vertical, 14);
    landing.set_halign(gtk::Align::Fill);
    landing.set_valign(gtk::Align::Center);
    landing.set_hexpand(true);
    landing.set_vexpand(true);
    landing.set_margin_top(30);
    landing.set_margin_bottom(30);
    let icon = gtk::Image::from_icon_name("network-server-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("dim-label");
    let landing_title = gtk::Label::new(Some("Начните с подключения к базе данных"));
    landing_title.add_css_class("title-1");
    landing_title.set_wrap(true);
    landing_title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    landing_title.set_justify(gtk::Justification::Center);
    landing_title.set_max_width_chars(42);
    let landing_description = gtk::Label::new(Some(
        "Создайте профиль PostgreSQL или MySQL/MariaDB, либо выберите сохранённое подключение слева.",
    ));
    landing_description.set_wrap(true);
    landing_description.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    landing_description.set_max_width_chars(54);
    landing_description.set_justify(gtk::Justification::Center);
    landing_description.add_css_class("dim-label");
    let create_connection = gtk::Button::with_label("Создать подключение");
    create_connection.add_css_class("suggested-action");
    create_connection.set_halign(gtk::Align::Center);
    landing.append(&icon);
    landing.append(&landing_title);
    landing.append(&landing_description);
    landing.append(&create_connection);
    workspace.append(&landing);
    let empty_session = gtk::Box::new(gtk::Orientation::Vertical, 8);
    empty_session.set_halign(gtk::Align::Fill);
    empty_session.set_valign(gtk::Align::Center);
    empty_session.set_hexpand(true);
    empty_session.set_vexpand(true);
    empty_session.set_visible(false);
    let empty_session_hint = gtk::Label::new(None);
    empty_session_hint.add_css_class("dim-label");
    empty_session_hint.set_wrap(true);
    empty_session_hint.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    empty_session_hint.set_max_width_chars(54);
    empty_session_hint.set_justify(gtk::Justification::Center);
    let empty_session_sql = gtk::Button::with_label("Новый SQL-запрос");
    empty_session_sql.add_css_class("suggested-action");
    empty_session_sql.set_halign(gtk::Align::Center);
    empty_session.append(&empty_session_hint);
    empty_session.append(&empty_session_sql);
    workspace.append(&empty_session);
    workspace.append(&table_tabs);
    body.set_end_child(Some(&workspace));
    root.append(&body);
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    footer.set_margin_start(14);
    footer.set_margin_end(14);
    footer.set_margin_bottom(6);
    footer.add_css_class("app-footer");
    let status = gtk::Label::new(Some("Готово"));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_width_chars(1);
    footer.append(&dialogs::copyable_label(&status));
    footer.append(&connection_block.root);
    let background_tasks = BackgroundTasks::new();
    footer.append(&background_tasks.button);
    root.append(&footer);
    window.set_content(Some(&root));

    let theme_provider = install_css();
    let active_theme = Rc::new(Cell::new(theme::load_mode()));
    theme_selector.set_active_id(Some(active_theme.get().id()));
    theme::apply_mode(active_theme.get(), &theme_provider);
    {
        let provider = theme_provider.clone();
        let mode = active_theme.clone();
        theme_selector.connect_changed(move |selector| {
            let selected = ThemeMode::from_id(selector.active_id().as_deref());
            mode.set(selected);
            theme::apply_mode(selected, &provider);
            if let Err(error) = theme::save_mode(selected) {
                eprintln!("Не удалось сохранить тему: {error}");
            }
        });
    }
    let (store, settings_error) = match SettingsStore::load() {
        Ok(store) => (Some(store), None),
        Err(error) => {
            status.set_text(&error);
            (None, Some(error))
        }
    };
    let sql_queries_directory = store
        .as_ref()
        .and_then(SettingsStore::sql_queries_directory);
    let ui = Rc::new(Ui {
        remember_unlock_button: remember_unlock,
        unlock_provider,
        storage_path_label: storage_path,
        transfer_cancel: RefCell::new(None),
        background_tasks,
        sql_directory_label,
        csv_directory_label,
        unlock_busy: Cell::new(false),
        keyring_busy: Cell::new(false),
        after_auto_unlock: RefCell::new(None),
        active_theme: active_theme.clone(),
        preferences,
        application_menu,
        window,
        sidebar_stack: sidebar_stack.clone(),
        sidebar_toggle: sidebar_toggle.clone(),
        layout: layout.clone(),
        connections_nav: connections_nav.clone(),
        tables_nav: tables_nav.clone(),
        tables_hint,
        profiles,
        profile_search,
        collapsed_environments: RefCell::new(HashSet::new()),
        objects,
        table_filter,
        object_search_branches: RefCell::new(Vec::new()),
        system_schemas_toggle: system_schemas_toggle.clone(),
        system_schemas_syncing: Cell::new(false),
        namespace_row,
        namespace_label,
        namespace_selector,
        namespace_spinner,
        namespace_state: RefCell::new(namespace_ui::State::default()),
        table_list_items: RefCell::new(Vec::new()),
        landing,
        empty_session,
        empty_session_hint,
        empty_session_sql,
        active_header,
        active_details,
        session_tabs,
        tab_strip,
        connection_block,
        table_tabs,
        session_table_tabs: RefCell::new(HashMap::new()),
        workspace_groups: RefCell::new(HashMap::new()),
        active_group_by_session: RefCell::new(HashMap::new()),
        status,
        active,
        test_feedback: RefCell::new(None),
        last_test: RefCell::new(None),
        profile_status: RefCell::new(HashMap::new()),
        server_icons: RefCell::new(HashMap::new()),
        store: RefCell::new(store),
        sql_queries_directory: RefCell::new(sql_queries_directory),
        current_profile: RefCell::new(None),
        sessions: RefCell::new(HashMap::new()),
        catalog_tree: RefCell::new(catalog_tree::State::default()),
        pending_database_opens: RefCell::new(HashSet::new()),
        desired_database_session: RefCell::new(None),
        connect_requests: RefCell::new(HashMap::new()),
        table_grids: RefCell::new(HashMap::new()),
        tabulator_grids: RefCell::new(HashMap::new()),
        table_profiles: RefCell::new(HashMap::new()),
        table_objects: RefCell::new(HashMap::new()),
        table_view_states: RefCell::new(HashMap::new()),
        table_filter_panels: RefCell::new(HashMap::new()),
        active_table_by_session: RefCell::new(HashMap::new()),
        table_requests: RefCell::new(HashMap::new()),
        table_more_loading: RefCell::new(HashSet::new()),
        table_data: RefCell::new(HashMap::new()),
        table_edit_states: RefCell::new(HashMap::new()),
        table_spinners: RefCell::new(HashMap::new()),
        sql_tabs: RefCell::new(HashMap::new()),
        sql_draft_timers: RefCell::new(HashMap::new()),
        restoring_drafts: Cell::new(false),
        selected_table_button: RefCell::new(None),
        objects_available: Cell::new(false),
        tab_switching: Cell::new(false),
        pool: RefCell::new(None),
        sender,
        events: Mutex::new(receiver),
        runtime,
        connection_generation: Cell::new(0),
        view_generation: Cell::new(0),
        test_generation: Cell::new(0),
        activity: activity::ActivityCounter::default(),
        connection_banner,
        broken_sessions: RefCell::new(HashSet::new()),
        reconnecting: RefCell::new(HashSet::new()),
        banner_profile: RefCell::new(None),
        pending_retry: RefCell::new(HashMap::new()),
        last_active: RefCell::new(HashMap::new()),
        recovered_at: RefCell::new(HashMap::new()),
    });
    tab_strip::bind(&ui);
    let weak = Rc::downgrade(&ui);
    ui.connection_banner.connect_button_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(profile_id) = ui.banner_profile.borrow().clone() else {
            return;
        };
        if profile_has_broken_session(&ui, &profile_id) {
            reconnect_profile(&ui, &profile_id);
        } else {
            run_pending_retries(&ui, &profile_id);
        }
    });
    let weak = Rc::downgrade(&ui);
    body.connect_position_notify(move |_| {
        let weak = weak.clone();
        gtk::glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                for notebook in ui.session_table_tabs.borrow().values() {
                    update_notebook_header_width(notebook);
                }
            }
        });
    });
    let weak = Rc::downgrade(&ui);
    ui.table_filter.connect_search_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            apply_table_list_filter(&ui);
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.profile_search.connect_search_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            refresh_profiles(&ui);
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.table_filter.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            apply_table_list_filter(&ui);
            let first = ui
                .table_list_items
                .borrow()
                .iter()
                .find(|(button, _)| button.is_visible())
                .map(|(button, _)| button.clone());
            if let Some(button) = first {
                button.emit_clicked();
            }
        }
    });
    let weak = Rc::downgrade(&ui);
    settings_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.preferences.present();
        }
    });
    namespace_ui::install(&ui);
    storage_ui::sync_system_schemas(&ui);
    let weak = Rc::downgrade(&ui);
    system_schemas_toggle.connect_toggled(move |toggle| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.system_schemas_syncing.get() {
            return;
        }
        let saved = ui
            .store
            .borrow_mut()
            .as_mut()
            .map(|store| store.set_show_system_schemas(toggle.is_active()));
        if let Some(Err(error)) = saved {
            ui.status.set_text(&error);
            // Keep the checkbox equal to the stored value when saving failed.
            ui.system_schemas_syncing.set(true);
            toggle.set_active(!toggle.is_active());
            ui.system_schemas_syncing.set(false);
        }
        show_table_catalog(&ui);
    });
    let weak = Rc::downgrade(&ui);
    ui.empty_session_sql.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if let Some(group) = active_workspace_group(&ui) {
            create_sql_tab_in_group(&ui, &group);
        } else if let Some(profile) = ui.current_profile.borrow().clone() {
            // No workspace yet: open one for the selected database right away.
            create_sql_tab(&ui, &profile_session_key(&profile));
        }
    });
    refresh_file_location_labels(&ui);
    let weak = Rc::downgrade(&ui);
    sql_directory.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let owner = ui.clone();
            ensure_vault_unlocked(&ui, move || choose_sql_queries_directory(&owner));
        }
    });
    let weak = Rc::downgrade(&ui);
    csv_directory.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let owner = ui.clone();
            ensure_vault_unlocked(&ui, move || choose_csv_directory(&owner));
        }
    });
    refresh_profiles(&ui);
    let weak = Rc::downgrade(&ui);
    connections_nav.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_sidebar(&ui, "connections");
        }
    });
    let weak = Rc::downgrade(&ui);
    tables_nav.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_sidebar(&ui, "tables");
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.session_tabs.connect_switch_page(move |_, child, _| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.tab_switching.replace(true) {
            return;
        }
        let Some(session_key) = child.tooltip_text() else {
            ui.tab_switching.set(false);
            return;
        };
        workspace_ui::select(&ui, session_key.as_str());
        ui.tab_switching.set(false);
    });
    let weak = Rc::downgrade(&ui);
    add_button.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                start_new_connection(&ui);
            }
        }
    });
    create_connection.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            start_new_connection(&ui);
        }
    });
    let weak = Rc::downgrade(&ui);
    export_backup.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let after_unlock = ui.clone();
            ensure_vault_unlocked(&ui, move || choose_backup_export(&after_unlock));
        }
    });
    let weak = Rc::downgrade(&ui);
    import_backup.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let existing_vault = ui
                .store
                .borrow()
                .as_ref()
                .is_some_and(SettingsStore::vault_exists);
            if existing_vault {
                let after_unlock = ui.clone();
                ensure_vault_unlocked(&ui, move || choose_backup_import(&after_unlock));
            } else {
                choose_backup_import(&ui);
            }
        }
    });
    // Keep UI state alive while the GTK application is running. Signal handlers
    // intentionally capture Weak<Ui>, so this event source owns the strong ref.
    let ui_for_events = ui.clone();
    let mut activity_visibility = activity::DelayedVisibility::default();
    gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
        let show_bar =
            activity_visibility.update(ui_for_events.activity.active(), std::time::Instant::now());
        activity_bar.set_visible(show_bar);
        loop {
            let event = ui_for_events.events.lock().expect("event lock").try_recv();
            match event {
                Ok(event) => handle_event(&ui_for_events, event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return gtk::glib::ControlFlow::Break,
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    let weak = Rc::downgrade(&ui);
    theme_selector.connect_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            if ui.active_theme.get() == ThemeMode::Noctalia {
                for tab in ui.sql_tabs.borrow().values() {
                    theme::refresh_sql_schemes(&tab.style_schemes);
                }
            }
            refresh_sql_editor_themes(&ui);
        }
    });
    let weak = Rc::downgrade(&ui);
    adw::StyleManager::default().connect_notify_local(None, move |_, property| {
        if matches!(
            property.name(),
            "dark" | "accent-color" | "accent-color-rgba"
        ) {
            let weak = weak.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(ui) = weak.upgrade()
                    && ui.active_theme.get() == ThemeMode::System
                {
                    refresh_sql_editor_themes(&ui);
                }
            });
        }
    });
    let weak = Rc::downgrade(&ui);
    let provider = theme_provider.clone();
    if let Some(monitor) = noctalia::watch(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.active_theme.get() != ThemeMode::Noctalia {
            return;
        }
        theme::apply_mode(ThemeMode::Noctalia, &provider);
        for tab in ui.sql_tabs.borrow().values() {
            theme::refresh_sql_schemes(&tab.style_schemes);
        }
        refresh_sql_editor_themes(&ui);
    }) {
        // Монитор живёт, пока жив обработчик закрытия окна.
        ui.window.connect_destroy(move |_| {
            monitor.cancel();
        });
    }
    let weak = Rc::downgrade(&ui);
    let palette_ui = Rc::downgrade(&ui);
    commands_button.connect_clicked(move |_| {
        if let Some(ui) = palette_ui.upgrade() {
            shortcuts::palette(&ui);
        }
    });
    let settings_ui = Rc::downgrade(&ui);
    settings_menu_button.connect_clicked(move |_| {
        if let Some(ui) = settings_ui.upgrade() {
            ui.preferences.present();
        }
    });
    let history_ui = Rc::downgrade(&ui);
    history_menu_button.connect_clicked(move |_| {
        if let Some(ui) = history_ui.upgrade() {
            sql_history_ui::show(&ui);
        }
    });
    keyboard_help.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            shortcuts::help(&ui);
        }
    });
    let weak = Rc::downgrade(&ui);
    ui.window.connect_close_request(move |_| {
        if let Some(ui) = weak.upgrade() {
            let pending = ui.sql_tabs.borrow().values().any(|tab| !tab.run_button.is_sensitive() || tab.session.has_pending_transaction());
            if pending {
                show_error_dialog(&ui, "SQL-сессия ещё используется", "Остановите выполнение и завершите транзакции через Commit или Rollback перед выходом.");
                return gtk::glib::Propagation::Stop;
            }
            let profiles: HashSet<_> = ui.sql_tabs.borrow().values().map(|t| t.profile_id.clone()).collect();
            for profile in profiles {
                if let Err(error) = sql_history_ui::save_drafts(&ui, &profile) { show_error_dialog(&ui, "SQL-вкладки не сохранены", &error); return gtk::glib::Propagation::Stop; }
            }
            if let Err(error) = ui.layout.borrow().save() { eprintln!("{error}"); }
            if let Some(cancel) = ui.transfer_cancel.borrow().as_ref() { cancel.request(); }
            let sessions: Vec<_> = ui.sql_tabs.borrow().values().map(|t| t.session.clone()).collect();
            for session in sessions { ui.runtime.spawn(async move { session.close().await; }); }
        }
        gtk::glib::Propagation::Proceed
    });
    shortcuts::install(&ui);
    storage_ui::install(&ui, &import_gopass);
    ui.window.present();
    if let Some(error) = settings_error {
        show_error_dialog(&ui, "Ошибка настроек", &error);
    }
    ui
}

fn refresh_profiles(ui: &Rc<Ui>) {
    refresh_file_location_labels(ui);
    clear_box(&ui.profiles);
    let profiles = ui
        .store
        .borrow()
        .as_ref()
        .map(|s| s.profiles.clone())
        .unwrap_or_default();
    ui.profile_search.set_visible(!profiles.is_empty());
    if profiles.is_empty() {
        let empty = gtk::Label::new(Some("Подключений пока нет\nНажмите «＋ Подключение»"));
        empty.set_wrap(true);
        empty.set_margin_top(18);
        empty.set_margin_bottom(18);
        empty.add_css_class("dim-label");
        ui.profiles.append(&empty);
        return;
    }
    let query = ui.profile_search.text().trim().to_lowercase();
    let matches = |profile: &ConnectionProfile| {
        query.is_empty()
            || profile.name.to_lowercase().contains(&query)
            || profile.host.to_lowercase().contains(&query)
            || profile.database.to_lowercase().contains(&query)
    };
    let mut shown = 0;
    for environment in [
        EnvironmentTag::Prod,
        EnvironmentTag::Stage,
        EnvironmentTag::Local,
    ] {
        let group: Vec<_> = profiles
            .iter()
            .filter(|p| p.environment.label() == environment.label() && matches(p))
            .collect();
        if group.is_empty() {
            continue;
        }
        shown += group.len();
        let label = environment.label();
        // A search shows every match regardless of collapsed groups.
        let collapsed = query.is_empty() && ui.collapsed_environments.borrow().contains(label);
        ui.profiles
            .append(&profile_group_header(ui, label, group.len(), collapsed));
        if !collapsed {
            for profile in group {
                ui.profiles.append(&profile_row(ui, profile));
            }
        }
    }
    if shown == 0 {
        let empty = gtk::Label::new(Some("Ничего не найдено"));
        empty.set_margin_top(18);
        empty.add_css_class("dim-label");
        ui.profiles.append(&empty);
    }
}

fn profile_group_header(
    ui: &Rc<Ui>,
    label: &'static str,
    count: usize,
    collapsed: bool,
) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let arrow = gtk::Image::from_icon_name(if collapsed {
        "pan-end-symbolic"
    } else {
        "pan-down-symbolic"
    });
    arrow.set_pixel_size(12);
    content.append(&arrow);
    let title = gtk::Label::new(Some(label));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    content.append(&title);
    content.append(&gtk::Label::new(Some(&count.to_string())));
    let header = gtk::Button::new();
    header.set_child(Some(&content));
    header.add_css_class("flat");
    header.add_css_class("connection-group-header");
    header.set_tooltip_text(Some(if collapsed {
        "Развернуть группу"
    } else {
        "Свернуть группу"
    }));
    let weak = Rc::downgrade(ui);
    header.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            {
                let mut collapsed = ui.collapsed_environments.borrow_mut();
                if !collapsed.remove(label) {
                    collapsed.insert(label);
                }
            }
            refresh_profiles(&ui);
        }
    });
    header
}

fn profile_row_icon(name: &str, size: i32) -> gtk::Image {
    let icon = penpot_icon_image(name);
    icon.remove_css_class("penpot-icon");
    icon.set_pixel_size(size);
    icon.set_size_request(size, size);
    icon
}

/// Copies the profile with its stored password and opens the copy for editing.
fn duplicate_profile(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    let mut copy = profile.clone();
    copy.id = Uuid::new_v4().to_string();
    copy.name = format!("{} (копия)", profile.name);
    let result = ui.store.borrow_mut().as_mut().map(|store| {
        let password = if profile.gopass_entry.is_some() {
            String::new()
        } else {
            store.password(&profile.id)?
        };
        store
            .save_profile(copy.clone(), &password)
            .map_err(|error| match error {
                ProfileSaveError::Secret(error) | ProfileSaveError::Settings(error) => error,
            })
    });
    match result {
        Some(Ok(())) => {
            ui.status.set_text("Подключение скопировано");
            refresh_profiles(ui);
            show_connection_form(ui, Some(copy));
        }
        Some(Err(error)) => show_error_dialog(ui, "Подключение не скопировано", &error),
        None => {}
    }
}

/// Single click focuses an open connection; double click or Enter opens a closed one.
fn activate_profile_row(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    if is_profile_connected(ui, &profile.id) {
        select_connection_database(ui, profile);
    } else if !matches!(
        ui.profile_status.borrow().get(&profile.id),
        Some(ProfileStatus::Connecting)
    ) {
        connect_profile(ui, profile.clone());
    }
}

fn profile_row(ui: &Rc<Ui>, profile: &ConnectionProfile) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("connection-row");
    card.add_css_class(profile_color_class(&profile.color));
    card.set_focusable(true);
    let database = if profile.database.trim().is_empty() {
        String::new()
    } else {
        format!(" · {}", profile.database)
    };
    card.set_tooltip_text(Some(&format!(
        "{} · {}:{}{database}",
        profile.kind.label(),
        profile.host,
        profile.port
    )));
    if ui
        .current_profile
        .borrow()
        .as_ref()
        .is_some_and(|current| current.id == profile.id)
    {
        card.add_css_class("connection-row-active");
    }
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.add_css_class("connection-row-line");
    let default_brand = match profile.kind {
        DatabaseKind::PostgreSql => "postgresql",
        DatabaseKind::MySql => "mysql",
        DatabaseKind::MariaDb => "mariadb",
    };
    let brand = profile_row_icon(
        ui.server_icons
            .borrow()
            .get(&profile_session_key(profile))
            .copied()
            .unwrap_or(default_brand),
        16,
    );
    top.append(&brand);
    let name = gtk::Label::new(Some(&profile.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("connection-name");
    top.append(&name);
    let profile_state = ui.profile_status.borrow().get(&profile.id).cloned();
    if matches!(profile_state, Some(ProfileStatus::Broken)) {
        let broken = gtk::Image::from_icon_name("network-offline-symbolic");
        broken.set_tooltip_text(Some("Соединение потеряно"));
        top.append(&broken);
    }
    let is_connected = matches!(
        profile_state.as_ref(),
        Some(ProfileStatus::Connected | ProfileStatus::Broken)
    );
    let is_connecting = matches!(profile_state.as_ref(), Some(ProfileStatus::Connecting));
    let edit = gtk::Button::new();
    let edit_icon = profile_row_icon("edit", 18);
    edit.set_child(Some(&edit_icon));
    set_penpot_icon_hover(&edit, &edit_icon, "edit");
    edit.add_css_class("flat");
    edit.add_css_class("connection-action");
    edit.add_css_class("connection-hover-action");
    edit.set_tooltip_text(Some("Изменить подключение"));
    let weak = Rc::downgrade(ui);
    let saved = profile.clone();
    edit.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let after_unlock = ui.clone();
            let profile = saved.clone();
            ensure_vault_unlocked(&ui, move || {
                show_connection_form(&after_unlock, Some(profile))
            });
        }
    });
    shortcuts::button(&card, &edit, "<Alt>e");
    top.append(&edit);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.add_css_class("connection-hover-action");
    backup_ui::install(ui, &card, &actions, profile);
    top.append(&actions);
    if is_connected {
        let disconnect = gtk::Button::new();
        disconnect.set_child(Some(&profile_row_icon("status-connected", 18)));
        disconnect.add_css_class("flat");
        disconnect.add_css_class("connection-action");
        disconnect.add_css_class("connected-action");
        disconnect.set_tooltip_text(Some("Отключить"));
        let weak = Rc::downgrade(ui);
        let profile_id = profile.id.clone();
        disconnect.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                close_profile_sessions(&ui, &profile_id);
            }
        });
        shortcuts::button(&card, &disconnect, "<Alt>Return");
        top.append(&disconnect);
    } else {
        let connect = gtk::Button::new();
        let connect_icon = profile_row_icon("status-idle", 18);
        connect.set_child(Some(&connect_icon));
        set_penpot_icon_hover(&connect, &connect_icon, "status-idle");
        connect.add_css_class("flat");
        connect.add_css_class("connection-action");
        if is_connecting {
            connect.set_tooltip_text(Some("Подключение..."));
            connect.set_sensitive(false);
            let spinner = adw::Spinner::new();
            spinner.set_size_request(18, 18);
            connect.set_child(Some(&spinner));
        } else {
            connect.add_css_class("connection-hover-action");
            connect.set_tooltip_text(Some("Подключить"));
        }
        let weak = Rc::downgrade(ui);
        let saved = profile.clone();
        connect.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                connect_profile(&ui, saved.clone());
            }
        });
        shortcuts::button(&card, &connect, "<Alt>Return");
        top.append(&connect);
    }
    card.append(&top);
    if let Some(state) = profile_state.as_ref() {
        let status = match state {
            ProfileStatus::Verified => {
                Some(("Проверка подключения успешна".to_owned(), "status-success"))
            }
            ProfileStatus::Error(error) => Some((format!("Ошибка: {error}"), "status-error")),
            ProfileStatus::Connecting | ProfileStatus::Connected | ProfileStatus::Broken => None,
        };
        if let Some((text, style)) = status {
            let status = gtk::Label::new(Some(&text));
            status.set_xalign(0.0);
            status.set_wrap(true);
            status.add_css_class("connection-status");
            status.add_css_class(style);
            if matches!(state, ProfileStatus::Error(_)) {
                card.append(&dialogs::copyable_label(&status));
            } else {
                card.append(&status);
            }
        }
    }
    let click = gtk::GestureClick::new();
    click.set_button(1);
    let weak = Rc::downgrade(ui);
    let saved = profile.clone();
    click.connect_released(move |_, presses, _, _| {
        let Some(ui) = weak.upgrade() else { return };
        let connected = is_profile_connected(&ui, &saved.id);
        if (connected && presses == 1) || (!connected && presses == 2) {
            activate_profile_row(&ui, &saved);
        }
    });
    top.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    let saved = profile.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let plain = !state.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SHIFT_MASK,
        );
        if plain
            && matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
            && let Some(ui) = weak.upgrade()
        {
            activate_profile_row(&ui, &saved);
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    card.add_controller(keys);
    card
}

fn is_profile_connected(ui: &Ui, profile_id: &str) -> bool {
    ui.sessions
        .borrow()
        .values()
        .any(|(profile, _)| profile.id == profile_id)
}

fn select_connection_database(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    let mut root = profile.clone();
    root.database.clear();
    let root_key = profile_session_key(&root);
    let source = ui.sessions.borrow().values().find_map(|(candidate, pool)| {
        let mut base = candidate.clone();
        base.database.clear();
        (profile_session_key(&base) == root_key).then(|| pool.clone())
    });
    database_target_ui::navigate(ui, profile, move |ui, target| {
        if let Some(pool) = source.clone() {
            open_database_session(ui, target.clone(), pool, &target.database);
        } else {
            connect_profile(ui, target);
        }
    });
}

fn add_session_tab(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    // Server-only connections provide navigation, never a workspace group.
    if profile.database.trim().is_empty() {
        return;
    }
    workspace_ui::ensure(ui, &profile_session_key(profile), None);
}

fn add_workspace_tab(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    group_key: &str,
    schema: Option<&str>,
) {
    if let Some(index) = session_page_index(ui, group_key) {
        ui.session_tabs.set_current_page(Some(index));
        return;
    }
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_size_request(1, 1);
    page.set_tooltip_text(Some(group_key));
    let database = schema
        .map(|schema| format!("{}.{}", profile.database, schema))
        .unwrap_or_else(|| profile.database.clone());
    let title_text = match schema {
        Some(_) => format!("{} · {}.", profile.name, profile.database),
        None => format!("{} · {database}", profile.name),
    };
    let title = gtk::Label::new(Some(&title_text));
    title.set_tooltip_text(Some(&format!(
        "{} · {}:{} · {}",
        profile.kind.label(),
        profile.host,
        profile.port,
        database
    )));
    title.set_ellipsize(if schema.is_some() {
        gtk::pango::EllipsizeMode::Middle
    } else {
        gtk::pango::EllipsizeMode::End
    });
    title.set_width_chars(title_text.chars().count().min(22) as i32);
    // Keep "db." flush against the highlighted schema name.
    title.set_xalign(if schema.is_some() { 1.0 } else { 0.5 });
    title.set_max_width_chars(32);
    let tab_label = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_label.add_css_class("compact-tab-label");
    tab_label.append(&profile_tab_icon("database", &profile.color));
    let names = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    names.append(&title);
    if let Some(schema) = schema {
        let schema_label = gtk::Label::new(Some(schema));
        schema_label.add_css_class("tab-schema-name");
        names.append(&schema_label);
    }
    tab_label.append(&names);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&close, 14);
    close.add_css_class("flat");
    close.add_css_class("circular");
    close.add_css_class("compact-tab-close");
    close.set_tooltip_text(Some("Закрыть группу вкладок"));
    tab_label.append(&close);
    ui.session_tabs.append_page(&page, Some(&tab_label));
    let weak = Rc::downgrade(ui);
    let group_key = group_key.to_owned();
    close.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            workspace_ui::close(&ui, &group_key);
        }
    });
    ui.session_tabs.set_visible(true);
    let index = ui.session_tabs.page_num(&page);
    ui.session_tabs.set_current_page(index);
}

fn ensure_session_table_tabs(ui: &Rc<Ui>, session_key: &str) -> gtk::Notebook {
    if let Some(notebook) = ui.session_table_tabs.borrow().get(session_key) {
        return notebook.clone();
    }
    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);
    notebook.add_css_class("workspace-tabs");
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    notebook.set_show_border(false);
    notebook.set_show_tabs(false);
    tab_strip::watch_notebook(ui, &notebook);
    let name = session_key.to_owned();
    ui.table_tabs.add_named(&notebook, Some(&name));
    let weak = Rc::downgrade(ui);
    notebook.connect_switch_page(move |notebook, page, _| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.tab_switching.replace(true) {
            return;
        }
        let Some(key) = table_key_from_page(notebook, page) else {
            ui.tab_switching.set(false);
            return;
        };
        let session = workspace_ui::source(&ui, &name);
        note_tab_switch(&ui, &name, &key, &session);
        ui.active_group_by_session
            .borrow_mut()
            .insert(session.clone(), name.clone());
        if key.starts_with("sql::") {
            ui.active_table_by_session
                .borrow_mut()
                .insert(name.clone(), key.clone());
            if let Some((profile, pool)) = ui.sessions.borrow().get(&session).cloned() {
                *ui.current_profile.borrow_mut() = Some(profile);
                *ui.pool.borrow_mut() = Some(pool);
            }
            if let Some(index) = session_page_index(&ui, &name) {
                ui.session_tabs.set_current_page(Some(index));
            }
            ui.table_tabs.set_visible_child_name(&name);
            ui.table_tabs.set_visible(true);
            ui.empty_session.set_visible(false);
            ui.active_header.set_visible(false);
            let connection = ui
                .sessions
                .borrow()
                .get(&session)
                .map(|(profile, _)| profile.name.clone())
                .unwrap_or_default();
            let last = {
                let tabs = ui.sql_tabs.borrow();
                key.strip_prefix("sql::")
                    .and_then(|id| tabs.get(id))
                    .and_then(|tab| tab.last_status.clone())
            };
            ui.status
                .set_text(&last.unwrap_or_else(|| format!("SQL · {connection}")));
            tab_strip::sync_connection(&ui);
            tab_strip::refresh(&ui);
            ui.tab_switching.set(false);
            return;
        }
        select_table_button(&ui, &key);
        ui.active_table_by_session
            .borrow_mut()
            .insert(name.clone(), key);
        if let Some(index) = session_page_index(&ui, &name) {
            ui.session_tabs.set_current_page(Some(index));
        }
        if let Some((profile, pool)) = ui.sessions.borrow().get(&session).cloned() {
            *ui.current_profile.borrow_mut() = Some(profile);
            *ui.pool.borrow_mut() = Some(pool);
        }
        tab_strip::refresh(&ui);
        ui.table_tabs.set_visible_child_name(&name);
        ui.table_tabs.set_visible(true);
        ui.empty_session.set_visible(false);
        ui.tab_switching.set(false);
    });
    ui.session_table_tabs
        .borrow_mut()
        .insert(session_key.to_owned(), notebook.clone());
    compact_notebook_header(&notebook);
    notebook
}

// Notebook's scrollable tab strip has the natural width of one tab. Request
// the combined tab widths, capped at the visible window, so the action follows
// the tabs without losing native overflow/reordering when space runs out.
fn update_notebook_header_width(notebook: &gtk::Notebook) {
    let Some(header) = notebook
        .first_child()
        .filter(|child| child.css_name() == "header")
    else {
        return;
    };
    let Some(root) = notebook
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    else {
        return;
    };
    let Some(bounds) = notebook.compute_bounds(&root) else {
        return;
    };
    let action_width = notebook
        .action_widget(gtk::PackType::End)
        .map_or(0, |button| {
            button.measure(gtk::Orientation::Horizontal, -1).1
        });
    let available = (root.width() - bounds.x().ceil() as i32 - action_width - 20).max(0);
    let desired = (0..notebook.n_pages())
        .filter_map(|index| notebook.nth_page(Some(index)))
        .filter_map(|page| notebook.tab_label(&page))
        .map(|label| {
            let (minimum, natural, _, _) = label
                .parent()
                .unwrap_or(label)
                .measure(gtk::Orientation::Horizontal, -1);
            if minimum > 0 { minimum } else { natural }
        })
        .sum::<i32>()
        + 4;
    let mut child = header.first_child();
    while let Some(widget) = child {
        if widget.css_name() == "tabs" {
            let width = desired.min(available);
            if widget.width_request() != width {
                widget.set_size_request(width, -1);
            }
        }
        child = widget.next_sibling();
    }
}

fn compact_notebook_header(notebook: &gtk::Notebook) {
    if let Some(header) = notebook
        .first_child()
        .filter(|child| child.css_name() == "header")
    {
        header.set_halign(gtk::Align::Start);
    }
    notebook.connect_page_added(|notebook, _, _| update_notebook_header_width(notebook));
    notebook.connect_page_removed(|notebook, _, _| update_notebook_header_width(notebook));
    let connection = Rc::new(RefCell::new(
        None::<(gtk::gdk::Surface, gtk::glib::SignalHandlerId)>,
    ));
    let mapped_connection = connection.clone();
    notebook.connect_map(move |notebook| {
        let weak = notebook.downgrade();
        gtk::glib::idle_add_local_once(move || {
            if let Some(notebook) = weak.upgrade() {
                update_notebook_header_width(&notebook);
            }
        });
        if let Some(surface) = notebook.native().and_then(|native| native.surface()) {
            let weak = notebook.downgrade();
            let id = surface.connect_layout(move |_, _, _| {
                let weak = weak.clone();
                gtk::glib::idle_add_local_once(move || {
                    if let Some(notebook) = weak.upgrade() {
                        update_notebook_header_width(&notebook);
                    }
                });
            });
            *mapped_connection.borrow_mut() = Some((surface, id));
        }
    });
    notebook.connect_unmap(move |notebook| {
        if let Some(tabs) = notebook
            .first_child()
            .and_then(|header| header.first_child())
        {
            if tabs.css_name() == "tabs" {
                tabs.set_size_request(-1, -1);
            }
        }
        if let Some((surface, id)) = connection.borrow_mut().take() {
            surface.disconnect(id);
        }
    });
}

fn sql_menu_action(menu: &gtk::Box, popover: &gtk::Popover, text: &str, icon: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(16);
    row.append(&image);
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);
    let button = gtk::Button::new();
    button.set_child(Some(&row));
    button.add_css_class("flat");
    let weak = popover.downgrade();
    button.connect_clicked(move |_| {
        if let Some(popover) = weak.upgrade() {
            popover.popdown();
        }
    });
    menu.append(&button);
    button
}

fn sql_empty_results() -> gtk::Box {
    let hints = gtk::Box::new(gtk::Orientation::Vertical, 12);
    hints.set_halign(gtk::Align::Center);
    hints.set_valign(gtk::Align::Center);
    hints.add_css_class("sql-shortcut-hints");
    for (text, accelerator) in [
        ("Выполнить SQL", "<Control>Return"),
        ("Автодополнение", "<Control>space"),
        ("Все сочетания клавиш", "<Control><Shift>question"),
    ] {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);
        row.append(&gtk::ShortcutLabel::new(accelerator));
        hints.append(&row);
    }
    hints
}

fn create_sql_tab(ui: &Rc<Ui>, session_key: &str) {
    let source = workspace_ui::source(ui, session_key);
    let group = if source == session_key {
        workspace_ui::active(ui, &source)
    } else {
        session_key.to_owned()
    };
    create_sql_tab_in_group(ui, &group);
}

fn create_sql_tab_in_group(ui: &Rc<Ui>, requested_group: &str) {
    let source = workspace_ui::source(ui, requested_group);
    let session_key = source.as_str();
    let Some((profile, pool)) = ui.sessions.borrow().get(session_key).cloned() else {
        return;
    };
    if profile.database.trim().is_empty() {
        database_target_ui::choose(ui, &profile, move |ui, target| {
            with_database_session(ui, target, pool.clone(), create_sql_tab);
        });
        return;
    }
    // Count disconnected drafts too: the encrypted workspace has one global limit.
    if !ui.restoring_drafts.get() {
        let tabs = ui.sql_tabs.borrow();
        let groups: std::collections::HashSet<_> =
            tabs.values().map(|t| t.group_key.as_str()).collect();
        let saved = ui.store.borrow();
        let inactive = saved
            .as_ref()
            .map(|s| {
                s.sql_workspace()
                    .drafts
                    .iter()
                    .filter(|d| {
                        !groups.contains(
                            workspace_ui::key_from_session(&d.connection_key, d.schema.as_deref())
                                .as_str(),
                        )
                    })
                    .count()
            })
            .unwrap_or(0);
        if tabs.len() + inactive >= sql_workspace::MAX_DRAFTS {
            ui.status
                .set_text("Лимит 32 SQL-вкладки. Закройте ненужную вкладку перед созданием новой.");
            return;
        }
    }
    let risk_dialect = if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
        sql_risk::Dialect::MySql
    } else {
        sql_risk::Dialect::PostgreSql
    };
    let schema = ui
        .workspace_groups
        .borrow()
        .get(requested_group)
        .and_then(|g| g.schema.clone());
    let group_key = workspace_ui::ensure(ui, session_key, schema.as_deref());
    workspace_ui::select(ui, &group_key);
    let schema = ui
        .workspace_groups
        .borrow()
        .get(&group_key)
        .and_then(|g| g.schema.clone());
    let notebook = ensure_session_table_tabs(ui, &group_key);
    let query_number = ui
        .sql_tabs
        .borrow()
        .values()
        .filter(|tab| tab.session_key == session_key)
        .count()
        + 1;
    let id = Uuid::new_v4().to_string();
    let page_key = format!("sql::{id}");

    let manager = sourceview5::LanguageManager::new();
    let buffer = sourceview5::Buffer::new(None);
    if let Some(language) = manager.language("sql") {
        buffer.set_language(Some(&language));
    }
    buffer.set_highlight_syntax(true);
    let style_manager = adw::StyleManager::default();
    let style_schemes = theme::sql_style_schemes();
    apply_sql_style_scheme(
        &buffer,
        &style_schemes,
        ui.active_theme.get(),
        style_manager.is_dark(),
    );
    buffer.set_text("-- Введите SQL и нажмите «Выполнить» (Ctrl+Enter)\n");
    let editor = sourceview5::View::with_buffer(&buffer);
    editor.set_monospace(true);
    editor.set_show_line_numbers(true);
    editor.set_highlight_current_line(true);
    editor.set_auto_indent(true);
    editor.set_tab_width(4);
    editor.set_left_margin(8);
    editor.set_right_margin(8);
    editor.set_top_margin(6);
    editor.set_bottom_margin(6);
    editor.add_css_class("sql-editor");
    editor.connect_unmap(|editor| editor.completion().hide());

    let dialect = match profile.kind {
        DatabaseKind::PostgreSql => sql_editor::Dialect::PostgreSql,
        DatabaseKind::MySql | DatabaseKind::MariaDb => sql_editor::Dialect::MySql,
    };
    let completion_provider = sql_editor::SqlProvider::new(dialect, schema.clone());
    editor.completion().add_provider(&completion_provider);
    editor.completion().set_select_on_show(true);

    let editor_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(160)
        .child(&editor)
        .build();
    let editor_region = gtk::Box::new(gtk::Orientation::Vertical, 0);
    editor_region.add_css_class("sql-editor-region");
    editor_region.append(&editor_scroll);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("sql-toolbar");
    let status = gtk::Label::new(Some("Готово к выполнению"));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_width_chars(1);
    status.add_css_class("dim-label");
    toolbar.append(&dialogs::copyable_label(&status));
    let risk_indicator = gtk::Label::new(None);
    risk_indicator.add_css_class("query-risk-indicator");
    toolbar.append(&risk_indicator);
    let transaction_label = gtk::Label::new(Some("Auto Commit"));
    transaction_label.add_css_class("dim-label");
    transaction_label.set_tooltip_text(Some(
        "Режим транзакций этой SQL-вкладки. Ctrl+Alt+T — переключить.",
    ));
    toolbar.append(&transaction_label);
    let auto_commit = gtk::Switch::new();
    auto_commit.set_widget_name("sql-auto-commit");
    auto_commit.set_active(true);
    auto_commit.set_valign(gtk::Align::Center);
    auto_commit.set_tooltip_text(Some("Auto Commit (Ctrl+Alt+T)"));
    toolbar.append(&auto_commit);
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.add_css_class("sql-actions-popover");
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(6);
    menu.set_margin_end(6);
    popover.set_child(Some(&menu));
    let timeout_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let timeout_label = gtk::Label::new(Some("Таймаут, сек. (0 — без ограничения)"));
    timeout_label.set_hexpand(true);
    timeout_label.set_xalign(0.0);
    let timeout = gtk::SpinButton::with_range(0.0, 3600.0, 5.0);
    timeout.set_value(60.0);
    timeout.set_tooltip_text(Some(
        "Ограничение времени выполнения; применяется к следующему запуску",
    ));
    timeout_row.append(&timeout_label);
    timeout_row.append(&timeout);
    menu.append(&timeout_row);
    let reset_button = sql_menu_action(
        &menu,
        &popover,
        "Сбросить SQL-сессию… (Ctrl+Alt+Shift+R)",
        "view-refresh-symbolic",
    );
    let manual_button = sql_menu_action(
        &menu,
        &popover,
        "Ручной режим / Auto Commit (Ctrl+Alt+T)",
        "emblem-synchronizing-symbolic",
    );
    let commit_button = sql_menu_action(
        &menu,
        &popover,
        "Commit (Ctrl+Alt+Enter)",
        "emblem-ok-symbolic",
    );
    let rollback_button = sql_menu_action(
        &menu,
        &popover,
        "Rollback (Ctrl+Alt+R)",
        "edit-undo-symbolic",
    );
    commit_button.set_sensitive(false);
    rollback_button.set_sensitive(false);
    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let write_button = sql_menu_action(
        &menu,
        &popover,
        "Выполнить с правом записи… (Ctrl+Shift+Enter)",
        "dialog-warning-symbolic",
    );
    write_button.set_sensitive(!profile.read_only);
    let owner = Rc::downgrade(ui);
    let write_id = id.clone();
    write_button.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            run_sql_tab_policy(&ui, &write_id, None, false, SqlRunMode::Write);
        }
    });
    let explain_button = sql_menu_action(
        &menu,
        &popover,
        "План запроса (EXPLAIN, Ctrl+Shift+G)",
        "view-list-symbolic",
    );
    explain_button.set_widget_name("sql-explain");
    let owner = Rc::downgrade(ui);
    let explain_id = id.clone();
    explain_button.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            explain_sql_tab(&ui, &explain_id);
        }
    });

    let open_button = sql_menu_action(&menu, &popover, "Открыть SQL…", "document-open-symbolic");
    let save_button = sql_menu_action(&menu, &popover, "Сохранить SQL", "document-save-symbolic");
    let format_button = sql_menu_action(
        &menu,
        &popover,
        "Форматировать SQL (Ctrl+Shift+F)",
        "document-edit-symbolic",
    );
    format_button.set_widget_name("sql-format");
    let weak = Rc::downgrade(ui);
    let format_editor = editor.clone();
    format_button.connect_clicked(move |_| {
        if let Err(error) = sql_editor::format_buffer(&format_editor, dialect) {
            if let Some(ui) = weak.upgrade() {
                show_error_dialog(&ui, "Не удалось форматировать SQL", &error);
            }
        } else {
            format_editor.grab_focus();
        }
    });
    let rename_button =
        sql_menu_action(&menu, &popover, "Переименовать…", "document-edit-symbolic");
    let directory_button =
        sql_menu_action(&menu, &popover, "Папка запросов…", "folder-open-symbolic");
    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let export_button = sql_menu_action(
        &menu,
        &popover,
        "Экспорт предпросмотра в CSV…",
        "document-save-as-symbolic",
    );
    export_button.set_sensitive(false);
    let full_export_button = sql_menu_action(
        &menu,
        &popover,
        "Выполнить запрос и сохранить полный CSV…",
        "folder-download-symbolic",
    );
    let owner = Rc::downgrade(ui);
    let full_export_id = id.clone();
    full_export_button.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            confirm_full_sql_csv_export(&ui, &full_export_id);
        }
    });
    let batch_button = sql_menu_action(
        &menu,
        &popover,
        "Выполнить в Markdown…",
        "x-office-document-symbolic",
    );
    batch_button.set_tooltip_text(Some(
        "Выполнить весь текст по одному читающему запросу и сохранить результаты и ошибки в .md",
    ));
    let owner = Rc::downgrade(ui);
    let batch_id = id.clone();
    batch_button.connect_clicked(move |_| {
        if let Some(ui) = owner.upgrade() {
            sql_batch_ui::choose_report(&ui, &batch_id);
        }
    });
    let run_button = gtk::Button::with_label("Выполнить");
    run_button.add_css_class("suggested-action");
    run_button.add_css_class("sql-run-button");
    run_button.set_tooltip_text(Some("Выполнить выделенный SQL или весь текст (Ctrl+Enter)"));
    toolbar.append(&run_button);
    let stop_button = gtk::Button::from_icon_name("media-playback-stop-symbolic");
    stop_button.set_tooltip_text(Some("Остановить запрос (Ctrl+Shift+.)"));
    stop_button.set_sensitive(false);
    toolbar.append(&stop_button);
    let actions_button = gtk::MenuButton::new();
    actions_button.set_icon_name("view-more-symbolic");
    actions_button.set_tooltip_text(Some("Действия с SQL и результатом (Ctrl+.)"));
    actions_button.add_css_class("sql-actions-button");
    actions_button.set_popover(Some(&popover));
    toolbar.append(&actions_button);
    editor_region.append(&toolbar);
    let error_message = gtk::Label::new(None);
    error_message.set_xalign(0.0);
    error_message.set_wrap(true);
    error_message.set_selectable(true);
    error_message.set_margin_start(8);
    error_message.set_margin_end(8);
    error_message.set_margin_top(6);
    error_message.set_margin_bottom(6);
    error_message.add_css_class("query-error-message");
    error_message.set_visible(false);
    editor_region.append(&dialogs::copyable_label(&error_message));
    let syntax_message = gtk::Label::new(None);
    syntax_message.set_xalign(0.0);
    syntax_message.set_selectable(true);
    syntax_message.set_wrap(true);
    syntax_message.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    syntax_message.set_max_width_chars(100);
    syntax_message.add_css_class("sql-syntax-diagnostic");
    syntax_message.set_visible(false);
    syntax_message.set_tooltip_text(Some("Локальная проверка синтаксиса. Существование таблиц и полей проверяет сервер при выполнении."));
    editor_region.append(&dialogs::copyable_label(&syntax_message));
    let diagnostics = sql_editor::Diagnostics::install(
        &buffer,
        &syntax_message,
        ui.runtime.clone(),
        dialect,
        &theme::palette(ui.active_theme.get()).error,
    );

    let scheme_buffer = buffer.clone();
    let scheme_manager = style_schemes.clone();
    let scheme_mode = ui.active_theme.clone();
    let diagnostic_theme = Rc::downgrade(&diagnostics);
    let style_notify = style_manager.connect_dark_notify(move |manager| {
        apply_sql_style_scheme(
            &scheme_buffer,
            &scheme_manager,
            scheme_mode.get(),
            manager.is_dark(),
        );
        if let Some(diagnostics) = diagnostic_theme.upgrade() {
            diagnostics.set_color(&theme::palette(scheme_mode.get()).error);
        }
    });
    let grid = gtk::Grid::new();
    grid.set_column_spacing(0);
    grid.set_row_spacing(0);
    grid.set_hexpand(true);
    grid.set_halign(gtk::Align::Fill);
    grid.set_visible(false);
    let weak = Rc::downgrade(ui);
    let result_key = page_key.clone();
    let tabulator = TabulatorGrid::new(true, move |message| {
        if let Some(ui) = weak.upgrade() {
            handle_tabulator_message(&ui, &result_key, &message);
        }
    });
    let edit_state = create_table_edit_state();
    let result_region = gtk::Box::new(gtk::Orientation::Vertical, 0);
    ui.table_edit_states
        .borrow_mut()
        .insert(page_key.clone(), edit_state.clone());
    ui.table_profiles
        .borrow_mut()
        .insert(page_key.clone(), profile.clone());
    ui.table_grids
        .borrow_mut()
        .insert(page_key.clone(), grid.clone());
    ui.tabulator_grids
        .borrow_mut()
        .insert(page_key.clone(), tabulator.clone());
    let save_grid = tabulator.clone();
    edit_state
        .save_button
        .connect_clicked(move |_| save_grid.finish_edit(false));
    let discard_grid = tabulator.clone();
    edit_state
        .discard_button
        .connect_clicked(move |_| discard_grid.finish_edit(true));
    tabulator.set_theme(&theme::grid_css(ui.active_theme.get()));
    let copy_result = sql_menu_action(
        &menu,
        &popover,
        "Копировать результат",
        "edit-copy-symbolic",
    );
    copy_result.set_sensitive(false);
    let result_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&tabulator.webview)
        .build();
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.set_start_child(Some(&editor_region));
    let result_stack = gtk::Stack::new();
    result_stack.add_css_class("sql-results-region");
    result_stack.set_hexpand(true);
    result_stack.set_vexpand(true);
    result_stack.add_named(&sql_empty_results(), Some("empty"));
    result_stack.add_named(&result_scroll, Some("data"));
    let plan_view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .left_margin(8)
        .right_margin(8)
        .top_margin(6)
        .bottom_margin(6)
        .build();
    let plan_scroll = gtk::ScrolledWindow::builder().child(&plan_view).build();
    result_stack.add_named(&plan_scroll, Some("plan"));
    result_stack.set_visible_child_name("empty");
    let copy_grid = tabulator.clone();
    let copy_stack = result_stack.downgrade();
    let copy_plan = plan_view.clone();
    copy_result.connect_clicked(move |_| {
        if copy_stack
            .upgrade()
            .and_then(|stack| stack.visible_child_name())
            .as_deref()
            == Some("plan")
        {
            let buffer = copy_plan.buffer();
            let (start, end) = buffer.selection_bounds().unwrap_or_else(|| buffer.bounds());
            copy_plan
                .clipboard()
                .set_text(&buffer.text(&start, &end, false));
        } else {
            copy_grid.copy_selection();
        }
    });
    result_region.append(&result_stack);
    result_region.append(&edit_state.toolbar);
    split.set_end_child(Some(&result_region));
    split.set_position(ui.layout.borrow().sql_position);
    let layout = ui.layout.clone();
    split.connect_position_notify(move |split| {
        if split.position() > 0 {
            layout.borrow_mut().sql_position = split.position();
        }
    });
    // Тонкую ручку перекрывает WebView результата, широкая ловит перетаскивание.
    split.set_wide_handle(true);
    split.add_css_class("sql-split");
    split.set_resize_start_child(true);
    split.set_resize_end_child(true);
    split.set_shrink_start_child(false);
    split.set_shrink_end_child(false);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_hexpand(true);
    page.set_vexpand(true);
    page.set_size_request(1, 1);
    page.set_tooltip_text(Some(&page_key));
    page.append(&split);
    shortcuts::button(&page, &write_button, "<Control><Shift>Return");
    shortcuts::button(&page, &open_button, "<Control>o");
    shortcuts::button(&page, &save_button, "<Control>s");
    shortcuts::button(&page, &rename_button, "<Control><Shift>r");
    shortcuts::button(&page, &directory_button, "<Control><Shift>o");
    shortcuts::button(&page, &export_button, "<Control><Shift>e");

    let tab_label = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_label.add_css_class("compact-tab-label");
    tab_label.set_tooltip_text(Some(&page_key));
    tab_label.append(&profile_tab_icon("sql-query", &profile.color));
    let title = gtk::Label::new(Some(&format!("SQL {query_number}")));
    let weak_ui = Rc::downgrade(ui);
    title.connect_label_notify(move |_| {
        if let Some(ui) = weak_ui.upgrade() {
            tab_strip::refresh(&ui);
        }
    });
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(8);
    title.set_max_width_chars(20);
    tab_label.append(&title);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(14, 14);
    spinner.set_visible(false);
    tab_label.append(&spinner);
    let close_button = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&close_button, 14);
    close_button.add_css_class("flat");
    close_button.add_css_class("circular");
    close_button.add_css_class("compact-tab-close");
    close_button.set_tooltip_text(Some("Закрыть SQL-вкладку"));
    tab_label.append(&close_button);

    ui.sql_tabs.borrow_mut().insert(
        id.clone(),
        SqlTab {
            session_key: session_key.to_owned(),
            group_key: group_key.clone(),
            schema: schema.clone(),
            profile_id: profile.id.clone(),
            session: sql_session::SqlSession::with_profile_and_schema(
                pool.clone(),
                &profile,
                schema,
            ),
            transaction_state: sql_session::TransactionState::default(),
            transaction_label,
            auto_commit: auto_commit.clone(),
            transaction_buttons: vec![
                manual_button.clone(),
                commit_button.clone(),
                rollback_button.clone(),
            ],
            actions_menu: actions_button,
            format_button,
            page: page.clone(),
            buffer: buffer.clone(),
            editor: editor.clone(),
            grid,
            tabulator,
            status,
            export_button: export_button.clone(),
            copy_button: copy_result,
            result_stack,
            plan_view,
            running_plan: None,
            run_button: run_button.clone(),
            stop_button: stop_button.clone(),
            timeout,
            completion_provider,
            diagnostics,
            style_schemes,
            risk_dialect,
            style_manager,
            style_notify: Some(style_notify),
            result: None,
            executed_sql: None,
            read_only_columns: Vec::new(),
            request_id: String::new(),
            title: title.clone(),
            query_name: format!("SQL {query_number}"),
            saved_path: None,
            risk_indicator: risk_indicator.clone(),
            error_message: error_message.clone(),
            spinner,
            started: None,
            elapsed_timer: None,
            sent_needing_approval: false,
            last_status: None,
        },
    );
    let tab_index = notebook.append_page(&page, Some(&tab_label));
    notebook.set_tab_reorderable(&page, true);
    notebook.set_current_page(Some(tab_index));
    ui.active_table_by_session
        .borrow_mut()
        .insert(group_key.clone(), page_key);
    ui.table_tabs.set_visible_child_name(&group_key);
    ui.table_tabs.set_visible(true);
    ui.empty_session.set_visible(false);
    ui.active_header.set_visible(false);

    let weak = Rc::downgrade(ui);
    let close_id = id.clone();
    close_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            close_sql_tab(&ui, &close_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let run_id = id.clone();
    run_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            run_sql_tab(&ui, &run_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let stop_id = id.clone();
    stop_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            stop_sql_tab(&ui, &stop_id, None);
        }
    });
    let weak = Rc::downgrade(ui);
    let reset_id = id.clone();
    reset_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            reset_sql_session(&ui, &reset_id);
        }
    });
    manual_button
        .bind_property("sensitive", &auto_commit, "sensitive")
        .sync_create()
        .build();
    let weak = Rc::downgrade(ui);
    let switch_id = id.clone();
    auto_commit.connect_state_set(move |_, wanted| {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        // try_borrow: update_transaction_controls sets the switch while holding the tab.
        let manual = match ui.sql_tabs.try_borrow() {
            Ok(tabs) => tabs.get(&switch_id).map(|tab| tab.transaction_state.manual),
            Err(_) => None,
        };
        if manual.is_some_and(|manual| manual == wanted) {
            change_sql_transaction(&ui, &switch_id, None);
            // Until the session confirms, the switch shows the actual mode;
            // update_transaction_controls moves it once the change lands.
            let weak = Rc::downgrade(&ui);
            let id = switch_id.clone();
            gtk::glib::idle_add_local_once(move || {
                let Some(ui) = weak.upgrade() else { return };
                if let Ok(tabs) = ui.sql_tabs.try_borrow()
                    && let Some(tab) = tabs.get(&id)
                {
                    tab.auto_commit.set_active(!tab.transaction_state.manual);
                    tab.auto_commit.set_state(!tab.transaction_state.manual);
                }
            });
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    for (button, action) in [
        (manual_button, None),
        (commit_button, Some(sql_session::TransactionAction::Commit)),
        (
            rollback_button,
            Some(sql_session::TransactionAction::Rollback),
        ),
    ] {
        let weak = Rc::downgrade(ui);
        let id = id.clone();
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                change_sql_transaction(&ui, &id, action);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    let export_id = id.clone();
    export_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_sql_csv_export(&ui, &export_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let open_id = id.clone();
    open_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_sql_open(&ui, &open_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let save_id = id.clone();
    save_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            save_sql_tab(&ui, &save_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let rename_id = id.clone();
    rename_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            rename_sql_tab(&ui, &rename_id);
        }
    });
    let weak = Rc::downgrade(ui);
    directory_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_sql_queries_directory(&ui);
        }
    });
    let draft_ui = Rc::downgrade(ui);
    let draft_profile = profile.id.clone();
    buffer.connect_changed(move |_| {
        if let Some(ui) = draft_ui.upgrade() {
            sql_history_ui::schedule_drafts(&ui, &draft_profile);
        }
    });
    sql_history_ui::schedule_drafts(ui, &profile.id);
    let risk_buffer = buffer.clone();
    let risk_label = risk_indicator.clone();
    buffer.connect_changed(move |_| {
        update_sql_risk_indicator_for(&risk_buffer, &risk_label, risk_dialect);
    });
    let risk_label = risk_indicator.clone();
    buffer.connect_mark_set(move |buffer, _, _| {
        update_sql_risk_indicator_for(buffer, &risk_label, risk_dialect)
    });
    if let Some(tab) = ui.sql_tabs.borrow().get(&id) {
        update_sql_risk_indicator(tab);
    }
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    let shortcut_id = id.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let control_only = state
            & (gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SHIFT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK)
            == gtk::gdk::ModifierType::CONTROL_MASK;
        let is_run = control_only && matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter);
        let is_complete = control_only && key == gtk::gdk::Key::space;
        if is_run {
            if let Some(ui) = weak.upgrade() {
                run_sql_tab(&ui, &shortcut_id);
            }
            gtk::glib::Propagation::Stop
        } else if is_complete {
            if let Some(ui) = weak.upgrade()
                && let Some(tab) = ui.sql_tabs.borrow().get(&shortcut_id)
                && tab.editor.is_mapped()
                && tab.editor.width() > 1
            {
                tab.editor.completion().show();
            }
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    editor.add_controller(keys);

    ui.status
        .set_text(&format!("Открыта SQL-вкладка {query_number}"));
    load_sql_completions(ui, &id, pool);
}

fn load_sql_completions(ui: &Ui, id: &str, pool: DbPool) {
    let sender = ui.sender.clone();
    let id = id.to_owned();
    ui.runtime.spawn(async move {
        let result = pool.list_sql_completions().await;
        let _ = sender.send(Event::SqlCompletionsLoaded(id, pool, result));
    });
}

fn refresh_sql_editor_themes(ui: &Ui) {
    for grid in ui.tabulator_grids.borrow().values() {
        grid.set_theme(&theme::grid_css(ui.active_theme.get()));
    }
    for tab in ui.sql_tabs.borrow().values() {
        tab.tabulator
            .set_theme(&theme::grid_css(ui.active_theme.get()));
        tab.diagnostics
            .set_color(&theme::palette(ui.active_theme.get()).error);
        apply_sql_style_scheme(
            &tab.buffer,
            &tab.style_schemes,
            ui.active_theme.get(),
            tab.style_manager.is_dark(),
        );
    }
}

fn apply_sql_style_scheme(
    buffer: &sourceview5::Buffer,
    manager: &sourceview5::StyleSchemeManager,
    mode: ThemeMode,
    dark: bool,
) {
    let scheme_name = theme::sql_scheme_id(mode, dark);
    if let Some(scheme) = manager
        .scheme(scheme_name)
        .or_else(|| manager.scheme(if dark { "Adwaita-dark" } else { "Adwaita" }))
    {
        buffer.set_style_scheme(Some(&scheme));
    }
}

fn run_sql_tab(ui: &Rc<Ui>, id: &str) {
    run_sql_tab_with_text(ui, id, None);
}
fn run_sql_tab_with_text(ui: &Rc<Ui>, id: &str, snapshot: Option<String>) {
    run_sql_tab_policy(ui, id, snapshot, false, SqlRunMode::Normal);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SqlRunMode {
    Normal,
    Write,
    Explain,
}

fn explain_sql_tab(ui: &Rc<Ui>, id: &str) {
    run_sql_tab_policy(ui, id, None, false, SqlRunMode::Explain);
}

fn run_sql_tab_policy(
    ui: &Rc<Ui>,
    id: &str,
    snapshot: Option<String>,
    approved: bool,
    mode: SqlRunMode,
) {
    let key = format!("sql::{id}");
    if let Some(state) = ui.table_edit_states.borrow().get(&key) {
        if state.saving.get() || state.grid_editing.get() || !state.pending.borrow().is_empty() {
            state
                .status
                .set_text("Сначала сохраните или отмените изменения результата");
            return;
        }
    }
    let Some(session_key) = ui
        .sql_tabs
        .borrow()
        .get(id)
        .map(|tab| tab.session_key.clone())
    else {
        return;
    };
    let Some((_pool, sql)) = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(_, pool)| pool.clone())
        .zip(
            ui.sql_tabs
                .borrow()
                .get(id)
                .map(|tab| sql_editor_text(&tab.buffer)),
        )
    else {
        return;
    };
    let sql = snapshot.unwrap_or(sql);
    if sql.trim().is_empty() {
        return;
    }
    let Some((profile, _)) = ui.sessions.borrow().get(&session_key).cloned() else {
        return;
    };
    let Some((session, state, idle, dialect)) = ui.sql_tabs.borrow().get(id).map(|t| {
        (
            t.session.clone(),
            t.transaction_state,
            t.run_button.is_sensitive(),
            t.risk_dialect,
        )
    }) else {
        return;
    };
    let explain = mode == SqlRunMode::Explain;
    if explain && !idle {
        return;
    }
    let plan = if explain {
        match sql_explain::EstimatedPlan::new(profile.kind, &sql) {
            Ok(plan) => Some(plan),
            Err(error) => {
                show_error_dialog(ui, "Не удалось построить план запроса", &error);
                return;
            }
        }
    } else {
        None
    };
    let needs_approval = !explain
        && (session.requires_write_approval(&sql, state)
            || (mode == SqlRunMode::Write
                && matches!(profile.environment, EnvironmentTag::Prod)
                && !profile.read_only));
    if block_if_broken(
        ui,
        &session_key,
        (!explain).then(|| RetryAction::Sql {
            tab_id: id.to_owned(),
            dialect,
            needed_approval: needs_approval,
            in_transaction: state.pending,
        }),
    ) {
        if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(id) {
            tab.executed_sql = (!explain).then_some(sql);
            tab.status
                .set_text("Соединение потеряно. Переподключитесь.");
        }
        return;
    }
    if needs_approval && !approved {
        let owner = Rc::downgrade(ui);
        let id = id.to_owned();
        let expected = session.identity();
        production_ui::confirm(
            ui,
            &profile,
            "Выполнить SQL с правом изменения данных",
            &sql.clone(),
            move || {
                if let Some(ui) = owner.upgrade() {
                    if ui
                        .sql_tabs
                        .borrow()
                        .get(&id)
                        .is_some_and(|t| t.session.identity() == expected)
                    {
                        run_sql_tab_policy(&ui, &id, Some(sql.clone()), true, mode);
                    }
                }
            },
        );
        return;
    }
    let request_id = Uuid::new_v4().to_string();
    let mut tabs = ui.sql_tabs.borrow_mut();
    let Some(tab) = tabs.get_mut(id) else {
        return;
    };
    if !tab.run_button.is_sensitive() {
        return;
    }
    let session = if explain {
        let session = tab.session.isolated_read_only();
        tab.running_plan = Some(session.clone());
        session
    } else {
        tab.session.clone()
    };
    session.clear_cancel_request();
    let session_key = tab.session_key.clone();
    let timeout = tab.timeout.value_as_int() as u64;
    tab.editor.completion().hide();
    // History and reconnect replay use runnable editor SQL only.
    tab.executed_sql = (!explain).then(|| sql.clone());
    tab.request_id = request_id.clone();
    tab.sent_needing_approval = needs_approval;
    ui.last_active
        .borrow_mut()
        .insert(format!("sql::{id}"), std::time::Instant::now());
    tab.status.set_text(if explain {
        "Строю план запроса…"
    } else {
        "Выполняю запрос…"
    });
    tab.run_button.set_sensitive(false);
    tab.stop_button.set_sensitive(true);
    for button in &tab.transaction_buttons {
        button.set_sensitive(false);
    }
    tab.export_button.set_sensitive(false);
    tab.copy_button.set_sensitive(false);
    tab.result_stack.set_visible_child_name("empty");
    tab.plan_view.buffer().set_text("");
    tab.error_message.set_text("");
    tab.error_message.set_visible(false);
    tab.result = None;
    ui.table_data.borrow_mut().remove(&key);
    ui.table_objects.borrow_mut().remove(&key);
    tab.tabulator.set_locked(true);
    while let Some(child) = tab.grid.first_child() {
        tab.grid.remove(&child);
    }
    start_sql_timer(ui, id, tab);
    let task_title = ui.sessions.borrow().get(&session_key).map_or_else(
        || tab.query_name.clone(),
        |(profile, _)| {
            format!(
                "{} · {} · {}",
                tab.query_name, profile.name, profile.database
            )
        },
    );
    let task_title = if explain {
        format!("EXPLAIN · {task_title}")
    } else {
        task_title
    };
    drop(tabs);
    ui.status.set_text(if explain {
        "Строю план запроса…"
    } else {
        "Выполняю SQL-запрос…"
    });

    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let tab_id = id.to_owned();
    let weak = Rc::downgrade(ui);
    let cancel_tab = tab_id.clone();
    let cancel_request = request_id.clone();
    ui.background_tasks
        .start(&request_id, &task_title, move || {
            if let Some(ui) = weak.upgrade() {
                stop_sql_tab(&ui, &cancel_tab, Some(&cancel_request));
            }
        });
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let started = std::time::Instant::now();
        let (result, reconnected) = if let Some(plan) = plan {
            let result = session.execute_with_timeout(plan.sql(), timeout).await;
            session.close().await;
            (result, false)
        } else {
            session.execute_with_recovery(&sql, timeout, approved).await
        };
        let result = report_lost(session.pool(), &sender, &session_key, result).await;
        let state = if explain {
            state
        } else {
            session.state().await
        };
        let _ = sender.send(Event::SqlQueryFinished(
            tab_id,
            request_id,
            result,
            state,
            started.elapsed().as_millis(),
            reconnected,
        ));
    });
}

fn stop_sql_tab(ui: &Rc<Ui>, id: &str, expected_request: Option<&str>) {
    if let Some(tab) = ui.sql_tabs.borrow().get(id)
        && expected_request.is_none_or(|request| tab.request_id == request)
        && tab.stop_button.is_sensitive()
    {
        tab.stop_button.set_sensitive(false);
        tab.status.set_text("Отменяю запрос на сервере…");
        tab.running_plan
            .as_ref()
            .unwrap_or(&tab.session)
            .request_cancel();
        ui.background_tasks.cancel_status(&tab.request_id, true);
    }
}

fn start_sql_timer(ui: &Rc<Ui>, id: &str, tab: &mut SqlTab) {
    stop_sql_timer(tab);
    tab.started = Some(std::time::Instant::now());
    tab.spinner.set_visible(true);
    let weak = Rc::downgrade(ui);
    let id = id.to_owned();
    tab.elapsed_timer = Some(gtk::glib::timeout_add_seconds_local(1, move || {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        let Ok(tabs) = ui.sql_tabs.try_borrow() else {
            return gtk::glib::ControlFlow::Continue;
        };
        // Missing tab was dropped together with its SourceId, so Break is safe.
        let Some(tab) = tabs.get(&id) else {
            return gtk::glib::ControlFlow::Break;
        };
        if let Some(started) = tab.started
            && tab.stop_button.is_sensitive()
        {
            let status = format!("Выполняется… {} с", started.elapsed().as_secs());
            tab.status.set_text(&status);
            ui.background_tasks.progress(&tab.request_id, &status);
        }
        gtk::glib::ControlFlow::Continue
    }));
}

/// Removes the running-query timer exactly once; safe to call repeatedly.
fn stop_sql_timer(tab: &mut SqlTab) {
    if let Some(source) = tab.elapsed_timer.take() {
        source.remove();
    }
    tab.started = None;
    tab.spinner.set_visible(false);
}

fn reset_sql_session(ui: &Rc<Ui>, id: &str) {
    let tabs = ui.sql_tabs.borrow();
    let Some(tab) = tabs.get(id) else { return };
    if !tab.run_button.is_sensitive() {
        tab.status
            .set_text("Сначала остановите запрос (Ctrl+Shift+.)");
        return;
    }
    let key = format!("sql::{id}");
    if ui
        .table_edit_states
        .borrow()
        .get(&key)
        .is_some_and(|s| s.saving.get() || s.grid_editing.get() || !s.pending.borrow().is_empty())
    {
        tab.status
            .set_text("Сначала сохраните или отмените правки результата");
        return;
    }
    let dialog = gtk::MessageDialog::builder().transient_for(&ui.window).modal(true).buttons(gtk::ButtonsType::YesNo).message_type(gtk::MessageType::Warning).text("Сбросить SQL-сессию?").secondary_text("Незавершённая транзакция будет отменена, временные таблицы и настройки этой сессии потеряются. Текст SQL останется во вкладке.").build();
    let weak = Rc::downgrade(ui);
    let id = id.to_owned();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response != gtk::ResponseType::Yes {
            return;
        }
        let Some(ui) = weak.upgrade() else { return };
        let mut tabs = ui.sql_tabs.borrow_mut();
        let Some(tab) = tabs.get_mut(&id) else { return };
        if !tab.run_button.is_sensitive() {
            return;
        }
        let Some((profile, pool)) = ui.sessions.borrow().get(&tab.session_key).cloned() else {
            return;
        };
        let previous = std::mem::replace(
            &mut tab.session,
            sql_session::SqlSession::with_profile_and_schema(pool, &profile, tab.schema.clone()),
        );
        ui.runtime.spawn(async move {
            previous.close().await;
        });
        update_transaction_controls(tab, sql_session::TransactionState::default());
        tab.tabulator.set_locked(true);
        tab.status
            .set_text("SQL-сессия сброшена. Выполните запрос заново.");
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn change_sql_transaction(ui: &Rc<Ui>, id: &str, action: Option<sql_session::TransactionAction>) {
    change_sql_transaction_authorized(ui, id, action, None);
}
fn change_sql_transaction_authorized(
    ui: &Rc<Ui>,
    id: &str,
    action: Option<sql_session::TransactionAction>,
    approved: Option<u64>,
) {
    let key = format!("sql::{id}");
    if ui
        .table_edit_states
        .borrow()
        .get(&key)
        .is_some_and(|s| s.saving.get() || s.grid_editing.get() || !s.pending.borrow().is_empty())
    {
        ui.status
            .set_text("Сначала сохраните или отмените правки результата");
        return;
    }
    let tabs = ui.sql_tabs.borrow();
    let Some(tab) = tabs.get(id) else { return };
    if !tab.run_button.is_sensitive() {
        return;
    }
    let action = action.unwrap_or(if tab.transaction_state.manual {
        sql_session::TransactionAction::AutoCommit
    } else {
        sql_session::TransactionAction::Manual
    });
    if matches!(action, sql_session::TransactionAction::Commit)
        && tab.transaction_state.pending
        && !tab.transaction_state.read_only
        && approved.is_none()
    {
        let profile = ui
            .sessions
            .borrow()
            .get(&tab.session_key)
            .map(|(p, _)| p.clone());
        if let Some(profile) = profile.filter(|p| matches!(p.environment, EnvironmentTag::Prod)) {
            let identity = tab.session.identity();
            let revision = tab.session.revision();
            drop(tabs);
            let owner = Rc::downgrade(ui);
            let id = id.to_owned();
            production_ui::confirm(
                ui,
                &profile,
                "Commit транзакции PROD",
                "Зафиксировать ранее выполненные изменения этой SQL-сессии?",
                move || {
                    if let Some(ui) = owner.upgrade() {
                        if ui.sql_tabs.borrow().get(&id).is_some_and(|t| {
                            t.session.identity() == identity && t.session.revision() == revision
                        }) {
                            change_sql_transaction_authorized(
                                &ui,
                                &id,
                                Some(sql_session::TransactionAction::Commit),
                                Some(revision),
                            );
                        }
                    }
                },
            );
            return;
        }
    }
    tab.run_button.set_sensitive(false);
    for button in &tab.transaction_buttons {
        button.set_sensitive(false);
    }
    tab.tabulator.set_locked(true);
    let session = tab.session.clone();
    let session_key = tab.session_key.clone();
    let id = id.to_owned();
    let sender = ui.sender.clone();
    let guard = ui.activity.start();
    ui.runtime.spawn(async move {
        let _guard = guard;
        let result = session.transaction_with_approval(action, approved).await;
        let result = report_lost(session.pool(), &sender, &session_key, result).await;
        let _ = sender.send(Event::SqlTransactionFinished(id, result));
    });
}

fn update_transaction_controls(tab: &mut SqlTab, state: sql_session::TransactionState) {
    tab.transaction_state = state;
    tab.transaction_label.set_text(if state.failed {
        "Ошибка · Rollback"
    } else if state.pending && state.read_only {
        "● Только чтение"
    } else if state.pending {
        "● Транзакция"
    } else if state.manual {
        "Ручной режим"
    } else {
        "Auto Commit"
    });
    let auto = !state.manual;
    if tab.auto_commit.is_active() != auto {
        tab.auto_commit.set_active(auto);
    }
    tab.auto_commit.set_state(auto);
    if state.pending {
        tab.transaction_label.add_css_class("status-pending");
    } else {
        tab.transaction_label.remove_css_class("status-pending");
    }
    for (index, button) in tab.transaction_buttons.iter().enumerate() {
        button.set_sensitive(index == 0 || state.pending);
    }
}

fn close_sql_tab(ui: &Rc<Ui>, id: &str) {
    let key = format!("sql::{id}");
    if let Some(state) = ui.table_edit_states.borrow().get(&key) {
        if state.saving.get() || state.grid_editing.get() || !state.pending.borrow().is_empty() {
            state
                .status
                .set_text("Сначала сохраните или отмените изменения результата перед закрытием");
            return;
        }
    }
    let mut tabs = ui.sql_tabs.borrow_mut();
    let Some(tab) = tabs.get_mut(id) else {
        return;
    };
    if !tab.run_button.is_sensitive() || tab.session.has_pending_transaction() {
        tab.status.set_text(
            "Завершите выполнение и транзакцию через Commit или Rollback перед закрытием",
        );
        return;
    }
    let draft_profile = tab.profile_id.clone();
    drop(tabs);
    if let Err(error) = sql_history_ui::save_drafts_except(ui, &draft_profile, Some(id)) {
        ui.status.set_text(&error);
        return;
    }
    let mut tabs = ui.sql_tabs.borrow_mut();
    let Some(tab) = tabs.get_mut(id) else {
        return;
    };
    let session = tab.session.clone();
    ui.runtime.spawn(async move {
        session.close().await;
    });
    let group_key = tab.group_key.clone();
    tab.editor.completion().hide();
    tab.diagnostics.close();
    let page = tab.page.clone();
    let style_manager = tab.style_manager.clone();
    let style_notify = tab.style_notify.take();
    let page_key = format!("sql::{id}");
    let was_active = ui
        .active_table_by_session
        .borrow()
        .get(&group_key)
        .is_some_and(|key| key == &page_key);
    drop(tabs);
    if let Some(style_notify) = style_notify {
        style_manager.disconnect(style_notify);
    }
    let notebook = ui.session_table_tabs.borrow().get(&group_key).cloned();
    if let Some(notebook) = notebook
        && let Some(index) = notebook.page_num(&page)
    {
        notebook.remove_page(Some(index));
        if was_active && notebook.n_pages() == 0 {
            ui.active_table_by_session.borrow_mut().remove(&group_key);
            if workspace_ui::is_active(ui, &group_key) {
                ui.table_tabs.set_visible(false);
                ui.empty_session.set_visible(true);
                ui.active_header.set_visible(false);
            }
        }
    }
    ui.sql_tabs.borrow_mut().remove(id);
    ui.table_data.borrow_mut().remove(&key);
    ui.table_objects.borrow_mut().remove(&key);
    ui.table_profiles.borrow_mut().remove(&key);
    ui.table_edit_states.borrow_mut().remove(&key);
    ui.table_grids.borrow_mut().remove(&key);
    ui.tabulator_grids.borrow_mut().remove(&key);
}

fn query_text(tab: &SqlTab) -> String {
    tab.buffer
        .text(&tab.buffer.start_iter(), &tab.buffer.end_iter(), true)
        .to_string()
}

fn update_sql_risk_indicator(tab: &SqlTab) {
    update_sql_risk_indicator_for(&tab.buffer, &tab.risk_indicator, tab.risk_dialect);
}

fn update_sql_risk_indicator_for(
    buffer: &sourceview5::Buffer,
    indicator: &gtk::Label,
    dialect: sql_risk::Dialect,
) {
    let sql = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    let selected = buffer
        .selection_bounds()
        .map(|(start, end)| buffer.text(&start, &end, true).to_string());
    let sql = selected
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&sql);
    let risk = sql_risk::analyze(sql, dialect);
    indicator.set_visible(risk == sql_risk::SqlRisk::ChangesData);
    indicator.set_text(match risk {
        sql_risk::SqlRisk::Empty => "Ожидает запрос",
        sql_risk::SqlRisk::ChangesData => "⚠ Может изменить данные",
        sql_risk::SqlRisk::ReadOnly => "Читающий запрос",
        sql_risk::SqlRisk::Unknown => "SQL не распознан",
    });
    indicator.remove_css_class("status-pending");
    indicator.remove_css_class("status-success");
    indicator.remove_css_class("dim-label");
    indicator.add_css_class(match risk {
        sql_risk::SqlRisk::ChangesData => "status-pending",
        sql_risk::SqlRisk::ReadOnly => "status-success",
        _ => "dim-label",
    });
    indicator.set_tooltip_text(Some(
        "Предварительная эвристическая оценка SQL. Она не гарантирует отсутствие побочных эффектов.",
    ));
}

fn query_directory_for_tab(ui: &Ui, id: &str) -> Result<PathBuf, String> {
    let root =
        ui.sql_queries_directory.borrow().clone().ok_or_else(|| {
            "Сначала выберите корневую папку запросов кнопкой «Папка…»".to_owned()
        })?;
    let session_key = ui
        .sql_tabs
        .borrow()
        .get(id)
        .map(|tab| tab.session_key.clone())
        .ok_or_else(|| "SQL-вкладка уже закрыта".to_owned())?;
    let profile = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(profile, _)| profile.clone())
        .ok_or_else(|| "Подключение к базе данных закрыто".to_owned())?;
    let database = if profile.database.trim().is_empty() {
        profile.name.as_str()
    } else {
        profile.database.as_str()
    };
    let folder = safe_query_file_component(database);
    let directory = root.join(folder);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Не удалось создать папку запросов: {error}"))?;
    Ok(directory)
}

fn safe_query_file_component(value: &str) -> String {
    let component = value
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*') {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    let component = component.trim().trim_matches('.');
    if component.is_empty() || component == ".." {
        "database".to_owned()
    } else {
        component.to_owned()
    }
}

fn sql_file_path(directory: &std::path::Path, name: &str) -> PathBuf {
    let name = name.trim();
    let file_name = if name.to_ascii_lowercase().ends_with(".sql") {
        name.to_owned()
    } else {
        format!("{name}.sql")
    };
    directory.join(safe_query_file_component(&file_name))
}

fn start_transfer(
    ui: &Rc<Ui>,
    operation: &str,
    path: &std::path::Path,
) -> Option<Arc<transfer::Cancellation>> {
    if ui.transfer_cancel.borrow().is_some() {
        show_error_dialog(
            ui,
            "Операция уже выполняется",
            "Дождитесь завершения или отмените текущую операцию.",
        );
        return None;
    }
    let cancel = Arc::new(transfer::Cancellation::new());
    *ui.transfer_cancel.borrow_mut() = Some(cancel.clone());
    let title = path.file_name().map_or_else(
        || operation.to_owned(),
        |name| format!("{operation} · {}", name.to_string_lossy()),
    );
    let weak = Rc::downgrade(ui);
    ui.background_tasks.start(TRANSFER, &title, move || {
        if let Some(ui) = weak.upgrade() {
            cancel_transfer(&ui);
        }
    });
    Some(cancel)
}

fn cancel_transfer(ui: &Ui) {
    if ui.background_tasks.can_cancel(TRANSFER)
        && let Some(cancel) = ui.transfer_cancel.borrow().as_ref()
    {
        let accepted = cancel.request();
        ui.background_tasks.cancel_status(TRANSFER, accepted);
        ui.status.set_text(if accepted {
            "Запрошена отмена операции…"
        } else {
            "Операция завершается…"
        });
    }
}

fn transfer_progress(ui: &Ui, text: &str) {
    if ui.background_tasks.progress(TRANSFER, text) {
        ui.status.set_text(text);
    }
}

fn finish_transfer(ui: &Ui, outcome: Outcome) {
    ui.transfer_cancel.borrow_mut().take();
    ui.background_tasks.finish(TRANSFER, outcome);
}

fn refresh_file_location_labels(ui: &Ui) {
    let store = ui.store.borrow();
    if let Some(store) = store.as_ref() {
        ui.sql_directory_label.set_text(
            &store
                .sql_queries_directory()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Не выбрана — папку можно указать при сохранении".into()),
        );
        ui.csv_directory_label.set_text(
            &store
                .csv_export_directory()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Последняя папка выбирается автоматически".into()),
        );
    }
}

fn configure_csv_chooser(ui: &Ui, chooser: &gtk::FileChooserDialog) {
    if let Some(path) = ui
        .store
        .borrow()
        .as_ref()
        .and_then(SettingsStore::csv_export_directory)
        .filter(|p| p.is_dir())
    {
        let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(path)));
    }
}

fn remember_csv_directory(ui: &Rc<Ui>, path: &std::path::Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let result = ui
        .store
        .borrow_mut()
        .as_mut()
        .ok_or("Хранилище недоступно".to_owned())
        .and_then(|store| store.set_csv_export_directory(parent.to_owned()));
    if let Err(error) = result {
        show_error_dialog(ui, "Папка CSV не запомнена", &error);
    }
    refresh_file_location_labels(ui);
}

fn choose_csv_directory(ui: &Rc<Ui>) {
    let chooser = dialogs::file_chooser(
        Some("Папка CSV"),
        Some(&ui.preferences),
        gtk::FileChooserAction::SelectFolder,
        Some("Выбрать"),
        Some("Отмена"),
    );
    configure_csv_chooser(ui, &chooser);
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) = (weak.upgrade(), dialog.file().and_then(|f| f.path())) {
                // This is a directory, not an exported file.
                let result = ui
                    .store
                    .borrow_mut()
                    .as_mut()
                    .ok_or("Хранилище недоступно".to_owned())
                    .and_then(|store| store.set_csv_export_directory(path));
                if let Err(error) = result {
                    show_error_dialog(&ui, "Папка не сохранена", &error);
                }
                refresh_file_location_labels(&ui);
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn choose_sql_queries_directory(ui: &Rc<Ui>) {
    let chooser = dialogs::file_chooser(
        Some("Папка SQL-запросов"),
        Some(&ui.window),
        gtk::FileChooserAction::SelectFolder,
        Some("Выбрать"),
        Some("Отмена"),
    );
    if let Some(path) = ui.sql_queries_directory.borrow().as_ref() {
        let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(path)));
    }
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
        {
            let saved = ui
                .store
                .borrow_mut()
                .as_mut()
                .ok_or_else(|| "Хранилище настроек не загружено".to_owned())
                .and_then(|store| store.set_sql_queries_directory(path.clone()));
            match saved {
                Ok(()) => {
                    *ui.sql_queries_directory.borrow_mut() = Some(path);
                    refresh_file_location_labels(&ui);
                    ui.status.set_text("Папка SQL-запросов сохранена");
                }
                Err(error) => show_error_dialog(&ui, "Не удалось сохранить папку запросов", &error),
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn choose_sql_open(ui: &Rc<Ui>, id: &str) {
    let directory = query_directory_for_tab(ui, id).ok();
    let chooser = dialogs::file_chooser(
        Some("Открыть SQL-запрос"),
        Some(&ui.window),
        gtk::FileChooserAction::Open,
        Some("Открыть"),
        Some("Отмена"),
    );
    if let Some(directory) = directory {
        let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(directory)));
    }
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("SQL-файлы (*.sql)"));
    filter.add_pattern("*.sql");
    chooser.add_filter(&filter);
    let weak = Rc::downgrade(ui);
    let id = id.to_owned();
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
        {
            match fs::read_to_string(&path) {
                Ok(sql) => {
                    if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(&id) {
                        tab.buffer.set_text(&sql);
                        tab.query_name = path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Запрос")
                            .to_owned();
                        tab.title.set_text(&tab.query_name);
                        tab.saved_path = Some(path);
                        tab.status.set_text("Запрос загружен из файла");
                    }
                }
                Err(error) => {
                    show_error_dialog(&ui, "Не удалось открыть SQL-файл", &error.to_string())
                }
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn save_sql_tab(ui: &Rc<Ui>, id: &str) {
    let saved_path = ui
        .sql_tabs
        .borrow()
        .get(id)
        .and_then(|tab| tab.saved_path.clone());
    if let Some(path) = saved_path {
        match fs::write(
            &path,
            ui.sql_tabs
                .borrow()
                .get(id)
                .map(query_text)
                .unwrap_or_default(),
        ) {
            Ok(()) => {
                if let Some(tab) = ui.sql_tabs.borrow().get(id) {
                    tab.status.set_text("Запрос сохранён");
                }
            }
            Err(error) => {
                show_error_dialog(ui, "Не удалось сохранить SQL-файл", &error.to_string())
            }
        }
        return;
    }
    let directory = query_directory_for_tab(ui, id).ok();
    let name = ui
        .sql_tabs
        .borrow()
        .get(id)
        .map(|tab| tab.query_name.clone())
        .unwrap_or_else(|| "Запрос".to_owned());
    let chooser = dialogs::file_chooser(
        Some("Сохранить SQL-запрос"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить"),
        Some("Отмена"),
    );
    if let Some(directory) = directory {
        let _ = chooser.set_current_folder(Some(&gtk::gio::File::for_path(directory)));
    }
    chooser.set_current_name(&format!("{}.sql", safe_query_file_component(&name)));
    let weak = Rc::downgrade(ui);
    let id = id.to_owned();
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
        {
            let path = if path.extension().is_some() {
                path
            } else {
                path.with_extension("sql")
            };
            let sql = ui
                .sql_tabs
                .borrow()
                .get(&id)
                .map(query_text)
                .unwrap_or_default();
            match fs::write(&path, sql) {
                Ok(()) => {
                    if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(&id) {
                        tab.saved_path = Some(path.clone());
                        tab.query_name = path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Запрос")
                            .to_owned();
                        tab.title.set_text(&tab.query_name);
                        tab.status.set_text("Запрос сохранён");
                    }
                }
                Err(error) => {
                    show_error_dialog(&ui, "Не удалось сохранить SQL-файл", &error.to_string())
                }
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn rename_sql_tab(ui: &Rc<Ui>, id: &str) {
    let Some((current_name, saved_path)) = ui
        .sql_tabs
        .borrow()
        .get(id)
        .map(|tab| (tab.query_name.clone(), tab.saved_path.clone()))
    else {
        return;
    };
    let dialog = gtk::Dialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .title("Переименовать запрос")
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Переименовать", gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_text(&current_name);
    entry.set_activates_default(true);
    dialog.set_default_response(gtk::ResponseType::Accept);
    dialog.content_area().append(&entry);
    let weak = Rc::downgrade(ui);
    let id = id.to_owned();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let Some(ui) = weak.upgrade() else {
                dialog.close();
                return;
            };
            let name = safe_query_file_component(&entry.text());
            let new_path = if let Some(old_path) = saved_path.as_ref() {
                let Some(parent) = old_path.parent() else {
                    dialog.close();
                    return;
                };
                let new_path = sql_file_path(parent, &name);
                if new_path != *old_path && new_path.exists() {
                    show_error_dialog(&ui, "Имя уже занято", "Файл с таким именем уже существует.");
                    dialog.close();
                    return;
                }
                if new_path != *old_path
                    && let Err(error) = fs::rename(old_path, &new_path)
                {
                    show_error_dialog(&ui, "Не удалось переименовать SQL-файл", &error.to_string());
                    dialog.close();
                    return;
                }
                Some(new_path)
            } else {
                None
            };
            if let Some(tab) = ui.sql_tabs.borrow_mut().get_mut(&id) {
                tab.query_name = name;
                tab.title.set_text(&tab.query_name);
                sql_history_ui::schedule_drafts(&ui, &tab.profile_id);
                if new_path.is_some() {
                    tab.saved_path = new_path;
                    tab.status.set_text("SQL-файл переименован");
                }
            }
        }
        dialog.close();
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn sql_editor_text(buffer: &sourceview5::Buffer) -> String {
    buffer
        .selection_bounds()
        .map(|(start, end)| buffer.text(&start, &end, true).to_string())
        .filter(|selection| !selection.trim().is_empty())
        .unwrap_or_else(|| {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        })
}

fn confirm_full_sql_csv_export(ui: &Rc<Ui>, id: &str) {
    let Some((session, sql, timeout)) = ui.sql_tabs.borrow().get(id).map(|tab| {
        (
            tab.session.clone(),
            sql_editor_text(&tab.buffer),
            tab.timeout.value_as_int() as u64,
        )
    }) else {
        return;
    };
    if let Err(error) = session.validate_csv_query(&sql) {
        show_error_dialog(ui, "Не удалось запустить полный экспорт CSV", &error);
        return;
    }
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .text("Выполнить запрос для полного CSV?")
        .secondary_text("Будет выполнен выделенный SQL или весь текст редактора в отдельной транзакции только для чтения. Временные таблицы и несохранённые изменения SQL-сессии недоступны.")
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Выбрать файл…", gtk::ResponseType::Accept);
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Accept
            && let Some(ui) = weak.upgrade()
        {
            choose_full_sql_csv_export(&ui, session.clone(), sql.clone(), timeout);
        }
    });
    dialog.present();
}

fn choose_full_sql_csv_export(
    ui: &Rc<Ui>,
    session: sql_session::SqlSession,
    sql: String,
    timeout: u64,
) {
    let title = "Экспорт полного результата SQL в CSV";
    let chooser = dialogs::file_chooser(
        Some(title),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить CSV"),
        Some("Отмена"),
    );
    configure_csv_chooser(ui, &chooser);
    chooser.set_current_name("query-result-full.csv");
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            && let Some(cancel) = start_transfer(&ui, title, &path)
        {
            remember_csv_directory(&ui, &path);
            ui.status.set_text("Выполняю запрос для полного CSV…");
            let sender = ui.sender.clone();
            let session = session.clone();
            let sql = sql.clone();
            let runtime = ui.runtime.clone();
            let guard = ui.activity.start();
            ui.runtime.spawn_blocking(move || {
                let _guard = guard;
                let result = runtime.block_on(write_full_sql_csv(
                    &path,
                    &session,
                    &sql,
                    timeout,
                    &cancel,
                    |count| {
                        let _ = sender.send(Event::ExportProgress(count));
                    },
                ));
                let event = match result {
                    Ok(rows) => Event::CsvExported(rows, Ok(())),
                    Err(error) => Event::CsvExported(0, Err(error)),
                };
                let _ = sender.send(event);
            });
        }
        dialog.destroy();
    });
    chooser.show();
}

async fn write_full_sql_csv(
    path: &std::path::Path,
    session: &sql_session::SqlSession,
    sql: &str,
    timeout: u64,
    cancel: &transfer::Cancellation,
    progress: impl FnMut(usize) + Send,
) -> Result<usize, String> {
    let mut output = output_file::AtomicOutput::new(path)?;
    let rows = session
        .export_csv(sql, timeout, &mut output.file, cancel, progress)
        .await?;
    output.commit(cancel)?;
    Ok(rows)
}

fn choose_sql_csv_export(ui: &Rc<Ui>, id: &str) {
    let result = ui
        .sql_tabs
        .borrow()
        .get(id)
        .and_then(|tab| tab.result.clone());
    let Some(data) = result else {
        return;
    };
    let chooser = dialogs::file_chooser(
        Some("Экспорт результата SQL в CSV"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить CSV"),
        Some("Отмена"),
    );
    configure_csv_chooser(ui, &chooser);
    chooser.set_current_name("query-result.csv");
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                remember_csv_directory(&ui, &path);
                let data = data.clone();
                ui.status.set_text("Сохраняю результат запроса в CSV…");
                let sender = ui.sender.clone();
                let runtime = ui.runtime.clone();
                let Some(cancel) = start_transfer(&ui, "Экспорт результата SQL в CSV", &path)
                else {
                    dialog.destroy();
                    return;
                };
                let guard = ui.activity.start();
                runtime.spawn_blocking(move || {
                    let _guard = guard;
                    let rows = data.rows.len();
                    let result = if cancel.is_cancelled() {
                        Err("Экспорт отменён".into())
                    } else {
                        write_snapshot_csv(&path, &data, &cancel, |count| {
                            let _ = sender.send(Event::ExportProgress(count));
                        })
                    };
                    let event = match result {
                        Ok(()) => (rows, Ok(())),
                        Err(error) => (0, Err(error)),
                    };
                    let _ = sender.send(Event::CsvExported(event.0, event.1));
                });
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn profile_session_key(profile: &ConnectionProfile) -> String {
    if !profile.tls_files.is_empty() {
        return serde_json::to_string(&(
            &profile.id,
            profile.kind,
            &profile.host,
            profile.port,
            &profile.database,
            &profile.username,
            profile.tls_mode,
            &profile.tls_files,
        ))
        .expect("connection identity");
    }
    // Structured encoding avoids collisions when names contain separators.
    serde_json::to_string(&(
        &profile.id,
        profile.kind,
        &profile.host,
        profile.port,
        &profile.database,
        &profile.username,
        profile.tls_mode,
    ))
    .expect("connection identity is serializable")
}

fn session_page_index(ui: &Ui, session_key: &str) -> Option<u32> {
    for index in 0..ui.session_tabs.n_pages() {
        let page = ui.session_tabs.nth_page(Some(index))?;
        if page.tooltip_text().as_deref() == Some(session_key) {
            return Some(index);
        }
    }
    None
}

fn close_session(ui: &Rc<Ui>, session_key: &str) {
    if ui.sql_tabs.borrow().values().any(|tab| {
        tab.session_key == session_key
            && (!tab.run_button.is_sensitive() || tab.session.has_pending_transaction())
    }) {
        ui.status
            .set_text("Завершите SQL-запросы и транзакции перед отключением");
        return;
    }
    if ui.table_edit_states.borrow().iter().any(|(key, state)| {
        state.saving.get()
            && ui
                .table_profiles
                .borrow()
                .get(key)
                .is_some_and(|profile| profile_session_key(profile) == session_key)
    }) {
        ui.status
            .set_text("Подождите завершения сохранения изменений таблицы");
        return;
    }
    let dirty_tables = ui
        .table_edit_states
        .borrow()
        .iter()
        .filter(|(key, state)| {
            ui.table_profiles
                .borrow()
                .get(*key)
                .is_some_and(|profile| profile_session_key(profile) == session_key)
                && (!state.pending.borrow().is_empty() || state.bulk_column.borrow().is_some())
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if !dirty_tables.is_empty() {
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&ui.window)
            .modal(true)
            .message_type(gtk::MessageType::Warning)
            .buttons(gtk::ButtonsType::YesNo)
            .text("Отменить несохранённые изменения и отключиться?")
            .secondary_text("Изменения в открытых таблицах будут потеряны.")
            .build();
        let weak = Rc::downgrade(ui);
        let session_key = session_key.to_owned();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Yes
                && let Some(ui) = weak.upgrade()
            {
                for key in &dirty_tables {
                    if let Some(state) = ui.table_edit_states.borrow().get(key) {
                        state.pending.borrow_mut().clear();
                        state.bulk_column.borrow_mut().take();
                    }
                }
                close_session(&ui, &session_key);
            }
        });
        dialogs::install(&dialog);
        dialog.present();
        return;
    }
    let draft_profile = ui
        .sessions
        .borrow()
        .get(session_key)
        .map(|(profile, _)| profile.id.clone());
    if let Some(profile) = &draft_profile {
        if let Err(error) = sql_history_ui::save_drafts(ui, profile) {
            ui.status.set_text(&error);
            return;
        }
    }
    ui.restoring_drafts.set(true);
    let profile_id = ui
        .sessions
        .borrow()
        .get(session_key)
        .map(|(profile, _)| profile.id.clone());
    let was_active = ui
        .current_profile
        .borrow()
        .as_ref()
        .is_some_and(|profile| profile_session_key(profile) == session_key);
    let groups: Vec<_> = ui
        .workspace_groups
        .borrow()
        .iter()
        .filter(|(_, g)| g.session_key == session_key)
        .map(|(key, _)| key.clone())
        .collect();
    for group in &groups {
        if let Some(index) = session_page_index(ui, group) {
            ui.session_tabs.remove_page(Some(index));
        }
    }
    ui.session_tabs.set_visible(ui.session_tabs.n_pages() > 0);
    ui.sessions.borrow_mut().remove(session_key);
    ui.broken_sessions.borrow_mut().remove(session_key);
    ui.server_icons.borrow_mut().remove(session_key);
    catalog_tree::forget(ui, session_key);
    if let Some(profile_id) = profile_id
        && !ui
            .sessions
            .borrow()
            .values()
            .any(|(profile, _)| profile.id == profile_id)
    {
        ui.profile_status.borrow_mut().remove(&profile_id);
        ui.pending_retry.borrow_mut().remove(&profile_id);
        if ui.banner_profile.borrow().as_deref() == Some(profile_id.as_str()) {
            ui.connection_banner.set_revealed(false);
        }
    }
    ui.connect_requests.borrow_mut().remove(session_key);
    ui.pending_database_opens.borrow_mut().remove(session_key);
    let keys = ui
        .table_profiles
        .borrow()
        .iter()
        .filter(|(_, profile)| profile_session_key(profile) == session_key)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        close_table_tab(ui, &key);
    }
    let query_ids = ui
        .sql_tabs
        .borrow()
        .iter()
        .filter(|(_, tab)| tab.session_key == session_key)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in query_ids {
        close_sql_tab(ui, &id);
    }
    for group in groups
        .into_iter()
        .chain(std::iter::once(session_key.to_owned()))
    {
        if let Some(notebook) = ui.session_table_tabs.borrow_mut().remove(&group) {
            ui.table_tabs.remove(&notebook);
        }
        ui.workspace_groups.borrow_mut().remove(&group);
        ui.active_table_by_session.borrow_mut().remove(&group);
    }
    ui.active_group_by_session.borrow_mut().remove(session_key);
    ui.restoring_drafts.set(false);
    if ui.sessions.borrow().is_empty() {
        ui.desired_database_session.borrow_mut().take();
        ui.connection_generation
            .set(ui.connection_generation.get().wrapping_add(1));
        ui.view_generation
            .set(ui.view_generation.get().wrapping_add(1));
        ui.session_tabs.set_visible(false);
        *ui.current_profile.borrow_mut() = None;
        *ui.pool.borrow_mut() = None;
        tab_strip::refresh(ui);
        namespace_ui::sync(ui);
        ui.active_header.set_visible(false);
        ui.connection_block.set(None);
        ui.table_tabs.set_visible(false);
        ui.empty_session.set_visible(false);
        // Drop the closed connection's tree: its rows hold the old pool.
        clear_box(&ui.objects);
        ui.object_search_branches.borrow_mut().clear();
        ui.table_list_items.borrow_mut().clear();
        ui.selected_table_button.borrow_mut().take();
        ui.table_filter.set_visible(false);
        ui.objects_available.set(false);
        set_tables_nav_sensitive(ui, false);
        show_sidebar(ui, "connections");
        ui.landing.set_visible(true);
        ui.active.set_text("Нет активного подключения");
        refresh_profiles(ui);
        return;
    }
    if !was_active {
        catalog_tree::render(ui);
        refresh_profiles(ui);
        return;
    }
    // Prefer a visible database group over a hidden server-navigation connection.
    let next_id = ui
        .session_tabs
        .current_page()
        .and_then(|index| ui.session_tabs.nth_page(Some(index)))
        .and_then(|page| page.tooltip_text().map(|key| key.to_string()))
        .filter(|key| {
            ui.sessions
                .borrow()
                .contains_key(&workspace_ui::source(ui, key))
        })
        .or_else(|| ui.sessions.borrow().keys().next().cloned());
    if let Some(next_id) = next_id {
        workspace_ui::select(ui, &next_id);
    }
}

fn close_profile_sessions(ui: &Rc<Ui>, profile_id: &str) {
    ui.connect_requests.borrow_mut().retain(|key, _| {
        serde_json::from_str::<serde_json::Value>(key)
            .ok()
            .and_then(|v| v.get(0).and_then(|id| id.as_str()).map(str::to_owned))
            .as_deref()
            != Some(profile_id)
    });
    ui.pending_database_opens
        .borrow_mut()
        .retain(|key| ui.connect_requests.borrow().contains_key(key));
    let keys = ui
        .sessions
        .borrow()
        .iter()
        .filter(|(_, (profile, _))| profile.id == profile_id)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        close_session(ui, &key);
    }
}

fn find_table_page(ui: &Ui, key: &str) -> Option<(gtk::Notebook, u32, gtk::Widget)> {
    let session_key = workspace_ui::table_group(ui, key)?;
    let notebook = ui.session_table_tabs.borrow().get(&session_key).cloned()?;
    for index in 0..notebook.n_pages() {
        let page = notebook.nth_page(Some(index))?;
        if table_key_from_page(&notebook, &page).as_deref() == Some(key) {
            return Some((notebook, index, page));
        }
    }
    None
}

fn table_key_from_page(notebook: &gtk::Notebook, page: &gtk::Widget) -> Option<String> {
    let tab = notebook.tab_label(page)?.downcast::<gtk::Box>().ok()?;
    tab.tooltip_text().map(|key| key.to_string())
}

fn close_table_tab(ui: &Rc<Ui>, key: &str) {
    if ui
        .table_edit_states
        .borrow()
        .get(key)
        .is_some_and(|state| state.saving.get())
    {
        ui.status
            .set_text("Подождите завершения сохранения изменений таблицы");
        return;
    }
    if let Some(state) = ui.table_edit_states.borrow().get(key)
        && (!state.pending.borrow().is_empty() || state.bulk_column.borrow().is_some())
    {
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&ui.window)
            .modal(true)
            .message_type(gtk::MessageType::Warning)
            .buttons(gtk::ButtonsType::YesNo)
            .text("Отменить несохранённые изменения?")
            .secondary_text("Закрытие вкладки удалит все подготовленные изменения ячеек.")
            .build();
        let weak = Rc::downgrade(ui);
        let key = key.to_owned();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Yes
                && let Some(ui) = weak.upgrade()
            {
                if let Some(state) = ui.table_edit_states.borrow().get(&key) {
                    state.pending.borrow_mut().clear();
                    state.bulk_column.borrow_mut().take();
                }
                close_table_tab(&ui, &key);
            }
        });
        dialogs::install(&dialog);
        dialog.present();
        return;
    }
    let session_key = workspace_ui::table_group(ui, key);
    let page_info = find_table_page(ui, key);
    let was_active = session_key.as_ref().is_some_and(|session_key| {
        ui.active_table_by_session
            .borrow()
            .get(session_key)
            .is_some_and(|active| active == key)
    });
    let fallback = page_info.as_ref().and_then(|(notebook, target_index, _)| {
        let mut candidates = (0..notebook.n_pages())
            .filter(|index| index != target_index)
            .filter_map(|index| {
                let page = notebook.nth_page(Some(index))?;
                let candidate_key = table_key_from_page(notebook, &page)?;
                if candidate_key
                    .strip_prefix("sql::")
                    .and_then(|id| {
                        ui.sql_tabs
                            .borrow()
                            .get(id)
                            .map(|tab| tab.group_key.clone())
                    })
                    .as_deref()
                    == session_key.as_deref()
                {
                    return Some((index, candidate_key));
                }
                let candidate_group = workspace_ui::table_group(ui, &candidate_key)?;
                (session_key.as_deref() == Some(candidate_group.as_str()))
                    .then_some((index, candidate_key))
            })
            .collect::<Vec<_>>();
        candidates
            .sort_by_key(|(index, _)| (index.abs_diff(*target_index), *index < *target_index));
        candidates
            .into_iter()
            .next()
            .map(|(index, key)| (notebook.clone(), index, key))
    });
    if let Some((notebook, page, widget)) = page_info {
        ui.tab_switching.set(true);
        notebook.remove_page(Some(page));
        ui.tab_switching.set(false);
        ui.table_grids.borrow_mut().remove(key);
        ui.table_profiles.borrow_mut().remove(key);
        ui.table_objects.borrow_mut().remove(key);
        ui.table_view_states.borrow_mut().remove(key);
        ui.table_filter_panels.borrow_mut().remove(key);
        ui.table_requests.borrow_mut().remove(key);
        ui.table_more_loading.borrow_mut().remove(key);
        ui.table_data.borrow_mut().remove(key);
        ui.table_edit_states.borrow_mut().remove(key);
        ui.table_spinners.borrow_mut().remove(key);
        drop(widget);
    }
    if let Some(session_key) = session_key {
        if was_active {
            if let Some((_, _, fallback)) = fallback {
                ui.active_table_by_session
                    .borrow_mut()
                    .insert(session_key.clone(), fallback.clone());
                if workspace_ui::is_active(ui, &session_key) {
                    if let Some((notebook, page, _)) = find_table_page(ui, &fallback) {
                        select_table_button(ui, &fallback);
                        notebook.set_current_page(Some(page));
                    } else if let Some(query_id) = fallback.strip_prefix("sql::")
                        && let Some(tab) = ui.sql_tabs.borrow().get(query_id)
                        && let Some(notebook) = ui.session_table_tabs.borrow().get(&session_key)
                        && let Some(page) = notebook.page_num(&tab.page)
                    {
                        notebook.set_current_page(Some(page));
                    }
                }
            } else {
                ui.active_table_by_session.borrow_mut().remove(&session_key);
                if workspace_ui::is_active(ui, &session_key) {
                    ui.table_tabs.set_visible(false);
                    ui.empty_session.set_visible(true);
                    ui.active_header.set_visible(false);
                }
            }
        }
    }
}

fn select_session(ui: &Rc<Ui>, session_key: &str) {
    let session = ui.sessions.borrow().get(session_key).cloned();
    let Some((profile, pool)) = session else {
        return;
    };
    *ui.desired_database_session.borrow_mut() = Some(session_key.to_owned());
    let changed = ui
        .current_profile
        .borrow()
        .as_ref()
        .is_none_or(|current| profile_session_key(current) != session_key);
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    *ui.pool.borrow_mut() = Some(pool);
    tab_strip::refresh(ui);
    let group = workspace_ui::active(ui, session_key);
    if let Some(index) = session_page_index(ui, &group) {
        if ui.session_tabs.current_page() != Some(index) {
            ui.session_tabs.set_current_page(Some(index));
        }
    }
    let database = if profile.database.is_empty() {
        "Все доступные базы"
    } else {
        profile.database.as_str()
    };
    let schema = ui
        .workspace_groups
        .borrow()
        .get(&group)
        .and_then(|g| g.schema.clone());
    ui.active.set_text(
        &schema
            .map(|schema| format!("{database}.{schema}"))
            .unwrap_or_else(|| database.to_owned()),
    );
    ui.active_details.set_text(&format!(
        "{}  ·  {}:{}  ·  {}{}",
        profile.kind.label(),
        profile.host,
        profile.port,
        profile.environment.label(),
        if profile.read_only {
            "  ·  ТОЛЬКО ЧТЕНИЕ"
        } else {
            ""
        }
    ));
    ui.active_details.set_text(&format!(
        "{}  ·  {}",
        ui.active_details.text(),
        transport_summary(&profile)
    ));
    set_profile_accent(&ui.active_header, &profile.color);
    ui.active_header.set_visible(false);
    ui.connection_block.set(Some(&profile));
    ui.landing.set_visible(false);
    activate_session_table(ui, session_key);
    namespace_ui::sync(ui);
    refresh_profiles(ui);
    if changed {
        ui.objects_available.set(false);
        set_tables_nav_sensitive(ui, false);
        clear_box(&ui.objects);
        ui.object_search_branches.borrow_mut().clear();
        show_table_catalog(ui);
    }
}

fn activate_session_table(ui: &Ui, session_key: &str) {
    refresh_empty_session(ui);
    let group = workspace_ui::active(ui, session_key);
    let session_key = group.as_str();
    let notebook = ui.session_table_tabs.borrow().get(session_key).cloned();
    let Some(notebook) = notebook else {
        ui.table_tabs.set_visible(false);
        ui.empty_session.set_visible(true);
        ui.active_header.set_visible(false);
        return;
    };
    ui.table_tabs.set_visible_child_name(session_key);
    let active_key = ui
        .active_table_by_session
        .borrow()
        .get(session_key)
        .cloned()
        .filter(|key| {
            find_table_page(ui, key).is_some()
                || key
                    .strip_prefix("sql::")
                    .is_some_and(|id| ui.sql_tabs.borrow().contains_key(id))
        });
    if let Some(key) = active_key {
        if let Some((_, page, _)) = find_table_page(ui, &key) {
            select_table_button(ui, &key);
            ui.tab_switching.set(true);
            notebook.set_current_page(Some(page));
            ui.tab_switching.set(false);
            ui.table_tabs.set_visible(true);
            ui.empty_session.set_visible(false);
            tab_strip::sync_connection(ui);
            return;
        }
        if let Some(query_id) = key.strip_prefix("sql::")
            && let Some(tab) = ui.sql_tabs.borrow().get(query_id)
            && let Some(page) = notebook.page_num(&tab.page)
        {
            notebook.set_current_page(Some(page));
            ui.table_tabs.set_visible(true);
            ui.empty_session.set_visible(false);
            ui.active_header.set_visible(false);
            return;
        }
    }
    if notebook.n_pages() > 0 {
        notebook.set_current_page(Some(0));
        ui.table_tabs.set_visible(true);
        ui.empty_session.set_visible(false);
        tab_strip::sync_connection(ui);
        return;
    }
    ui.table_tabs.set_visible(false);
    ui.empty_session.set_visible(true);
    ui.active_header.set_visible(false);
}

/// Active group of the current session, if the user has opened one.
fn active_workspace_group(ui: &Ui) -> Option<String> {
    let profile = ui.current_profile.borrow().clone()?;
    let group = workspace_ui::active(ui, &profile_session_key(&profile));
    ui.workspace_groups
        .borrow()
        .contains_key(&group)
        .then_some(group)
}

fn refresh_empty_session(ui: &Ui) {
    let pg = ui
        .current_profile
        .borrow()
        .as_ref()
        .is_some_and(|p| matches!(p.kind, DatabaseKind::PostgreSql));
    let opened = active_workspace_group(ui).is_some();
    ui.empty_session_hint.set_text(match (opened, pg) {
        (true, _) => "Откройте таблицу слева или создайте SQL-запрос",
        (false, true) => "Выберите схему слева или создайте SQL-запрос",
        (false, false) => "Выберите базу данных слева или создайте SQL-запрос",
    });
    ui.empty_session_sql
        .set_visible(opened || ui.current_profile.borrow().is_some());
}

/// Opens (or focuses) the top-level workspace tab of a schema / MySQL database.
fn open_schema_group(ui: &Rc<Ui>, profile: &ConnectionProfile, schema: &str) {
    let session = profile_session_key(profile);
    if !ui.sessions.borrow().contains_key(&session) {
        return;
    }
    let schema = match profile.kind {
        DatabaseKind::PostgreSql => Some(schema),
        _ if profile.database == schema => None,
        _ => {
            with_mysql_database(ui, profile, schema, |_, _| {});
            return;
        }
    };
    let group = workspace_ui::ensure(ui, &session, schema);
    ensure_session_table_tabs(ui, &group);
    workspace_ui::select(ui, &group);
    namespace_ui::sync(ui);
}

/// MySQL/MariaDB keep one session per database: connect `database` on demand,
/// open its workspace group and run `action` with the new session key.
fn with_mysql_database(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    database: &str,
    action: impl FnOnce(&Rc<Ui>, &str) + 'static,
) {
    let pool = ui
        .sessions
        .borrow()
        .get(&profile_session_key(profile))
        .map(|(_, pool)| pool.clone());
    let Some(pool) = pool else {
        ui.status.set_text("База отключена. Раскройте её заново.");
        return;
    };
    let mut target = profile.clone();
    target.database = database.to_owned();
    with_database_session(ui, target, pool, move |ui, key| {
        let profile = ui.sessions.borrow().get(key).map(|(p, _)| p.clone());
        let Some(profile) = profile else { return };
        open_schema_group(ui, &profile, &profile.database);
        action(ui, key);
    });
}

/// Opens a new SQL tab bound to the chosen schema / MySQL database.
fn open_sql_in_schema(ui: &Rc<Ui>, profile: &ConnectionProfile, schema: &str) {
    match profile.kind {
        DatabaseKind::PostgreSql => {
            open_schema_group(ui, profile, schema);
            let group = workspace_ui::key(profile, Some(schema));
            if ui.workspace_groups.borrow().contains_key(&group) {
                create_sql_tab_in_group(ui, &group);
            }
        }
        _ if profile.database == schema => {
            open_schema_group(ui, profile, schema);
            create_sql_tab(ui, &profile_session_key(profile));
        }
        _ => with_mysql_database(ui, profile, schema, create_sql_tab),
    }
}

fn tree_sql_button(target: &str) -> gtk::Button {
    let icon = penpot_icon_image("sql-query");
    icon.remove_css_class("penpot-icon");
    icon.set_pixel_size(16);
    icon.set_size_request(16, 16);
    let button = gtk::Button::new();
    button.set_child(Some(&icon));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("flat");
    button.add_css_class("tree-row-action");
    button.set_tooltip_text(Some(&format!("Новый SQL-запрос: {target}")));
    button
}

/// Tree row with a hover action; the action is a sibling, not nested in the row button.
fn tree_line(row: &gtk::Button, action: &gtk::Button) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("tree-row-line");
    row.set_hexpand(true);
    line.append(row);
    line.append(action);
    line
}

fn start_new_connection(ui: &Rc<Ui>) {
    let after_unlock = ui.clone();
    ensure_vault_unlocked(ui, move || show_connection_form(&after_unlock, None));
}

fn select_table_button(ui: &Ui, key: &str) {
    let mut selected = None;
    for (button, _) in ui.table_list_items.borrow().iter() {
        button.remove_css_class("table-nav-item-selected");
        if button.tooltip_text().as_deref() == Some(key) {
            button.add_css_class("table-nav-item-selected");
            selected = Some(button.clone());
        }
    }
    *ui.selected_table_button.borrow_mut() = selected;
}

fn select_schema_button(ui: &Ui, selected: &gtk::Button) {
    fn clear(widget: &gtk::Widget) {
        widget.remove_css_class("schema-tree-row-active");
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            clear(&widget);
        }
    }
    clear(ui.objects.upcast_ref());
    selected.add_css_class("schema-tree-row-active");
}

fn apply_table_list_filter(ui: &Ui) {
    object_search::apply(ui);
}

fn install_table_context_menu(
    ui: &Rc<Ui>,
    anchor: &gtk::Button,
    profile: ConnectionProfile,
    object: DbObject,
    table_key: String,
) {
    let middle = gtk::GestureClick::new();
    middle.set_button(2);
    let weak = Rc::downgrade(ui);
    let middle_object = object.clone();
    let middle_session = profile_session_key(&profile);
    middle.connect_released(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let Some(ui) = weak.upgrade() else { return };
        if !ui.sessions.borrow().contains_key(&middle_session) {
            ui.status.set_text("База отключена. Раскройте её заново.");
            return;
        }
        select_session(&ui, &middle_session);
        open_table(&ui, middle_object.clone(), true);
    });
    anchor.add_controller(middle);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    let anchor_widget = anchor.downgrade();
    gesture.connect_pressed(move |_, _, x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(anchor_widget) = anchor_widget.upgrade() else {
            return;
        };
        let popover = gtk::Popover::new();
        popover.add_css_class("table-context-popover");
        set_profile_accent(&popover, &profile.color);
        popover.set_position(gtk::PositionType::Bottom);
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 1);
        actions.add_css_class("table-context-actions");

        let open = context_menu_action("document-open-symbolic", "Открыть в текущей вкладке");
        let weak = Rc::downgrade(&ui);
        let selected = object.clone();
        let source_session = profile_session_key(&profile);
        let popover_close = popover.clone();
        open.connect_clicked(move |_| {
            popover_close.popdown();
            if let Some(ui) = weak.upgrade() {
                if !ui.sessions.borrow().contains_key(&source_session) {
                    ui.status.set_text("База отключена. Раскройте её заново.");
                    return;
                }
                select_session(&ui, &source_session);
                open_table(&ui, selected.clone(), false);
            }
        });
        actions.append(&open);

        let open_new = context_menu_action("tab-new-symbolic", "Открыть в новой вкладке");
        let weak = Rc::downgrade(&ui);
        let selected = object.clone();
        let source_session = profile_session_key(&profile);
        let popover_close = popover.clone();
        open_new.connect_clicked(move |_| {
            popover_close.popdown();
            if let Some(ui) = weak.upgrade() {
                if !ui.sessions.borrow().contains_key(&source_session) {
                    ui.status.set_text("База отключена. Раскройте её заново.");
                    return;
                }
                select_session(&ui, &source_session);
                open_table(&ui, selected.clone(), true);
            }
        });
        actions.append(&open_new);

        let inspect = context_menu_action(
            "document-properties-symbolic",
            "Структура и DDL… (Ctrl+Shift+D)",
        );
        let weak = Rc::downgrade(&ui);
        let selected = object.clone();
        let selected_profile = profile.clone();
        let close = popover.downgrade();
        inspect.connect_clicked(move |_| {
            if let Some(popover) = close.upgrade() {
                popover.popdown();
            }
            if let Some(ui) = weak.upgrade() {
                structure_ui::show(&ui, &selected_profile, &selected);
            }
        });
        actions.append(&inspect);

        if object.kind == database::DbObjectKind::Table {
            let import = context_menu_action("document-open-symbolic", "Импорт CSV в эту таблицу…");
            let owner = Rc::downgrade(&ui);
            let target = object.clone();
            let target_profile = profile.clone();
            let close = popover.downgrade();
            import.connect_clicked(move |_| {
                if let Some(popover) = close.upgrade() {
                    popover.popdown();
                }
                if let Some(ui) = owner.upgrade() {
                    csv_import_ui::choose(&ui, &target_profile, Some(target.clone()));
                }
            });
            actions.append(&import);
        }
        if ui.table_data.borrow().contains_key(&table_key) {
            let export_shown =
                context_menu_action("document-save-symbolic", "Экспорт показанных строк в CSV…");
            let weak = Rc::downgrade(&ui);
            let key = table_key.clone();
            let table_name = object.name.clone();
            let popover_close = popover.clone();
            export_shown.connect_clicked(move |_| {
                popover_close.popdown();
                if let Some(ui) = weak.upgrade() {
                    choose_table_csv_export(&ui, &key, &table_name);
                }
            });
            actions.append(&export_shown);
        }

        let export_all =
            context_menu_action("folder-download-symbolic", "Экспорт всей таблицы в CSV…");
        let weak = Rc::downgrade(&ui);
        let profile = profile.clone();
        let selected = object.clone();
        let table_name = object.name.clone();
        let popover_close = popover.clone();
        export_all.connect_clicked(move |_| {
            popover_close.popdown();
            if let Some(ui) = weak.upgrade() {
                choose_full_table_csv_export(&ui, &profile, &selected, &table_name);
            }
        });
        actions.append(&export_all);

        popover.set_child(Some(&actions));
        popover.set_parent(&anchor_widget);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            1,
            1,
        )));
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    anchor.add_controller(gesture);
}

fn context_menu_action(icon_name: &str, title: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("table-context-action");
    button.add_css_class("flat");
    button.set_halign(gtk::Align::Fill);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(20);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&icon);
    row.append(&label);
    button.set_child(Some(&row));
    button
}

fn show_sidebar(ui: &Ui, page: &str) {
    ui.sidebar_toggle.set_active(true);
    ui.sidebar_stack.set_visible_child_name(page);
    if page == "connections" {
        ui.connections_nav.add_css_class("sidebar-nav-active");
        ui.tables_nav.remove_css_class("sidebar-nav-active");
    } else {
        ui.tables_nav.add_css_class("sidebar-nav-active");
        ui.connections_nav.remove_css_class("sidebar-nav-active");
    }
    update_navigation_rail_icon(&ui.connections_nav, "database");
    update_navigation_rail_icon(&ui.tables_nav, "tables");
}

/// Кнопка "Таблицы" всегда активна; без каталога раздел показывает подсказку.
fn set_tables_nav_sensitive(ui: &Ui, available: bool) {
    ui.tables_hint.set_visible(!available);
    update_navigation_rail_icon(&ui.tables_nav, "tables");
}

fn show_delete_profile_dialog(ui: &Rc<Ui>, profile_id: &str, profile_name: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .buttons(gtk::ButtonsType::YesNo)
        .text(format!("Удалить подключение «{profile_name}»?"))
        .secondary_text(
            "Профиль, его пароль и сохранённые виды таблиц будут удалены с этого компьютера.",
        )
        .build();
    let weak = Rc::downgrade(ui);
    let profile_id = profile_id.to_owned();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response != gtk::ResponseType::Yes {
            return;
        }
        let Some(ui) = weak.upgrade() else { return };
        let result = ui
            .store
            .borrow_mut()
            .as_mut()
            .ok_or_else(|| "Хранилище настроек не загружено".to_owned())
            .and_then(|store| store.delete_profile(&profile_id));
        match result {
            Ok(()) => {
                ui.profile_status.borrow_mut().remove(&profile_id);
                close_profile_sessions(&ui, &profile_id);
                ui.status.set_text("Подключение удалено");
                refresh_profiles(&ui);
            }
            Err(error) => show_error_dialog(&ui, "Не удалось удалить подключение", &error),
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn ensure_vault_unlocked<F>(ui: &Rc<Ui>, after_unlock: F)
where
    F: FnOnce() + 'static,
{
    let state = ui
        .store
        .borrow()
        .as_ref()
        .map(|store| (store.vault_is_unlocked(), store.vault_exists()));
    match state {
        Some((true, _)) => after_unlock(),
        Some((false, exists)) => {
            if secret_store::remember_enabled() {
                storage_ui::auto_unlock(ui, after_unlock);
            } else {
                show_vault_dialog(ui, exists, after_unlock);
            }
        }
        None => show_error_dialog(
            ui,
            "Хранилище недоступно",
            "Не удалось загрузить настройки приложения.",
        ),
    }
}

fn settings_dialog_parent(ui: &Ui) -> gtk::Window {
    if ui.preferences.is_visible() {
        ui.preferences.clone().upcast()
    } else {
        ui.window.clone().upcast()
    }
}

fn show_vault_dialog<F>(ui: &Rc<Ui>, exists: bool, after_unlock: F)
where
    F: FnOnce() + 'static,
{
    let dialog = adw::Window::builder()
        .transient_for(&settings_dialog_parent(ui))
        .modal(true)
        .title(if exists {
            "Разблокировать хранилище"
        } else {
            "Создать хранилище паролей"
        })
        .default_width(440)
        .build();
    dialog.add_css_class("dbdeck-settings");
    dialog.set_decorated(true);
    dialog.set_deletable(true);
    add_escape_to_window(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(20);
    content.set_margin_end(20);
    let description = if exists {
        "Введите мастер-пароль, чтобы расшифровать сохранённые пароли подключений."
    } else {
        "Придумайте мастер-пароль для зашифрованного файла подключений. Он понадобится при переносе на другой компьютер. Восстановить его невозможно."
    };
    let label = gtk::Label::new(Some(description));
    label.set_xalign(0.0);
    label.set_wrap(true);
    content.append(&label);
    let remember = gtk::CheckButton::with_label(
        "Запомнить ключ в выбранном менеджере (настраивается в Общих)",
    );
    remember.set_active(secret_store::remember_enabled());
    let new_password_hint = format!(
        "Мастер-пароль (минимум {} символов)",
        settings::MASTER_PASSWORD_MIN_LENGTH
    );
    let master_password = gtk::PasswordEntry::builder()
        .placeholder_text(if exists {
            "Мастер-пароль"
        } else {
            &new_password_hint
        })
        .hexpand(true)
        .build();
    content.append(&master_password);
    let confirmation = if exists {
        None
    } else {
        let entry = gtk::PasswordEntry::builder()
            .placeholder_text("Повторите мастер-пароль")
            .hexpand(true)
            .build();
        content.append(&entry);
        Some(entry)
    };
    content.append(&remember);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Отмена");
    let unlock = gtk::Button::with_label(if exists {
        "Разблокировать"
    } else {
        "Создать"
    });
    unlock.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&unlock);
    content.append(&buttons);
    dialog.set_content(Some(&content));
    shortcuts::default_button(&dialog, &unlock);
    let dialog_close = dialog.clone();
    cancel.connect_clicked(move |_| dialog_close.close());
    let weak = Rc::downgrade(ui);
    let dialog_close = dialog.clone();
    let after_unlock = Rc::new(RefCell::new(Some(
        Box::new(after_unlock) as Box<dyn FnOnce()>
    )));
    let after_unlock_clicked = after_unlock.clone();
    unlock.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        let secret = zeroize::Zeroizing::new(master_password.text().to_string());
        if let Some(confirm) = &confirmation {
            if secret.as_str() != confirm.text().as_str() {
                show_error_dialog(
                    &ui,
                    "Мастер-пароли не совпадают",
                    "Повторите мастер-пароль одинаково в обоих полях.",
                );
                return;
            }
        }
        let result = ui
            .store
            .borrow_mut()
            .as_mut()
            .map(|store| store.unlock_or_create_vault(&secret));
        match result {
            Some(Ok(_)) => {
                storage_ui::unlocked(&ui);
                if remember.is_active() {
                    storage_ui::remember(&ui);
                }
                master_password.set_text("");
                if let Some(confirm) = &confirmation {
                    confirm.set_text("");
                }
                dialog_close.close();
                if let Some(callback) = after_unlock_clicked.borrow_mut().take() {
                    callback();
                }
            }
            Some(Err(error)) => show_error_dialog(&ui, "Не удалось открыть хранилище", &error),
            None => show_error_dialog(
                &ui,
                "Хранилище недоступно",
                "Не удалось загрузить настройки приложения.",
            ),
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn choose_backup_export(ui: &Rc<Ui>) {
    let chooser = dialogs::file_chooser(
        Some("Экспорт резервной копии DBDeck"),
        Some(&settings_dialog_parent(ui)),
        gtk::FileChooserAction::Save,
        Some("Сохранить"),
        Some("Отмена"),
    );
    chooser.set_current_name("zer0-dbdeck-backup.json");
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                let result = ui
                    .store
                    .borrow()
                    .as_ref()
                    .map(|store| store.export_backup(&path));
                match result {
                    Some(Ok(())) => ui
                        .status
                        .set_text("Зашифрованная резервная копия сохранена"),
                    Some(Err(error)) => {
                        show_error_dialog(&ui, "Не удалось экспортировать копию", &error)
                    }
                    None => show_error_dialog(
                        &ui,
                        "Хранилище недоступно",
                        "Не удалось загрузить настройки.",
                    ),
                }
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn choose_table_csv_export(ui: &Rc<Ui>, table_key: &str, table_name: &str) {
    let Some(data) = ui.table_data.borrow().get(table_key).cloned() else {
        show_error_dialog(ui, "Таблица ещё загружается", "Дождитесь загрузки данных.");
        return;
    };
    let chooser = dialogs::file_chooser(
        Some("Экспорт показанных строк в CSV"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить CSV"),
        Some("Отмена"),
    );
    configure_csv_chooser(ui, &chooser);
    chooser.set_current_name(&format!(
        "{}-shown.csv",
        safe_query_file_component(table_name)
    ));
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                remember_csv_directory(&ui, &path);
                let Some(cancel) = start_transfer(&ui, "Экспорт выборки в CSV", &path)
                else {
                    dialog.destroy();
                    return;
                };
                let sender = ui.sender.clone();
                let data = data.clone();
                let guard = ui.activity.start();
                ui.runtime.spawn_blocking(move || {
                    let _guard = guard;
                    let rows = data.rows.len();
                    let result = if cancel.is_cancelled() {
                        Err("Экспорт отменён".into())
                    } else {
                        write_snapshot_csv(&path, &data, &cancel, |count| {
                            let _ = sender.send(Event::ExportProgress(count));
                        })
                    };
                    let _ = sender.send(Event::CsvExported(rows, result));
                });
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn choose_full_table_csv_export(
    ui: &Rc<Ui>,
    profile: &ConnectionProfile,
    object: &DbObject,
    table_name: &str,
) {
    let session_key = profile_session_key(profile);
    let Some((_, pool)) = ui.sessions.borrow().get(&session_key).cloned() else {
        show_error_dialog(
            ui,
            "Нет активного подключения",
            "Подключитесь к базе данных.",
        );
        return;
    };
    let chooser = dialogs::file_chooser(
        Some("Экспорт всей таблицы в CSV"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Сохранить CSV"),
        Some("Отмена"),
    );
    configure_csv_chooser(ui, &chooser);
    chooser.set_current_name(&format!(
        "{}-all.csv",
        safe_query_file_component(table_name)
    ));
    let weak = Rc::downgrade(ui);
    let object = object.clone();
    let table_name = table_name.to_owned();
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                remember_csv_directory(&ui, &path);
                ui.status
                    .set_text(&format!("Экспортирую всю таблицу {table_name}…"));
                let sender = ui.sender.clone();
                let runtime = ui.runtime.clone();
                let pool = pool.clone();
                let object = object.clone();
                let Some(cancel) = start_transfer(&ui, "Экспорт таблицы в CSV", &path)
                else {
                    dialog.destroy();
                    return;
                };
                let task_runtime = runtime.clone();
                let guard = ui.activity.start();
                runtime.spawn_blocking(move || {
                    let _guard = guard;
                    let progress = sender.clone();
                    let result = task_runtime.block_on(write_full_table_csv(
                        &path,
                        &pool,
                        &object,
                        &cancel,
                        move |count| {
                            let _ = progress.send(Event::ExportProgress(count));
                        },
                    ));
                    let event = match result {
                        Ok(rows) => (rows, Ok(())),
                        Err(error) => (0, Err(error)),
                    };
                    let _ = sender.send(Event::CsvExported(event.0, event.1));
                });
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

#[cfg(test)]
fn write_table_csv(path: &std::path::Path, data: &TableData) -> Result<(), String> {
    write_snapshot_csv(path, data, &transfer::Cancellation::new(), |_| {})
}
fn write_snapshot_csv(
    path: &std::path::Path,
    data: &TableData,
    cancelled: &transfer::Cancellation,
    mut progress: impl FnMut(usize),
) -> Result<(), String> {
    use std::io::Write;
    let mut output = output_file::AtomicOutput::new(path)?;
    {
        let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, &mut output.file);
        write_csv_header(&mut writer, &data.columns)?;
        for (index, row) in data.rows.iter().enumerate() {
            if cancelled.is_cancelled() {
                return Err("Экспорт отменён".into());
            }
            write_csv_row(&mut writer, row)?;
            if (index + 1) % 1000 == 0 {
                progress(index + 1);
            }
        }
        writer
            .flush()
            .map_err(|e| format!("Ошибка записи CSV: {e}"))?;
    }
    output.commit(cancelled)
}
async fn write_full_table_csv(
    path: &std::path::Path,
    pool: &DbPool,
    object: &DbObject,
    cancelled: &transfer::Cancellation,
    progress: impl FnMut(usize) + Send,
) -> Result<usize, String> {
    let mut output = output_file::AtomicOutput::new(path)?;
    let rows = pool
        .export_table_csv_controlled(object, &mut output.file, cancelled.flag(), progress)
        .await?;
    output.commit(cancelled)?;
    Ok(rows)
}

fn choose_backup_import(ui: &Rc<Ui>) {
    let chooser = dialogs::file_chooser(
        Some("Импорт резервной копии DBDeck"),
        Some(&settings_dialog_parent(ui)),
        gtk::FileChooserAction::Open,
        Some("Открыть"),
        Some("Отмена"),
    );
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                show_backup_password_dialog(&ui, path);
            }
        }
        dialog.destroy();
    });
    chooser.show();
}

fn show_backup_password_dialog(ui: &Rc<Ui>, path: std::path::PathBuf) {
    let dialog = adw::Window::builder()
        .transient_for(&settings_dialog_parent(ui))
        .modal(true)
        .title("Пароль резервной копии")
        .default_width(440)
        .build();
    dialog.add_css_class("dbdeck-settings");
    dialog.set_decorated(true);
    dialog.set_deletable(true);
    add_escape_to_window(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(20);
    content.set_margin_end(20);
    let instruction = gtk::Label::new(Some(
        "Введите мастер-пароль, которым была защищена эта резервная копия.",
    ));
    instruction.set_xalign(0.0);
    instruction.set_wrap(true);
    content.append(&instruction);
    let password = gtk::PasswordEntry::builder()
        .placeholder_text("Мастер-пароль резервной копии")
        .hexpand(true)
        .build();
    content.append(&password);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Отмена");
    let import = gtk::Button::with_label("Импортировать");
    import.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&import);
    content.append(&actions);
    dialog.set_content(Some(&content));
    shortcuts::default_button(&dialog, &import);
    let close = dialog.clone();
    cancel.connect_clicked(move |_| close.close());
    let weak = Rc::downgrade(ui);
    let close = dialog.clone();
    import.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        if !ui.sessions.borrow().is_empty(){show_error_dialog(&ui,"Сначала отключите подключения","Импорт профилей может изменить реквизиты и режим доступа. Завершите транзакции и отключите подключения перед импортом.");return;}
        let master_password = password.text().to_string();
        let result = ui
            .store
            .borrow_mut()
            .as_mut()
            .map(|store| store.import_backup(&path, &master_password));
        match result {
            Some(Ok(())) => {
                refresh_profiles(&ui);
                ui.status
                    .set_text("Подключения импортированы из резервной копии");
                close.close();
            }
            Some(Err(error)) => show_error_dialog(&ui, "Не удалось импортировать копию", &error),
            None => show_error_dialog(
                &ui,
                "Хранилище недоступно",
                "Не удалось загрузить настройки.",
            ),
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn show_connection_form(ui: &Rc<Ui>, existing: Option<ConnectionProfile>) {
    ui.test_generation
        .set(ui.test_generation.get().wrapping_add(1));
    *ui.last_test.borrow_mut() = None;
    let dialog = adw::Window::builder()
        .transient_for(&ui.window)
        .modal(true)
        .title(if existing.is_some() {
            "Изменить подключение"
        } else {
            "Новое подключение"
        })
        .build();
    dialog.set_decorated(true);
    dialog.set_deletable(true);
    add_escape_to_window(&dialog);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(20);
    content.set_margin_end(20);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(10);
    grid.set_column_spacing(12);
    let name = gtk::Entry::builder()
        .placeholder_text("Например, Production")
        .hexpand(true)
        .build();
    let kind = gtk::DropDown::from_strings(&["PostgreSQL", "MySQL", "MariaDB"]);
    let host = gtk::Entry::builder()
        .text("127.0.0.1")
        .hexpand(true)
        .build();
    let port = gtk::SpinButton::with_range(1.0, 65535.0, 1.0);
    port.set_value(5432.0);
    let database = gtk::Entry::builder()
        .placeholder_text("Необязательно")
        .hexpand(true)
        .build();
    database.set_tooltip_text(Some(
        "MySQL/MariaDB: оставьте пустым, чтобы видеть все доступные базы; в SQL указывайте база.таблица. PostgreSQL: оставьте пустым для списка доступных баз; выберите базу, чтобы открыть её схемы.",
    ));
    let username = gtk::Entry::builder()
        .placeholder_text("Пользователь")
        .hexpand(true)
        .build();
    let password = gtk::PasswordEntry::builder()
        .placeholder_text(if existing.is_some() {
            "Пусто — оставить текущий пароль"
        } else {
            "Пароль"
        })
        .hexpand(true)
        .build();
    let environment = gtk::DropDown::from_strings(&["local", "stage", "prod"]);
    let tls_mode = gtk::DropDown::from_strings(&[
        TlsMode::VerifyFull.label(),
        TlsMode::Require.label(),
        TlsMode::Disabled.label(),
    ]);
    let color_selection = Rc::new(Cell::new(0_usize));
    let color_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let mut color_buttons = Vec::new();
    for (label, hex) in connection_colors().iter() {
        let button = gtk::ToggleButton::new();
        let swatch = gtk::Label::new(None);
        swatch.set_markup(&format!("<span foreground=\"{hex}\">●</span>"));
        button.set_child(Some(&swatch));
        button.set_tooltip_text(Some(label));
        button.add_css_class("color-option");
        if let Some(first) = color_buttons.first() {
            button.set_group(Some(first));
        }
        color_row.append(&button);
        color_buttons.push(button);
    }
    for (index, button) in color_buttons.iter().enumerate() {
        let selection = color_selection.clone();
        let buttons = color_buttons.clone();
        button.connect_toggled(move |selected| {
            if !selected.is_active() {
                return;
            }
            selection.set(index);
            for (button_index, button) in buttons.iter().enumerate() {
                if button_index == index {
                    button.add_css_class("color-option-selected");
                } else {
                    button.remove_css_class("color-option-selected");
                }
            }
        });
    }
    if let Some(first) = color_buttons.first() {
        first.set_active(true);
    }
    let gopass_entry = gtk::Entry::builder()
        .placeholder_text("Необязательно: databases/production")
        .hexpand(true)
        .build();
    if let Some(entry) = existing
        .as_ref()
        .and_then(|profile| profile.gopass_entry.as_deref())
    {
        gopass_entry.set_text(entry);
        password.set_sensitive(false);
    }
    let source_password = password.clone();
    gopass_entry.connect_changed(move |entry| {
        source_password.set_sensitive(entry.text().trim().is_empty());
    });
    let read_only =
        gtk::CheckButton::with_label("Только просмотр (не изменяет права пользователя БД)");
    attach_form_row(&grid, 0, "Имя", &name);
    attach_form_row(&grid, 1, "Движок", &kind);
    attach_form_row(&grid, 2, "Хост", &host);
    attach_form_row(&grid, 3, "Порт", &port);
    attach_form_row(&grid, 4, "База данных", &database);
    attach_form_row(&grid, 5, "Пользователь", &username);
    attach_form_row(&grid, 6, "Пароль", &password);
    attach_form_row(&grid, 7, "Среда", &environment);
    attach_form_row(&grid, 8, "Защита соединения", &tls_mode);
    attach_form_row(&grid, 9, "Запись gopass", &gopass_entry);
    let color_label = gtk::Label::new(Some("Цвет подключения"));
    color_label.set_xalign(0.0);
    grid.attach(&color_label, 0, 10, 1, 1);
    grid.attach(&color_row, 1, 10, 1, 1);
    grid.attach(&read_only, 0, 11, 2, 1);
    let tls_files = connection_tls_ui::Fields::new(&grid, 12);
    content.append(&grid);
    let password_note = gtk::Label::new(Some(if existing.is_some() {
        "Оставьте пароль пустым, чтобы сохранить текущий. Новый пароль будет зашифрован в локальном хранилище."
    } else {
        "Подключение и пароль зашифрованы вместе. Если указана запись gopass, пароль читается из неё и не дублируется в DBDeck."
    }));
    password_note.set_xalign(0.0);
    password_note.set_wrap(true);
    password_note.set_max_width_chars(72);
    password_note.add_css_class("dim-label");
    content.append(&password_note);
    let test_feedback = gtk::Label::new(Some("Проверка соединения ещё не запускалась"));
    test_feedback.set_xalign(0.0);
    test_feedback.set_wrap(true);
    test_feedback.add_css_class("test-feedback");
    content.append(&dialogs::copyable_label(&test_feedback));
    *ui.test_feedback.borrow_mut() = Some(test_feedback);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let test = gtk::Button::with_label("Проверить");
    let save = gtk::Button::with_label("Сохранить");
    save.add_css_class("suggested-action");
    buttons.append(&test);
    buttons.append(&save);
    content.append(&buttons);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&content)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .vexpand(true)
        .build();
    dialog.set_content(Some(&scroll));
    shortcuts::default_button(&dialog, &save);
    shortcuts::button(&dialog, &save, "<Control>s");
    shortcuts::button(&dialog, &test, "<Control>Return");
    let port_keys = gtk::EventControllerKey::new();
    port_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let port_value = port.downgrade();
    let default_save = save.downgrade();
    port_keys.connect_key_pressed(move |_, key, _, state| {
        let modifiers = gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK;
        if (state & modifiers).is_empty()
            && matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
        {
            if let Some(port) = port_value.upgrade() {
                port.update();
            }
            if let Some(save) = default_save.upgrade() {
                save.emit_clicked();
            }
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    port.add_controller(port_keys);
    let port_clone = port.clone();
    kind.connect_selected_notify(move |kind| {
        let database_kind = if kind.selected() == 0 {
            DatabaseKind::PostgreSql
        } else {
            DatabaseKind::MySql
        };
        port_clone.set_value(f64::from(database_kind.default_port()));
    });
    let safe_default = read_only.clone();
    environment.connect_selected_notify(move |environment| {
        if environment.selected() == 2 {
            safe_default.set_active(true);
        }
    });
    if let Some(profile) = &existing {
        name.set_text(&profile.name);
        kind.set_selected(match profile.kind {
            DatabaseKind::PostgreSql => 0,
            DatabaseKind::MySql => 1,
            DatabaseKind::MariaDb => 2,
        });
        host.set_text(&profile.host);
        port.set_value(f64::from(profile.port));
        database.set_text(&profile.database);
        username.set_text(&profile.username);
        environment.set_selected(match profile.environment {
            EnvironmentTag::Local => 0,
            EnvironmentTag::Stage => 1,
            EnvironmentTag::Prod => 2,
        });
        tls_mode.set_selected(match profile.tls_mode {
            TlsMode::VerifyFull => 0,
            TlsMode::Require => 1,
            TlsMode::Disabled => 2,
        });
        read_only.set_active(profile.read_only);
        tls_files.load(&profile.tls_files);
        tls_files.load_tools(profile.native_tools_dir.as_deref());
        let selected_color = connection_colors()
            .iter()
            .position(|(_, hex)| *hex == profile.color)
            .unwrap_or(0);
        color_selection.set(selected_color);
        if let Some(button) = color_buttons.get(selected_color) {
            button.set_active(true);
        }
    }
    let weak = Rc::downgrade(ui);
    let name_test = name.clone();
    let kind_test = kind.clone();
    let host_test = host.clone();
    let port_test = port.clone();
    let database_test = database.clone();
    let username_test = username.clone();
    let environment_test = environment.clone();
    let tls_mode_test = tls_mode.clone();
    let tls_files_test = tls_files.clone();
    let color_test = color_selection.clone();
    let read_only_test = read_only.clone();
    let password_test = password.clone();
    let gopass_test = gopass_entry.clone();
    test.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut profile = match profile_from_form(
            &name_test,
            &kind_test,
            &host_test,
            &port_test,
            &database_test,
            &username_test,
            &environment_test,
            &tls_mode_test,
            &tls_files_test,
            color_test.get(),
            &read_only_test,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                ui.status.set_text(&error);
                show_error_dialog(&ui, "Проверьте поля подключения", &error);
                return;
            }
        };
        set_feedback(&ui, "Проверяю подключение…", "status-pending");
        let entry = gopass_test.text().trim().to_owned();
        if !entry.is_empty() {
            if let Err(error) = secret_store::validate_entry(&entry) {
                show_error_dialog(&ui, "Проверьте gopass", &error);
                return;
            }
            profile.gopass_entry = Some(entry);
        }
        let password_text = password_test.text().to_string();
        let test_generation = ui.test_generation.get().wrapping_add(1);
        ui.test_generation.set(test_generation);
        let connection_generation = ui.connection_generation.get();
        let sender = ui.sender.clone();
        let runtime = ui.runtime.clone();
        let guard = ui.activity.start();
        runtime.spawn(async move {
            let _guard = guard;
            let result = match resolve_profile_password(&profile, password_text).await {
                Ok(secret) => match DbPool::connect(&profile, &secret).await {
                    Ok(pool) => pool.check().await.map_err(String::from),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            let _ = sender.send(Event::Tested(
                test_generation,
                connection_generation,
                result,
            ));
        });
    });
    let weak = Rc::downgrade(ui);
    let dialog_save = dialog.clone();
    save.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut profile = match profile_from_form(
            &name,
            &kind,
            &host,
            &port,
            &database,
            &username,
            &environment,
            &tls_mode,
            &tls_files,
            color_selection.get(),
            &read_only,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                ui.status.set_text(&error);
                show_error_dialog(&ui, "Проверьте поля подключения", &error);
                return;
            }
        };
        let source = gopass_entry.text().trim().to_owned();
        if !source.is_empty() {
            if let Err(error) = secret_store::validate_entry(&source) {
                show_error_dialog(&ui, "Проверьте gopass", &error);
                return;
            }
            profile.gopass_entry = Some(source);
        }
        if let Some(existing) = &existing {
            profile.id = existing.id.clone();
        } else {
            profile.id = Uuid::new_v4().to_string();
        }
        if let Some(existing)=&existing {
            let changed=profile_session_key(existing)!=profile_session_key(&profile) || existing.read_only!=profile.read_only || std::mem::discriminant(&existing.environment)!=std::mem::discriminant(&profile.environment) || existing.gopass_entry!=profile.gopass_entry || !password.text().is_empty();
            if changed && is_profile_connected(&ui,&existing.id) {show_error_dialog(&ui,"Сначала завершите подключение","Завершите транзакции, сохраните правки и отключите это подключение перед изменением реквизитов, TLS или режима доступа.");return;}
        }
        if let Some(result) = ui.last_test.borrow().clone() {
            let profile_state = match result {
                Ok(()) => ProfileStatus::Verified,
                Err(error) => ProfileStatus::Error(format!("Проверка: {error}")),
            };
            ui.profile_status
                .borrow_mut()
                .insert(profile.id.clone(), profile_state);
        }
        ui.test_generation
            .set(ui.test_generation.get().wrapping_add(1));
        *ui.last_test.borrow_mut() = None;
        let secret = password.text().to_string();
        let result = ui.store.borrow_mut().as_mut().map(|store| {
            let result = if let Some(_existing) = &existing {
                store.update_profile(profile, (!secret.is_empty()).then_some(secret.as_str()))
            } else if secret.is_empty() && profile.gopass_entry.is_none() {
                return Err("Введите пароль, чтобы сохранить его для этого подключения.".to_owned());
            } else {
                store.save_profile(profile, &secret)
            };
            result.map_err(|error| match error {
                ProfileSaveError::Secret(error) | ProfileSaveError::Settings(error) => error,
            })
        });
        match result {
            Some(Ok(())) => {
                ui.status.set_text("Подключение сохранено");
                refresh_profiles(&ui);
                dialog_save.close();
            }
            Some(Err(error)) => {
                ui.objects.set_sensitive(true);
                ui.status.set_text(&error);
                show_error_dialog(&ui, "Не удалось сохранить подключение", &error);
            }
            None => show_error_dialog(
                &ui,
                "Не удалось сохранить подключение",
                "Не удалось загрузить хранилище настроек.",
            ),
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn profile_from_form(
    name: &gtk::Entry,
    kind: &gtk::DropDown,
    host: &gtk::Entry,
    port: &gtk::SpinButton,
    database: &gtk::Entry,
    username: &gtk::Entry,
    environment: &gtk::DropDown,
    tls_mode: &gtk::DropDown,
    tls_files: &connection_tls_ui::Fields,
    color_index: usize,
    read_only: &gtk::CheckButton,
) -> Result<ConnectionProfile, String> {
    let name_text = name.text().trim().to_owned();
    let host_text = host.text().trim().to_owned();
    let user_text = username.text().trim().to_owned();
    if name_text.is_empty() || host_text.is_empty() || user_text.is_empty() {
        return Err("Заполните имя, хост и пользователя".into());
    }
    let color_text = connection_colors()
        .get(color_index)
        .map(|(_, hex)| *hex)
        .unwrap_or("#4f8fe8")
        .to_owned();
    let profile = ConnectionProfile {
        id: String::new(),
        name: name_text,
        kind: match kind.selected() {
            0 => DatabaseKind::PostgreSql,
            2 => DatabaseKind::MariaDb,
            _ => DatabaseKind::MySql,
        },
        host: host_text,
        port: port.value_as_int() as u16,
        database: database.text().trim().to_owned(),
        username: user_text,
        environment: match environment.selected() {
            1 => EnvironmentTag::Stage,
            2 => EnvironmentTag::Prod,
            _ => EnvironmentTag::Local,
        },
        color: color_text,
        read_only: read_only.is_active(),
        tls_mode: match tls_mode.selected() {
            1 => TlsMode::Require,
            2 => TlsMode::Disabled,
            _ => TlsMode::VerifyFull,
        },
        password_saved: false,
        gopass_entry: None,
        tls_files: tls_files.values(),
        native_tools_dir: tls_files.tools_dir(),
    };
    profile.validate_transport()?;
    Ok(profile)
}

fn connection_colors() -> [(&'static str, &'static str); 7] {
    theme::connection_colors()
}

fn profile_color_class(color: &str) -> &'static str {
    match color {
        "#42b8b0" => "connection-color-teal",
        "#55b879" => "connection-color-green",
        "#e4b84f" => "connection-color-yellow",
        "#e58b4f" => "connection-color-orange",
        "#df626b" => "connection-color-red",
        "#a57be8" => "connection-color-purple",
        _ => "connection-color-blue",
    }
}

fn transport_summary(profile: &ConnectionProfile) -> &'static str {
    match profile.tls_mode {
        TlsMode::VerifyFull => "TLS: проверка сертификата и имени",
        TlsMode::Require => "TLS: сертификат НЕ проверяется",
        TlsMode::Disabled => "БЕЗ TLS",
    }
}

fn set_profile_accent(widget: &impl IsA<gtk::Widget>, color: &str) {
    for class in [
        "connection-color-blue",
        "connection-color-teal",
        "connection-color-green",
        "connection-color-yellow",
        "connection-color-orange",
        "connection-color-red",
        "connection-color-purple",
    ] {
        widget.as_ref().remove_css_class(class);
    }
    widget.as_ref().add_css_class(profile_color_class(color));
}

fn attach_form_row(grid: &gtk::Grid, row: i32, caption: &str, widget: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::new(Some(caption));
    label.set_xalign(0.0);
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

fn show_error_dialog(ui: &Rc<Ui>, title: &str, message: &str) {
    show_message_dialog(ui, gtk::MessageType::Error, title, message);
}

fn show_message_dialog(ui: &Rc<Ui>, kind: gtk::MessageType, title: &str, message: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(kind)
        .buttons(gtk::ButtonsType::Close)
        .text(title)
        .secondary_text(message)
        .build();
    dialog.set_deletable(true);
    dialog.connect_response(|dialog, _| dialog.close());
    dialogs::install(&dialog);
    dialog.present();
}

fn add_escape_to_window(window: &adw::Window) {
    dialogs::install(window);
}

fn connect_profile(ui: &Rc<Ui>, profile: ConnectionProfile) {
    let ui_after_unlock = ui.clone();
    ensure_vault_unlocked(ui, move || {
        connect_profile_unlocked(&ui_after_unlock, profile)
    });
}

fn connect_profile_unlocked(ui: &Rc<Ui>, profile: ConnectionProfile) {
    if profile.gopass_entry.is_some() {
        connect_profile_with_password(ui, profile, String::new());
        return;
    }
    let password_result = if profile.password_saved {
        ui.store
            .borrow()
            .as_ref()
            .map(|store| store.password(&profile.id))
            .unwrap_or_else(|| Ok(String::new()))
    } else {
        Ok(String::new())
    };
    let password = match password_result {
        Ok(password) if !password.is_empty() => password,
        Ok(_) => {
            show_password_prompt(ui, profile, None);
            return;
        }
        Err(error) => {
            show_password_prompt(ui, profile, Some(error));
            return;
        }
    };
    connect_profile_with_password(ui, profile, password);
}

fn show_password_prompt(ui: &Rc<Ui>, profile: ConnectionProfile, warning: Option<String>) {
    let dialog = adw::Window::builder()
        .transient_for(&ui.window)
        .modal(true)
        .title(format!("Пароль: {}", profile.name))
        .default_width(420)
        .build();
    dialog.set_decorated(true);
    dialog.set_deletable(true);
    add_escape_to_window(&dialog);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(20);
    content.set_margin_end(20);
    if let Some(warning) = warning {
        let label = gtk::Label::new(Some(&format!(
            "Не удалось прочитать сохранённый пароль. Введите его вручную.\n{warning}"
        )));
        label.set_wrap(true);
        label.set_xalign(0.0);
        content.append(&dialogs::copyable_label(&label));
    } else {
        content.append(&gtk::Label::new(Some("Введите пароль для подключения.")));
    }
    let password = gtk::PasswordEntry::builder()
        .placeholder_text("Пароль")
        .hexpand(true)
        .build();
    content.append(&password);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Отмена");
    let connect = gtk::Button::with_label("Подключиться");
    connect.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&connect);
    content.append(&buttons);
    dialog.set_content(Some(&content));
    shortcuts::default_button(&dialog, &connect);

    let dialog_close = dialog.clone();
    cancel.connect_clicked(move |_| dialog_close.close());
    let weak = Rc::downgrade(ui);
    let dialog_close = dialog.clone();
    connect.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let secret = password.text().to_string();
            if secret.is_empty() {
                show_error_dialog(
                    &ui,
                    "Введите пароль",
                    "Пароль можно будет повторно использовать после сохранения в зашифрованное хранилище.",
                );
                return;
            }
            let save_result = ui.store.borrow_mut().as_mut().map(|store| {
                store
                    .save_password(&profile.id, &secret)
                    .map_err(|error| match error {
                        ProfileSaveError::Secret(error) | ProfileSaveError::Settings(error) => {
                            error
                        }
                    })
            });
            match save_result {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    show_error_dialog(&ui, "Не удалось сохранить пароль", &error);
                    return;
                }
                None => {
                    show_error_dialog(
                        &ui,
                        "Не удалось сохранить пароль",
                        "Хранилище настроек не загружено.",
                    );
                    return;
                }
            }
            let mut profile = profile.clone();
            profile.password_saved = true;
            refresh_profiles(&ui);
            dialog_close.close();
            connect_profile_with_password(&ui, profile, secret);
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

async fn resolve_profile_password(
    profile: &ConnectionProfile,
    local: String,
) -> Result<zeroize::Zeroizing<String>, String> {
    if let Some(entry) = profile.gopass_entry.clone() {
        let secret =
            tokio::task::spawn_blocking(move || secret_store::Cli::default().password(&entry))
                .await
                .map_err(|_| "Не удалось прочитать gopass")??;
        Ok(zeroize::Zeroizing::new(secret.text()?.to_owned()))
    } else {
        Ok(zeroize::Zeroizing::new(local))
    }
}

fn connect_profile_with_password(ui: &Rc<Ui>, profile: ConnectionProfile, password: String) {
    ui.profile_status
        .borrow_mut()
        .insert(profile.id.clone(), ProfileStatus::Connecting);
    refresh_profiles(ui);
    ui.status
        .set_text(&format!("Подключаюсь к {}…", profile.name));
    let session_key = profile_session_key(&profile);
    let request_id = Uuid::new_v4().to_string();
    ui.connect_requests
        .borrow_mut()
        .insert(session_key, request_id.clone());
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let result = match resolve_profile_password(&profile, password).await {
            Ok(secret) => DbPool::connect(&profile, &secret).await,
            Err(error) => Err(error),
        };
        let _ = sender.send(Event::Connected(request_id, profile, result));
    });
}

fn set_feedback(ui: &Ui, message: &str, state_class: &str) {
    if let Some(label) = ui.test_feedback.borrow().as_ref() {
        label.set_text(message);
        for class in ["status-pending", "status-success", "status-error"] {
            label.remove_css_class(class);
        }
        label.add_css_class(state_class);
    }
}

fn show_system_schemas(ui: &Ui) -> bool {
    ui.store
        .borrow()
        .as_ref()
        .is_some_and(SettingsStore::show_system_schemas)
}

fn show_table_catalog(ui: &Rc<Ui>) {
    let (Some(pool), Some(profile)) = (
        ui.pool.borrow().clone(),
        ui.current_profile.borrow().clone(),
    ) else {
        return;
    };
    namespace_ui::refresh(ui);
    ui.status.set_text("Загружаю таблицы и представления…");
    if matches!(profile.kind, DatabaseKind::PostgreSql) {
        catalog_tree::prepare(ui, &profile);
    } else {
        ui.objects.set_sensitive(false);
        clear_box(&ui.objects);
        ui.object_search_branches.borrow_mut().clear();
        ui.objects.append(&loading_row("Загружаю таблицы…"));
        ui.table_list_items.borrow_mut().clear();
        ui.selected_table_button.borrow_mut().take();
    }
    let generation = ui.connection_generation.get();
    let view_generation = ui.view_generation.get().wrapping_add(1);
    ui.view_generation.set(view_generation);
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let session_key = profile_session_key(&profile);
    // A MySQL database opened from the server tree keeps listing the saved scope.
    let mut catalog_scope = profile.clone();
    if !matches!(profile.kind, DatabaseKind::PostgreSql) {
        if let Some(saved) = ui
            .store
            .borrow()
            .as_ref()
            .and_then(|store| store.profiles.iter().find(|p| p.id == profile.id))
        {
            catalog_scope.database = saved.database.clone();
        }
    }
    let include_system = show_system_schemas(ui);
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let event = if matches!(profile.kind, DatabaseKind::PostgreSql)
            && profile.database.trim().is_empty()
        {
            let result = pool.postgres_databases().await;
            Event::DatabasesLoaded(
                generation,
                view_generation,
                report_lost(&pool, &sender, &session_key, result).await,
            )
        } else {
            let result = pool
                .list_tables_with_system(&catalog_scope, include_system)
                .await;
            Event::TablesLoaded(
                generation,
                view_generation,
                report_lost(&pool, &sender, &session_key, result).await,
            )
        };
        let _ = sender.send(event);
    });
}

fn open_database_session(
    ui: &Rc<Ui>,
    mut profile: ConnectionProfile,
    pool: DbPool,
    database: &str,
) {
    profile.database = database.to_owned();
    let key = profile_session_key(&profile);
    if ui.sessions.borrow().contains_key(&key) {
        select_session(ui, &key);
        return;
    }
    if ui.connect_requests.borrow().contains_key(&key) {
        *ui.desired_database_session.borrow_mut() = Some(key);
        return;
    }
    if ui.sessions.borrow().len() + ui.pending_database_opens.borrow().len() >= 16 {
        ui.status.set_text("Достигнут лимит 16 открытых/открываемых подключений. Закройте ненужную вкладку подключения.");
        return;
    }
    if matches!(
        ui.profile_status.borrow().get(&profile.id),
        Some(ProfileStatus::Broken)
    ) {
        queue_retry(ui, &profile.id, RetryAction::ExpandNode(profile.clone()));
        show_connection_banner(ui, &profile.id);
        return;
    }
    *ui.desired_database_session.borrow_mut() = Some(key.clone());
    ui.pending_database_opens.borrow_mut().insert(key.clone());
    let request = Uuid::new_v4().to_string();
    ui.connect_requests
        .borrow_mut()
        .insert(key, request.clone());
    ui.status.set_text(&format!("Открываю базу {database}…"));
    let sender = ui.sender.clone();
    let guard = ui.activity.start();
    ui.runtime.spawn(async move {
        let _guard = guard;
        let result = pool.connect_database(&profile.database).await;
        let _ = sender.send(Event::Connected(request, profile, result));
    });
}

fn with_database_session(
    ui: &Rc<Ui>,
    profile: ConnectionProfile,
    pool: DbPool,
    action: impl FnOnce(&Rc<Ui>, &str) + 'static,
) {
    let key = profile_session_key(&profile);
    let database = profile.database.clone();
    open_database_session(ui, profile, pool, &database);
    let owner = Rc::downgrade(ui);
    let mut action = Some(action);
    gtk::glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(ui) = owner.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if ui.desired_database_session.borrow().as_deref() != Some(&key) {
            return gtk::glib::ControlFlow::Break;
        }
        if ui
            .current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| profile_session_key(p) == key)
        {
            if let Some(action) = action.take() {
                action(&ui, &key);
            }
            return gtk::glib::ControlFlow::Break;
        }
        if ui.connect_requests.borrow().contains_key(&key) {
            gtk::glib::ControlFlow::Continue
        } else {
            gtk::glib::ControlFlow::Break
        }
    });
}

fn append_schema_tree(
    ui: &Rc<Ui>,
    parent: &gtk::Box,
    profile: &ConnectionProfile,
    catalog: TableCatalog,
) {
    let session_key = profile_session_key(profile);
    let truncated = catalog.truncated;
    let schemas = catalog.schemas;
    let preferred_schema = schemas
        .iter()
        .find(|schema| !schema.starts_with("pg_") && *schema != "information_schema")
        .cloned();
    let mut objects_by_schema = catalog.objects.into_iter().fold(
        HashMap::<String, Vec<DbObject>>::new(),
        |mut grouped, object| {
            grouped
                .entry(object.schema.clone())
                .or_default()
                .push(object);
            grouped
        },
    );
    let database_group = !matches!(profile.kind, DatabaseKind::PostgreSql);
    for schema in schemas {
        let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
        children.set_margin_start(24);
        let expanded = ui
            .catalog_tree
            .borrow()
            .schema_expanded
            .get(&(session_key.clone(), schema.clone()))
            .copied()
            .unwrap_or(if database_group {
                if profile.database.is_empty() {
                    preferred_schema.as_deref() == Some(schema.as_str())
                } else {
                    schema == profile.database
                }
            } else {
                schema == "public" || preferred_schema.as_deref() == Some(schema.as_str())
            });
        children.set_visible(expanded);
        let disclosure = gtk::Label::new(Some(if expanded { "▾" } else { "▸" }));
        disclosure.set_width_chars(2);
        let open_icon = if database_group {
            "database-open"
        } else {
            "folder-open"
        };
        let closed_icon = if database_group {
            "database-closed"
        } else {
            "folder-closed"
        };
        let folder = penpot_icon_image(if expanded { open_icon } else { closed_icon });
        folder.remove_css_class("penpot-icon");
        folder.set_pixel_size(16);
        folder.set_size_request(16, 16);
        let label = gtk::Label::new(Some(&schema));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.append(&disclosure);
        row.append(&folder);
        row.append(&label);
        let schema_button = gtk::Button::new();
        schema_button.set_child(Some(&row));
        schema_button.set_halign(gtk::Align::Fill);
        schema_button.add_css_class("flat");
        schema_button.add_css_class("schema-tree-row");
        schema_button.set_hexpand(true);
        if database_group {
            schema_button.set_tooltip_text(Some(&format!("Открыть базу {schema}")));
            if schema == profile.database {
                schema_button.add_css_class("schema-tree-row-active");
            }
        } else {
            schema_button.set_tooltip_text(Some(&format!("{}: {}", profile.database, schema)));
        }
        let user_expanded = Rc::new(Cell::new(expanded));
        let expanded_for_toggle = user_expanded.clone();
        let children_for_toggle = children.clone();
        let disclosure_for_toggle = disclosure.clone();
        let folder_for_toggle = folder.clone();
        let weak = Rc::downgrade(ui);
        let selected_schema_button = schema_button.downgrade();
        let schema_identity = (session_key.clone(), schema.clone());
        let profile_for_group = profile.clone();
        // A MySQL/MariaDB database is its own session, so its group belongs to that database.
        let group_profile = if database_group {
            let mut group_profile = profile.clone();
            group_profile.database = schema.clone();
            group_profile
        } else {
            profile.clone()
        };
        let open_requested = Rc::new(Cell::new(false));
        schema_button.connect_clicked(move |_| {
            let group_active = weak.upgrade().is_some_and(|ui| {
                let key = workspace_ui::key(&group_profile, Some(schema_identity.1.as_str()));
                ui.workspace_groups.borrow().contains_key(&key)
                    && workspace_ui::is_active(&ui, &key)
            });
            // First click on an inactive schema opens it; later clicks toggle,
            // even when opening the group did not succeed.
            let expanded =
                (!group_active && !open_requested.replace(true)) || !expanded_for_toggle.get();
            expanded_for_toggle.set(expanded);
            children_for_toggle.set_visible(expanded);
            disclosure_for_toggle.set_text(if expanded { "▾" } else { "▸" });
            set_penpot_icon(
                &folder_for_toggle,
                if expanded { open_icon } else { closed_icon },
                false,
            );
            if let Some(ui) = weak.upgrade() {
                ui.catalog_tree
                    .borrow_mut()
                    .schema_expanded
                    .insert(schema_identity.clone(), expanded);
                if let Some(button) = selected_schema_button.upgrade() {
                    select_schema_button(&ui, &button);
                }
                if !group_active {
                    open_schema_group(&ui, &profile_for_group, &schema_identity.1);
                }
                apply_table_list_filter(&ui);
            }
        });
        let sql_button = tree_sql_button(&if database_group {
            schema.clone()
        } else {
            format!("{}.{}", profile.database, schema)
        });
        let weak = Rc::downgrade(ui);
        let profile_for_sql = profile.clone();
        let schema_for_sql = schema.clone();
        sql_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                open_sql_in_schema(&ui, &profile_for_sql, &schema_for_sql);
            }
        });
        let line = tree_line(&schema_button, &sql_button);
        parent.append(&line);
        ui.object_search_branches
            .borrow_mut()
            .push(object_search::Branch {
                name: schema.clone(),
                line,
                children: children.clone(),
                disclosure: disclosure.clone(),
                icon: folder.clone(),
                open_icon,
                closed_icon,
                expanded: user_expanded,
            });
        if let Some(objects) = objects_by_schema.remove(&schema) {
            for object in objects {
                let button = table_object_button(&object);
                button.add_css_class("table-nav-item");
                let table_key = format!("{}:{}.{}", session_key, object.schema, object.name);
                button.set_tooltip_text(Some(&table_key));
                if ui
                    .current_profile
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| profile_session_key(p) == session_key)
                    && ui
                        .active_table_by_session
                        .borrow()
                        .get(&workspace_ui::active(ui, &session_key))
                        .is_some_and(|active| active == &table_key)
                {
                    button.add_css_class("table-nav-item-selected");
                    *ui.selected_table_button.borrow_mut() = Some(button.clone());
                }
                let weak = Rc::downgrade(ui);
                let selected = object.clone();
                let profile_for_menu = profile.clone();
                let table_key_for_menu = table_key.clone();
                install_table_context_menu(
                    ui,
                    &button,
                    profile_for_menu,
                    selected.clone(),
                    table_key_for_menu,
                );
                let source_session = session_key.clone();
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        if !ui.sessions.borrow().contains_key(&source_session) {
                            ui.status.set_text("База отключена. Раскройте её заново.");
                            return;
                        }
                        if ui
                            .current_profile
                            .borrow()
                            .as_ref()
                            .is_none_or(|p| profile_session_key(p) != source_session)
                        {
                            select_session(&ui, &source_session);
                        }
                        select_table_button(&ui, &table_key);
                        open_table(&ui, selected.clone(), false);
                    }
                });
                children.append(&button);
                ui.table_list_items.borrow_mut().push((button, object));
            }
        }
        if children.first_child().is_none() {
            let empty = gtk::Label::new(Some(if truncated {
                "Объекты не загружены: лимит каталога"
            } else {
                "Нет таблиц и представлений"
            }));
            empty.set_xalign(0.0);
            empty.add_css_class("dim-label");
            children.append(&empty);
        }
        parent.append(&children);
    }
}

fn session_profile_id(ui: &Ui, session_key: &str) -> Option<String> {
    ui.sessions
        .borrow()
        .get(session_key)
        .map(|(profile, _)| profile.id.clone())
}

fn profile_session_keys(ui: &Ui, profile_id: &str) -> Vec<String> {
    ui.sessions
        .borrow()
        .iter()
        .filter(|(_, (profile, _))| profile.id == profile_id)
        .map(|(key, _)| key.clone())
        .collect()
}

/// The session of the profile as saved (its default database), not a database opened later.
fn main_session_key(ui: &Ui, profile_id: &str) -> Option<String> {
    let store = ui.store.borrow();
    let store = store.as_ref()?;
    let profile = store.profiles.iter().find(|p| p.id == profile_id)?;
    Some(profile_session_key(profile))
}

fn profile_has_broken_session(ui: &Ui, profile_id: &str) -> bool {
    let broken = ui.broken_sessions.borrow();
    profile_session_keys(ui, profile_id)
        .iter()
        .any(|key| broken.contains(key))
}

/// Marks the lost database session (and its tabs); the whole profile when its main pool fell.
fn mark_connection_lost(ui: &Rc<Ui>, session_key: &str) {
    let Some(profile_id) = session_profile_id(ui, session_key) else {
        return;
    };
    // Late errors of the replaced pool must not break the fresh one.
    if ui.reconnecting.borrow().contains(&profile_id)
        || ui
            .recovered_at
            .borrow()
            .get(&profile_id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    {
        return;
    }
    let main_lost = main_session_key(ui, &profile_id).as_deref() == Some(session_key);
    let keys = if main_lost {
        profile_session_keys(ui, &profile_id)
    } else {
        vec![session_key.to_owned()]
    };
    ui.broken_sessions.borrow_mut().extend(keys);
    if main_lost {
        ui.profile_status
            .borrow_mut()
            .insert(profile_id.clone(), ProfileStatus::Broken);
        refresh_profiles(ui);
    }
    sync_broken_tab_icons(ui);
    refresh_tree_if_visible(ui);
    show_connection_banner(ui, &profile_id);
}

fn refresh_tree_if_visible(ui: &Rc<Ui>) {
    if ui.sidebar_stack.visible_child_name().as_deref() == Some("tables") {
        catalog_tree::render(ui);
    }
}

fn show_connection_banner(ui: &Rc<Ui>, profile_id: &str) {
    let banner = &ui.connection_banner;
    let same = banner.is_revealed()
        && banner.button_label().as_deref() == Some("Переподключиться")
        && ui.banner_profile.borrow().as_deref() == Some(profile_id);
    *ui.banner_profile.borrow_mut() = Some(profile_id.to_owned());
    if ui.reconnecting.borrow().contains(profile_id) {
        return;
    }
    if !same {
        banner.set_title("Соединение потеряно");
        banner.set_button_label(Some("Переподключиться"));
    }
    banner.set_revealed(true);
}

/// Blocks an action on a broken session: shows the banner instead of using the dead pool.
fn block_if_broken(ui: &Rc<Ui>, session_key: &str, retry: Option<RetryAction>) -> bool {
    if !ui.broken_sessions.borrow().contains(session_key) {
        return false;
    }
    if let Some(profile_id) = session_profile_id(ui, session_key) {
        if let Some(action) = retry {
            queue_retry(ui, &profile_id, action);
        }
        show_connection_banner(ui, &profile_id);
    }
    true
}

fn queue_retry(ui: &Ui, profile_id: &str, action: RetryAction) {
    if !retry_allowed(&action) {
        return;
    }
    let mut pending = ui.pending_retry.borrow_mut();
    let actions = pending.entry(profile_id.to_owned()).or_default();
    actions.retain(|existing| existing.id() != action.id());
    actions.push(action);
}

/// Remembers a read that failed because the session is broken.
fn queue_retry_if_lost(ui: &Ui, session_key: &str, action: RetryAction) {
    if !ui.broken_sessions.borrow().contains(session_key) {
        return;
    }
    if let Some(profile_id) = session_profile_id(ui, session_key) {
        queue_retry(ui, &profile_id, action);
    }
}

/// Adds or removes the "connection lost" icon on every table and SQL tab label.
fn sync_broken_tab_icons(ui: &Ui) {
    let broken = ui.broken_sessions.borrow().clone();
    let mut tabs: Vec<(String, bool)> = ui
        .table_profiles
        .borrow()
        .iter()
        .map(|(key, profile)| (key.clone(), broken.contains(&profile_session_key(profile))))
        .collect();
    tabs.extend(
        ui.sql_tabs
            .borrow()
            .iter()
            .map(|(id, tab)| (format!("sql::{id}"), broken.contains(&tab.session_key))),
    );
    for (key, is_broken) in tabs {
        set_tab_broken_icon(ui, &key, is_broken);
    }
    tab_strip::refresh(ui);
}

fn set_tab_broken_icon(ui: &Ui, key: &str, broken: bool) {
    let Some((notebook, _, page)) = find_table_page(ui, key) else {
        return;
    };
    let Some(label) = notebook
        .tab_label(&page)
        .and_then(|label| label.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    let mut existing = None;
    let mut child = label.first_child();
    while let Some(widget) = child {
        if widget.widget_name().as_str() == BROKEN_ICON_NAME {
            existing = Some(widget);
            break;
        }
        child = widget.next_sibling();
    }
    match (broken, existing) {
        (true, None) => {
            let icon = gtk::Image::from_icon_name("network-offline-symbolic");
            icon.set_widget_name(BROKEN_ICON_NAME);
            icon.set_tooltip_text(Some("Соединение потеряно"));
            label.insert_child_after(&icon, label.first_child().as_ref());
        }
        (false, Some(widget)) => label.remove(&widget),
        _ => {}
    }
}

fn reconnect_profile(ui: &Rc<Ui>, profile_id: &str) {
    if ui.reconnecting.borrow().contains(profile_id) {
        return;
    }
    let owner = ui.clone();
    let profile_id = profile_id.to_owned();
    ensure_vault_unlocked(ui, move || start_reconnect(&owner, &profile_id));
}

/// Same password sources as a normal connect; never captured by a widget closure.
fn reconnect_password(ui: &Ui, profile: &ConnectionProfile) -> Result<String, String> {
    if profile.gopass_entry.is_some() {
        return Ok(String::new());
    }
    let saved = if profile.password_saved {
        ui.store
            .borrow()
            .as_ref()
            .map(|store| store.password(&profile.id))
            .unwrap_or_else(|| Ok(String::new()))?
    } else {
        String::new()
    };
    if saved.is_empty() {
        return Err("Пароль не сохранён. Отключите подключение и подключитесь заново.".to_owned());
    }
    Ok(saved)
}

fn reconnect_failed(ui: &Rc<Ui>, profile_id: &str, error: &str) {
    ui.reconnecting.borrow_mut().remove(profile_id);
    *ui.banner_profile.borrow_mut() = Some(profile_id.to_owned());
    ui.connection_banner
        .set_title(&format!("Не удалось переподключиться: {error}"));
    ui.connection_banner
        .set_button_label(Some("Переподключиться"));
    ui.connection_banner.set_revealed(true);
    ui.status.set_text(error);
}

fn start_reconnect(ui: &Rc<Ui>, profile_id: &str) {
    if !ui.reconnecting.borrow_mut().insert(profile_id.to_owned()) {
        return;
    }
    let targets: Vec<(String, ConnectionProfile)> = ui
        .sessions
        .borrow()
        .iter()
        .filter(|(_, (profile, _))| profile.id == profile_id)
        .map(|(key, (profile, _))| (key.clone(), profile.clone()))
        .collect();
    let Some(first) = targets.first().map(|(_, profile)| profile.clone()) else {
        ui.reconnecting.borrow_mut().remove(profile_id);
        ui.connection_banner.set_revealed(false);
        return;
    };
    let password = match reconnect_password(ui, &first) {
        Ok(password) => password,
        Err(error) => {
            reconnect_failed(ui, profile_id, &error);
            return;
        }
    };
    *ui.banner_profile.borrow_mut() = Some(profile_id.to_owned());
    ui.connection_banner.set_title("Переподключаюсь…");
    ui.connection_banner.set_button_label(None);
    ui.connection_banner.set_revealed(true);
    let sender = ui.sender.clone();
    let profile_id = profile_id.to_owned();
    let guard = ui.activity.start();
    ui.runtime.spawn(async move {
        let _guard = guard;
        let result = reconnect_pools(targets, password).await;
        let _ = sender.send(Event::Reconnected(profile_id, result));
    });
}

/// Builds every pool first so a partial failure leaves the old state untouched.
async fn reconnect_pools(
    targets: Vec<(String, ConnectionProfile)>,
    password: String,
) -> Result<Vec<(String, DbPool)>, String> {
    let Some((_, first)) = targets.first() else {
        return Err("Нет открытых подключений профиля".to_owned());
    };
    let secret = resolve_profile_password(first, password).await?;
    let mut pools = Vec::with_capacity(targets.len());
    for (key, profile) in targets {
        pools.push((key, DbPool::connect(&profile, &secret).await?));
    }
    Ok(pools)
}

fn reconnect_finished(
    ui: &Rc<Ui>,
    profile_id: &str,
    result: Result<Vec<(String, DbPool)>, String>,
) {
    let pools = match result {
        Ok(pools) => pools,
        Err(error) => {
            reconnect_failed(ui, profile_id, &error);
            return;
        }
    };
    ui.reconnecting.borrow_mut().remove(profile_id);
    let replaced: Vec<String> = {
        let mut sessions = ui.sessions.borrow_mut();
        pools
            .into_iter()
            .filter_map(|(key, pool)| {
                sessions.get_mut(&key)?.1 = pool;
                Some(key)
            })
            .collect()
    };
    if replaced.is_empty() {
        ui.connection_banner.set_revealed(false);
        return;
    }
    let current_key = ui
        .current_profile
        .borrow()
        .as_ref()
        .map(profile_session_key);
    if let Some(key) = current_key.filter(|key| replaced.contains(key)) {
        let pool = ui.sessions.borrow().get(&key).map(|(_, pool)| pool.clone());
        if pool.is_some() {
            *ui.pool.borrow_mut() = pool;
        }
    }
    let tab_ids: Vec<String> = ui
        .sql_tabs
        .borrow()
        .iter()
        .filter(|(_, tab)| replaced.contains(&tab.session_key))
        .map(|(id, _)| id.clone())
        .collect();
    for id in tab_ids {
        replace_tab_session(ui, &id);
    }
    ui.broken_sessions
        .borrow_mut()
        .retain(|key| !replaced.contains(key));
    let was_broken = matches!(
        ui.profile_status.borrow().get(profile_id),
        Some(ProfileStatus::Broken)
    );
    if was_broken {
        ui.profile_status
            .borrow_mut()
            .insert(profile_id.to_owned(), ProfileStatus::Connected);
        refresh_profiles(ui);
    }
    ui.recovered_at
        .borrow_mut()
        .insert(profile_id.to_owned(), std::time::Instant::now());
    sync_broken_tab_icons(ui);
    refresh_tree_if_visible(ui);
    ui.status.set_text("Соединение восстановлено");
    let has_retry = ui
        .pending_retry
        .borrow()
        .get(profile_id)
        .is_some_and(|actions| !actions.is_empty());
    if has_retry {
        ui.connection_banner.set_title("Соединение восстановлено");
        ui.connection_banner.set_button_label(Some("Повторить"));
        ui.connection_banner.set_revealed(true);
    } else {
        ui.connection_banner.set_revealed(false);
    }
}

/// Like `reset_sql_session` but without the dialog; buffer, result and table edits stay.
fn replace_tab_session(ui: &Rc<Ui>, id: &str) {
    let mut tabs = ui.sql_tabs.borrow_mut();
    let Some(tab) = tabs.get_mut(id) else { return };
    let Some((profile, pool)) = ui.sessions.borrow().get(&tab.session_key).cloned() else {
        return;
    };
    let had_transaction = tab.transaction_state.pending || tab.session.has_pending_transaction();
    let was_running = !tab.run_button.is_sensitive();
    let previous = std::mem::replace(
        &mut tab.session,
        sql_session::SqlSession::with_profile_and_schema(
            pool.clone(),
            &profile,
            tab.schema.clone(),
        ),
    );
    tab.editor.completion().hide();
    tab.completion_provider.set_catalog(Default::default());
    load_sql_completions(ui, id, pool);
    previous.request_cancel();
    ui.runtime.spawn(async move {
        previous.close().await;
    });
    if was_running {
        // A new request id makes the late result of the old query be ignored.
        if let Some(plan) = tab.running_plan.take() {
            plan.request_cancel();
        }
        ui.background_tasks.cancel_status(&tab.request_id, true);
        tab.request_id = Uuid::new_v4().to_string();
        stop_sql_timer(tab);
        tab.run_button.set_sensitive(true);
        tab.stop_button.set_sensitive(false);
        tab.tabulator.set_locked(false);
        if tab.executed_sql.is_some() {
            queue_retry(
                ui,
                &tab.profile_id,
                RetryAction::Sql {
                    tab_id: id.to_owned(),
                    dialect: tab.risk_dialect,
                    needed_approval: tab.sent_needing_approval,
                    in_transaction: had_transaction,
                },
            );
        }
    }
    update_transaction_controls(tab, sql_session::TransactionState::default());
    if was_running && matches!(profile.kind, DatabaseKind::PostgreSql) {
        tab.status.set_text(if had_transaction {
            "Транзакция потеряна. Результат запроса неизвестен."
        } else {
            "Соединение потеряно. Результат запроса неизвестен."
        });
    } else if had_transaction {
        tab.status.set_text("Транзакция потеряна");
    } else if was_running {
        tab.status.set_text("Запрос прерван: соединение потеряно");
    }
}

/// Retries interrupted reads through the normal run path, so production
/// confirmation and the read-only checks apply again.
fn run_pending_retries(ui: &Rc<Ui>, profile_id: &str) {
    let actions = ui
        .pending_retry
        .borrow_mut()
        .remove(profile_id)
        .unwrap_or_default();
    ui.connection_banner.set_revealed(false);
    for action in actions.into_iter().filter(retry_allowed) {
        match action {
            RetryAction::Sql { tab_id, .. } => {
                let sql = ui
                    .sql_tabs
                    .borrow()
                    .get(&tab_id)
                    .and_then(|tab| tab.executed_sql.clone());
                if let Some(sql) = sql {
                    run_sql_tab_with_text(ui, &tab_id, Some(sql));
                }
            }
            RetryAction::LoadTable(key) => reload_table_view(ui, &key),
            RetryAction::ExpandNode(profile) => {
                let mut root = profile.clone();
                root.database.clear();
                let root_key = profile_session_key(&root);
                let pool = ui
                    .sessions
                    .borrow()
                    .get(&root_key)
                    .map(|(_, pool)| pool.clone())
                    .or_else(|| {
                        ui.sessions
                            .borrow()
                            .values()
                            .find(|(candidate, _)| candidate.id == profile.id)
                            .map(|(_, pool)| pool.clone())
                    });
                if let Some(pool) = pool {
                    let database = profile.database.clone();
                    open_database_session(ui, profile, pool, &database);
                }
            }
        }
    }
}

/// Records tab activity and, after a long idle, checks the connection in the
/// background. Not counted in the activity bar and silent unless it is lost.
fn note_tab_switch(ui: &Rc<Ui>, group: &str, key: &str, session_key: &str) {
    let now = std::time::Instant::now();
    let previous = ui.active_table_by_session.borrow().get(group).cloned();
    if let Some(previous) = previous.filter(|previous| previous != key) {
        ui.last_active.borrow_mut().insert(previous, now);
    }
    let last = ui.last_active.borrow().get(key).copied();
    ui.last_active.borrow_mut().insert(key.to_owned(), now);
    if ui.broken_sessions.borrow().contains(session_key) {
        if let Some(profile_id) = session_profile_id(ui, session_key) {
            show_connection_banner(ui, &profile_id);
        }
        return;
    }
    if last.is_some_and(|last| idle_check_due(last, now)) {
        spawn_idle_check(ui, key, session_key);
    }
}

fn spawn_idle_check(ui: &Rc<Ui>, key: &str, session_key: &str) {
    let Some((_, pool)) = ui.sessions.borrow().get(session_key).cloned() else {
        return;
    };
    let session = key
        .strip_prefix("sql::")
        .and_then(|id| ui.sql_tabs.borrow().get(id).map(|tab| tab.session.clone()));
    if session.is_some() && matches!(pool, DbPool::PostgreSql(_)) {
        // The next SQL execution checks the detached connection under its session lock.
        return;
    }
    let sender = ui.sender.clone();
    let session_key = session_key.to_owned();
    // Deliberately no activity guard: the background check stays invisible.
    ui.runtime.spawn(async move {
        let result = match &session {
            Some(session) => session.ping().await,
            None => pool.check().await,
        };
        let _ = report_lost(&pool, &sender, &session_key, result).await;
    });
}

/// Sends `Event::ConnectionLost` when `result` shows the connection is gone, then
/// hands back the user-facing `String` error so existing events stay unchanged.
async fn report_lost<T>(
    pool: &DbPool,
    sender: &mpsc::Sender<Event>,
    session_key: &str,
    result: Result<T, db_error::DbError>,
) -> Result<T, String> {
    if let Err(error) = &result
        && pool.is_connection_lost(error).await
    {
        let _ = sender.send(Event::ConnectionLost(session_key.to_owned()));
    }
    result.map_err(String::from)
}

fn loading_row(text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(16, 16);
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    row.append(&spinner);
    row.append(&label);
    row
}

/// Hides every loading spinner under `widget` after a load failed.
fn stop_loading_spinners(widget: &impl IsA<gtk::Widget>) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(spinner) = current.downcast_ref::<adw::Spinner>() {
            spinner.set_visible(false);
        } else {
            stop_loading_spinners(&current);
        }
        child = current.next_sibling();
    }
}

fn set_table_spinner(ui: &Ui, table_key: &str, visible: bool) {
    if let Some(spinner) = ui.table_spinners.borrow().get(table_key) {
        spinner.set_visible(visible);
    }
}

fn handle_event(ui: &Rc<Ui>, event: Event) {
    match event {
        Event::ConnectionLost(key) => mark_connection_lost(ui, &key),
        Event::Reconnected(profile_id, result) => reconnect_finished(ui, &profile_id, result),
        Event::NamespaceChoices(key, revision, result) => {
            namespace_ui::loaded(ui, &key, revision, result)
        }
        Event::ServerIcon(key, icon) => {
            if ui.sessions.borrow().contains_key(&key) {
                ui.server_icons.borrow_mut().insert(key, icon);
                refresh_profiles(ui);
            }
        }
        Event::VaultAutoUnlock(result) => storage_ui::auto_unlock_finished(ui, result),
        Event::KeyringChanged(enabled, result) => storage_ui::keyring_finished(ui, enabled, result),
        Event::GopassImported(result) => storage_ui::import_finished(ui, result),

        Event::Tested(test_generation, connection_generation, result) => {
            if test_generation != ui.test_generation.get()
                || connection_generation != ui.connection_generation.get()
            {
                return;
            }
            *ui.last_test.borrow_mut() = Some(result.clone());
            match result {
                Ok(()) => set_feedback(ui, "✓ Подключение успешно", "status-success"),
                Err(error) => {
                    set_feedback(ui, &format!("Ошибка: {error}"), "status-error");
                    show_error_dialog(ui, "Ошибка проверки подключения", &error);
                }
            }
        }
        Event::Connected(request_id, profile, result) => {
            let session_key = profile_session_key(&profile);
            if ui.connect_requests.borrow().get(&session_key) != Some(&request_id) {
                return;
            }
            ui.connect_requests.borrow_mut().remove(&session_key);
            let navigated = ui.pending_database_opens.borrow_mut().remove(&session_key);
            if navigated
                && ui.desired_database_session.borrow().as_deref() != Some(session_key.as_str())
            {
                return; // A later selection owns the screen; discard this unused pool.
            }
            match result {
                Ok(pool) => {
                    if ui.sessions.borrow().contains_key(&session_key) {
                        close_session(ui, &session_key);
                        if ui.sessions.borrow().contains_key(&session_key) {
                            ui.status.set_text("Завершите работу с текущим подключением перед повторным подключением");
                            return;
                        }
                    }
                    let generation = ui.connection_generation.get().wrapping_add(1);
                    ui.connection_generation.set(generation);
                    let next_view_generation = ui.view_generation.get().wrapping_add(1);
                    ui.view_generation.set(next_view_generation);
                    ui.profile_status
                        .borrow_mut()
                        .insert(profile.id.clone(), ProfileStatus::Connected);
                    ui.status.set_text("Подключено; загружаю таблицы…");
                    ui.landing.set_visible(false);
                    ui.empty_session.set_visible(true);
                    ui.table_filter.set_visible(false);
                    ui.table_list_items.borrow_mut().clear();
                    ui.active_header.set_visible(false);
                    ui.table_tabs.set_visible(false);
                    set_tables_nav_sensitive(ui, true);
                    ui.objects_available.set(true);
                    clear_box(&ui.objects);
                    ui.object_search_branches.borrow_mut().clear();
                    ui.objects.append(&loading_row("Загружаю таблицы…"));
                    show_sidebar(ui, "tables");
                    *ui.current_profile.borrow_mut() = Some(profile.clone());
                    *ui.pool.borrow_mut() = Some(pool.clone());
                    tab_strip::refresh(ui);
                    ui.sessions
                        .borrow_mut()
                        .insert(session_key.clone(), (profile.clone(), pool.clone()));
                    let detected_pool = pool.clone();
                    let detected_key = session_key.clone();
                    let sender = ui.sender.clone();
                    ui.runtime.spawn(async move {
                        if let Ok(icon) = detected_pool.server_icon().await {
                            let _ = sender.send(Event::ServerIcon(detected_key, icon));
                        }
                    });
                    if !profile.database.trim().is_empty() {
                        ensure_session_table_tabs(ui, &session_key);
                    }
                    // Schemas/databases open their tab only on an explicit choice.
                    if navigated && !matches!(profile.kind, DatabaseKind::PostgreSql) {
                        add_session_tab(ui, &profile);
                    }
                    refresh_empty_session(ui);
                    refresh_profiles(ui);
                    ui.active.set_text(&profile.name);
                    let database = if profile.database.is_empty() {
                        "Все доступные базы"
                    } else {
                        profile.database.as_str()
                    };
                    ui.active_details.set_text(&format!(
                        "{}  ·  {}:{}  ·  {}  ·  {}{}",
                        profile.kind.label(),
                        profile.host,
                        profile.port,
                        database,
                        profile.environment.label(),
                        if profile.read_only {
                            "  ·  ТОЛЬКО ЧТЕНИЕ"
                        } else {
                            ""
                        }
                    ));
                    ui.active_details.set_text(&format!(
                        "{}  ·  {}",
                        ui.active_details.text(),
                        transport_summary(&profile)
                    ));
                    set_profile_accent(&ui.active_header, &profile.color);
                    ui.connection_block.set(Some(&profile));
                    sql_history_ui::restore_drafts(ui, &profile);
                    show_table_catalog(ui);
                }
                Err(error) => {
                    let was_broken = matches!(
                        ui.profile_status.borrow().get(&profile.id),
                        Some(ProfileStatus::Broken)
                    );
                    ui.profile_status.borrow_mut().insert(
                        profile.id.clone(),
                        if is_profile_connected(ui, &profile.id) {
                            if was_broken {
                                ProfileStatus::Broken
                            } else {
                                ProfileStatus::Connected
                            }
                        } else {
                            ProfileStatus::Error(error.clone())
                        },
                    );
                    refresh_profiles(ui);
                    if ui.sessions.borrow().is_empty() {
                        ui.active_header.set_visible(false);
                        ui.connection_block.set(None);
                        ui.table_tabs.set_visible(false);
                        ui.landing.set_visible(true);
                        ui.active.set_text("Нет активного подключения");
                    }
                    ui.objects.set_sensitive(true);
                    stop_loading_spinners(&ui.objects);
                    ui.status.set_text(&error);
                    namespace_ui::sync(ui);
                    show_error_dialog(ui, "Ошибка подключения", &error);
                }
            }
        }
        Event::DatabasesLoaded(generation, view_generation, result) => {
            if generation != ui.connection_generation.get()
                || view_generation != ui.view_generation.get()
            {
                return;
            }
            ui.objects.set_sensitive(true);
            match result {
                Ok(databases) => {
                    let Some(profile) = ui.current_profile.borrow().clone() else {
                        return;
                    };
                    catalog_tree::databases_loaded(ui, &profile, databases);
                }
                Err(error) => {
                    stop_loading_spinners(&ui.objects);
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Не удалось загрузить базы данных", &error);
                }
            }
        }
        Event::TablesLoaded(generation, view_generation, result) => {
            if generation != ui.connection_generation.get()
                || view_generation != ui.view_generation.get()
            {
                return;
            }
            match result {
                Ok(catalog) => {
                    let Some(profile) = ui.current_profile.borrow().clone() else {
                        return;
                    };
                    let session_key = profile_session_key(&profile);
                    let table_count = catalog.objects.len();
                    if matches!(profile.kind, DatabaseKind::PostgreSql) {
                        catalog_tree::loaded(ui, &profile, catalog);
                        activate_session_table(ui, &session_key);
                        ui.status
                            .set_text(&format!("Загружено объектов: {table_count}"));
                        return;
                    }
                    let schemas = catalog.schemas.clone();
                    ui.table_filter.set_visible(true);
                    clear_box(&ui.objects);
                    ui.object_search_branches.borrow_mut().clear();
                    ui.table_list_items.borrow_mut().clear();
                    ui.selected_table_button.borrow_mut().take();
                    ui.objects_available.set(!schemas.is_empty());
                    set_tables_nav_sensitive(ui, !schemas.is_empty());
                    if !schemas.is_empty() {
                        show_sidebar(ui, "tables");
                    }
                    if schemas.is_empty()
                        && ui.sidebar_stack.visible_child_name().as_deref() == Some("tables")
                    {
                        show_sidebar(ui, "connections");
                    }
                    ui.objects.set_sensitive(true);
                    let group_name =
                        if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
                            "БАЗЫ ДАННЫХ"
                        } else {
                            "СХЕМЫ"
                        };
                    ui.objects.append(&navigation_section(&format!(
                        "{group_name} · {}",
                        schemas.len()
                    )));
                    let truncated = catalog.truncated;
                    append_schema_tree(ui, &ui.objects, &profile, catalog);
                    apply_table_list_filter(ui);
                    activate_session_table(ui, &session_key);
                    ui.status.set_text(&if truncated {
                        format!("Загружено объектов: {table_count}; показан лимит 1000")
                    } else {
                        format!("Загружено объектов: {table_count}")
                    });
                }
                Err(error) => {
                    ui.objects.set_sensitive(true);
                    stop_loading_spinners(&ui.objects);
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Не удалось загрузить таблицы", &error);
                }
            }
        }
        Event::TableLoaded(tab_key, request_id, saved_view, mut result) => {
            if ui.table_requests.borrow().get(&tab_key) != Some(&request_id) {
                return;
            }
            set_table_spinner(ui, &tab_key, false);
            let load_time = ui
                .table_edit_states
                .borrow()
                .get(&tab_key)
                .and_then(|state| state.load_started.take());
            if result.is_ok()
                && let Some((layout, pool)) = saved_view
            {
                result = result.and_then(|data| {
                    saved_table_views::commit_loaded(ui, &tab_key, layout, &pool, &data.columns)?;
                    Ok(data)
                });
            }
            if result.is_err() {
                if let Some(grid) = ui.tabulator_grids.borrow().get(&tab_key) {
                    grid.set_locked(false);
                    grid.reset_page_request();
                }
            }
            let grid = ui.table_grids.borrow().get(&tab_key).cloned();
            let Some(grid) = grid else {
                return;
            };
            match result {
                Ok(mut data) => {
                    let limited = data.trim_preview(database::MAX_BROWSE_ROWS);
                    if limited {
                        data.truncated = false;
                    }
                    let data = Arc::new(data);
                    render_table(ui, &tab_key, &grid, &data);
                    if let Some(tabulator) = ui.tabulator_grids.borrow().get(&tab_key) {
                        tabulator.set_data(
                            &data.columns,
                            &grid_rows(&data),
                            can_edit_table_cells(ui, &tab_key, &data),
                            data.truncated && !data.primary_key.is_empty(),
                            true,
                            &data.primary_key,
                            &grid_blocked_cells(&data),
                        );
                    }
                    if let Some(grid) = ui.tabulator_grids.borrow().get(&tab_key) {
                        let sort = ui
                            .table_view_states
                            .borrow()
                            .get(&tab_key)
                            .and_then(|view| view.sort.clone());
                        let sort = sort.as_ref().and_then(|sort| {
                            data.columns
                                .iter()
                                .position(|name| name == &sort.column)
                                .map(|index| (index, sort.descending))
                        });
                        grid.set_sort(sort);
                    }
                    ui.table_data
                        .borrow_mut()
                        .insert(tab_key.clone(), data.clone());
                    if let Some(state) = ui.table_edit_states.borrow().get(&tab_key) {
                        if let Some(started) = load_time {
                            state.load_ms.set(Some(started.elapsed().as_millis()));
                        }
                        update_table_summary(state, &data);
                        let editable = can_edit_table_cells(ui, &tab_key, &data);
                        state.cell_edit_allowed.set(editable);
                        let status = if !ui
                            .table_objects
                            .borrow()
                            .get(&tab_key)
                            .is_some_and(|object| object.kind == database::DbObjectKind::Table)
                        {
                            "Представление: редактирование отключено"
                        } else if ui
                            .table_profiles
                            .borrow()
                            .get(&tab_key)
                            .is_some_and(|profile| profile.read_only)
                        {
                            "Профиль подключения работает в режиме только чтение"
                        } else if editable {
                            "Клик выделяет; двойной клик или Enter редактирует; Shift/перетаскивание — диапазон; Escape — снять выделение"
                        } else {
                            "Для изменения отдельных ячеек таблице нужен загруженный первичный ключ"
                        };
                        update_table_edit_controls(state, Some(status));
                    }
                    if let Some(panel) = ui.table_filter_panels.borrow().get(&tab_key).cloned() {
                        initialize_filter_panel(&panel, &data.columns);
                    }
                    let mut details = Vec::new();
                    if limited {
                        details.push("Достигнут лимит памяти предпросмотра 32 МиБ; используйте фильтр или полный CSV".to_owned());
                    }
                    let view_state = ui
                        .table_view_states
                        .borrow()
                        .get(&tab_key)
                        .cloned()
                        .unwrap_or_default();
                    if view_state
                        .raw_condition
                        .as_ref()
                        .is_some_and(|s| !s.is_empty())
                    {
                        details.push("SQL-фильтр".to_owned());
                    }
                    if !view_state.filters.is_empty() {
                        details.push(format!("фильтров: {}", view_state.filters.len()));
                    }
                    if data.truncated {
                        details.push(if data.primary_key.is_empty() {
                            "дальнейшая загрузка отключена: нет первичного ключа для стабильных страниц".to_owned()
                        } else {
                            "следующие строки подгружаются при прокрутке".to_owned()
                        });
                    }
                    if data.truncated_columns {
                        details.push("показаны первые 64 столбца".to_owned());
                    }
                    if let Some(edit) = ui.table_edit_states.borrow().get(&tab_key) {
                        edit.summary.set_tooltip_text(Some(&details.join("; ")));
                        edit.loaded_view.replace(Some(view_state));
                    }
                    ui.status.set_text("Готово");
                }
                Err(error) => {
                    let lost_session = ui
                        .table_profiles
                        .borrow()
                        .get(&tab_key)
                        .map(profile_session_key);
                    if let Some(session_key) = lost_session {
                        queue_retry_if_lost(
                            ui,
                            &session_key,
                            RetryAction::LoadTable(tab_key.clone()),
                        );
                    }
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Не удалось прочитать таблицу", &error);
                }
            }
        }
        Event::TablePageLoaded(table_key, request_id, offset, result) => {
            if ui.table_requests.borrow().get(&table_key) != Some(&request_id) {
                return;
            }
            ui.table_more_loading.borrow_mut().remove(&table_key);
            set_table_spinner(ui, &table_key, false);
            let mut next = match result {
                Ok(next) => next,
                Err(error) => {
                    if let Some(grid) = ui.tabulator_grids.borrow().get(&table_key) {
                        grid.reset_page_request();
                    }
                    ui.status
                        .set_text(&format!("Не удалось загрузить следующую страницу: {error}"));
                    return;
                }
            };
            let mut tables = ui.table_data.borrow_mut();
            let Some(current) = tables.get_mut(&table_key) else {
                return;
            };
            let current = Arc::make_mut(current);
            if current.rows.len() != offset {
                return;
            }
            if current.columns != next.columns
                || current.cell_detail_key_columns != next.cell_detail_key_columns
            {
                current.truncated = false;
                if let Some(tabulator) = ui.tabulator_grids.borrow().get(&table_key) {
                    tabulator.append_data(offset, &[], false, &[]);
                }
                if let Some(state) = ui.table_edit_states.borrow().get(&table_key) {
                    update_table_summary(state, current);
                }
                ui.status
                    .set_text("Структура таблицы изменилась. Обновите данные");
                return;
            }
            current.rows.append(&mut next.rows);
            let limited = current.trim_preview(database::MAX_BROWSE_ROWS)
                || (next.truncated && current.rows.len() >= database::MAX_BROWSE_ROWS);
            current.truncated = next.truncated && !limited;
            if let Some(state) = ui.table_edit_states.borrow().get(&table_key) {
                update_table_summary(state, current);
            }
            if let Some(tabulator) = ui.tabulator_grids.borrow().get(&table_key) {
                tabulator.append_data(
                    offset,
                    &grid_rows_from_rows(&current.rows[offset..]),
                    current.truncated,
                    &grid_blocked_cells_from(current, offset),
                );
            }
            ui.status.set_text(&if limited {
                "Достигнут лимит предпросмотра (10 000 строк / 32 МиБ). Уточните фильтр или экспортируйте всю таблицу.".into()
            } else {format!("Загружено строк: {}",current.rows.len())});
        }
        Event::TableEditsSaved(table_key, result) => {
            if result.is_err() {
                if let Some(grid) = ui.tabulator_grids.borrow().get(&table_key) {
                    grid.set_locked(false);
                }
            }
            let Some(state) = ui.table_edit_states.borrow().get(&table_key).cloned() else {
                return;
            };
            state.saving.set(false);
            state.grid_editing.set(false);
            match result {
                Ok(count) => {
                    state.pending.borrow_mut().clear();
                    state.bulk_column.borrow_mut().take();
                    update_table_edit_controls(
                        &state,
                        Some(&format!("Сохранено изменений: {count}")),
                    );
                    reload_table_view(ui, &table_key);
                }
                Err(error) => {
                    update_table_edit_controls(
                        &state,
                        Some(&format!("Ошибка проверки/сохранения: {error}")),
                    );
                    state.save_button.set_sensitive(true);
                    state.discard_button.set_sensitive(true);
                }
            }
        }
        Event::TableRowChanged(table_key, action, result) => {
            if let Some(state) = ui.table_edit_states.borrow().get(&table_key) {
                state.saving.set(false);
            }
            if let Some(grid) = ui.tabulator_grids.borrow().get(&table_key) {
                grid.set_locked(false);
            }
            match result {
                Ok(count) => {
                    ui.status
                        .set_text(&format!("{action}: затронуто строк: {count}"));
                    reload_table_view(ui, &table_key);
                }
                Err(error) => {
                    ui.status.set_text(&format!("{action}: {error}"));
                    show_error_dialog(ui, "Не удалось изменить строку", &error);
                }
            }
        }
        Event::SqlCompletionsLoaded(id, source, result) => {
            let tabs = ui.sql_tabs.borrow();
            let Some(tab) = tabs.get(&id) else {
                return;
            };
            if !ui
                .sessions
                .borrow()
                .get(&tab.session_key)
                .is_some_and(|(_, current)| current.same_pool(&source))
            {
                return;
            }
            match result {
                Ok(catalog) => {
                    tab.editor.completion().hide();
                    tab.completion_provider.set_catalog(catalog);
                }
                Err(error) => tab
                    .status
                    .set_text(&format!("Подсказки объектов недоступны: {error}")),
            }
        }
        Event::SqlTransactionFinished(id, result) => {
            let mut tabs = ui.sql_tabs.borrow_mut();
            let Some(tab) = tabs.get_mut(&id) else { return };
            tab.run_button.set_sensitive(true);
            tab.tabulator.set_locked(false);
            match result {
                Ok(state) => {
                    update_transaction_controls(tab, state);
                    tab.status.set_text(
                        "Режим транзакций обновлён. Обновите результат через Ctrl+Enter.",
                    );
                }
                Err(error) => {
                    update_transaction_controls(tab, tab.transaction_state);
                    tab.error_message.set_text(&error);
                    tab.error_message.set_visible(true);
                }
            }
            // A displayed result predates the transaction mode change; reload before editing.
            if let Some(data) = tab.result.as_ref() {
                tab.tabulator.set_data(
                    &data.columns,
                    &grid_rows(data),
                    false,
                    false,
                    false,
                    &tab.read_only_columns,
                    &grid_blocked_cells(data),
                );
            }
        }
        Event::SqlQueryFinished(id, request_id, result, state, elapsed_ms, reconnected) => {
            ui.background_tasks
                .finish(&request_id, Outcome::for_result(&result));
            let Some(mut tab) = ui.sql_tabs.borrow_mut().remove(&id) else {
                return;
            };
            if tab.request_id != request_id {
                ui.sql_tabs.borrow_mut().insert(id, tab);
                return;
            }
            let is_active = workspace_ui::is_active(ui, &tab.group_key)
                && ui
                    .active_table_by_session
                    .borrow()
                    .get(&tab.group_key)
                    .is_some_and(|key| key == &format!("sql::{id}"));
            tab.run_button.set_sensitive(true);
            tab.stop_button.set_sensitive(false);
            stop_sql_timer(&mut tab);
            update_transaction_controls(&mut tab, state);
            sql_history_ui::record(ui, &tab, &result, elapsed_ms);
            let explain = tab.running_plan.take().is_some();
            match result {
                Ok(result) => {
                    render_query_result(&tab.grid, &result.data);
                    let key = format!("sql::{id}");
                    tab.read_only_columns = result.read_only_columns.clone();
                    if let Some(object) = result.editable_object.clone() {
                        ui.table_objects.borrow_mut().insert(key.clone(), object);
                    }
                    ui.table_data
                        .borrow_mut()
                        .insert(key.clone(), result.data.clone());
                    let editable = !explain
                        && !state.manual
                        && !state.pending
                        && can_edit_table_cells(ui, &key, &result.data);
                    if let Some(state) = ui.table_edit_states.borrow().get(&key) {
                        state.grid_editing.set(false);
                        state.cell_edit_allowed.set(editable);
                        update_table_edit_controls(state, None);
                    }
                    let text_plan = explain && tab.risk_dialect == sql_risk::Dialect::PostgreSql;
                    if text_plan {
                        let lines = result
                            .data
                            .rows
                            .iter()
                            .filter_map(|row| match row.first() {
                                Some(database::CellValue::Text(line)) => Some(line.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        let buffer = tab.plan_view.buffer();
                        buffer.set_text(&lines.join("\n"));
                        buffer.place_cursor(&buffer.start_iter());
                    } else {
                        tab.tabulator.set_data(
                            &result.data.columns,
                            &grid_rows(&result.data),
                            editable,
                            false,
                            false,
                            &result.read_only_columns,
                            &grid_blocked_cells(&result.data),
                        );
                    }
                    tab.export_button
                        .set_sensitive(!result.data.columns.is_empty());
                    let has_rows = !result.data.rows.is_empty();
                    tab.copy_button.set_sensitive(has_rows);
                    tab.result_stack
                        .set_visible_child_name(if text_plan && has_rows {
                            "plan"
                        } else if has_rows {
                            "data"
                        } else {
                            "empty"
                        });
                    let summary = format!("{} · {elapsed_ms} мс", query_result_summary(&result));
                    let summary = if reconnected {
                        format!("Открыта новая SQL-сессия · {summary}")
                    } else {
                        summary
                    };
                    tab.status.set_text(&if explain {
                        format!("Оценочный план (EXPLAIN) · {summary}")
                    } else if !result.data.columns.is_empty() && !editable {
                        format!("{summary} · только чтение")
                    } else {
                        summary
                    });
                    tab.status.set_tooltip_text(if explain {
                        Some("План содержит оценки оптимизатора")
                    } else {
                        (!editable).then_some("Для редактирования нужен SELECT прямых столбцов одной таблицы, полный первичный ключ и профиль с правом записи. JOIN, вычисления и агрегаты доступны для чтения.")
                    });
                    tab.error_message.set_text("");
                    tab.error_message.set_visible(false);
                    let line = if reconnected {
                        "Открыта новая SQL-сессия"
                    } else {
                        "Готово"
                    }
                    .to_owned();
                    tab.result = Some(result.data);
                    if is_active {
                        ui.status.set_text(&line);
                    }
                    tab.last_status = Some(line);
                }
                Err(error) => {
                    if !explain {
                        queue_retry_if_lost(
                            ui,
                            &tab.session_key,
                            RetryAction::Sql {
                                tab_id: id.clone(),
                                dialect: tab.risk_dialect,
                                needed_approval: tab.sent_needing_approval,
                                in_transaction: state.pending,
                            },
                        );
                    }
                    tab.tabulator.set_locked(false);
                    tab.status.set_text("Запрос завершился с ошибкой");
                    let error = if reconnected {
                        format!("Открыта новая SQL-сессия. Ошибка запроса: {error}")
                    } else {
                        format!("Ошибка запроса: {error}")
                    };
                    tab.error_message.set_text(&error);
                    tab.error_message.set_visible(true);
                    tab.last_status = None;
                    if is_active {
                        ui.status.set_text("Ошибка выполнения SQL-запроса");
                    }
                }
            }
            ui.sql_tabs.borrow_mut().insert(id, tab);
        }
        Event::CsvImportProgress(rows) => transfer_progress(
            ui,
            &format!("CSV: обработано {rows} строк; транзакция ещё не сохранена…"),
        ),
        Event::CsvImported(profile, object, result) => {
            finish_transfer(ui, Outcome::for_result(&result));
            match result {
                Ok(rows) => {
                    let key = format!(
                        "{}:{}.{}",
                        profile_session_key(&profile),
                        object.schema,
                        object.name
                    );
                    if ui.table_data.borrow().contains_key(&key) {
                        reload_table_view(ui, &key);
                    }
                    ui.status.set_text(&format!(
                        "CSV: добавлено {rows} строк в {}.{}.{}",
                        profile.database, object.schema, object.name
                    ));
                }
                Err(error) => show_error_dialog(ui, "Импорт CSV не завершён", &error),
            }
        }
        Event::RestoreFinished(profile, result) => {
            finish_transfer(ui, Outcome::for_result(&result));
            if ui
                .current_profile
                .borrow()
                .as_ref()
                .is_some_and(|p| profile_session_key(p) == profile_session_key(&profile))
            {
                show_table_catalog(ui);
            }
            match result {
                Ok(warnings) => {
                    let keys = ui
                        .table_profiles
                        .borrow()
                        .iter()
                        .filter(|(_, target)| {
                            profile_session_key(target) == profile_session_key(&profile)
                        })
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in keys {
                        reload_table_view(ui, &key);
                    }
                    ui.status
                        .set_text(&format!("Импорт завершён: {}", profile.database));
                    if !warnings.trim().is_empty() {
                        show_message_dialog(
                            ui,
                            gtk::MessageType::Warning,
                            "Импорт завершён с сообщениями",
                            &warnings,
                        );
                    }
                }
                Err(error) => show_error_dialog(
                    ui,
                    "Импорт не завершён",
                    &format!(
                        "{error}\nЧасть команд могла быть выполнена. Проверьте состояние БД перед повторным запуском."
                    ),
                ),
            }
        }
        Event::DumpProgress(bytes) => {
            transfer_progress(ui, &format!("Бэкап: {:.1} МиБ…", bytes as f64 / 1048576.0));
        }
        Event::DumpFinished(path, kind, result) => {
            finish_transfer(ui, Outcome::for_result(&result));
            match result {
                Ok(warnings) => {
                    ui.status
                        .set_text(&format!("Сохранено: {}", path.display()));
                    if !warnings.trim().is_empty() {
                        show_message_dialog(
                            ui,
                            gtk::MessageType::Warning,
                            "Бэкап создан с предупреждениями",
                            &warnings,
                        );
                    } else if matches!(kind, DatabaseKind::MySql | DatabaseKind::MariaDb) {
                        show_message_dialog(
                            ui,
                            gtk::MessageType::Info,
                            "Бэкап сохранён",
                            "Использован single-transaction: согласованный снимок обеспечивается для InnoDB. Во время выгрузки не следует менять структуру таблиц. Восстановление автоматически не выполняется.",
                        );
                    }
                }
                Err(error) => {
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Бэкап не сохранён", &error);
                }
            }
        }
        Event::ExportProgress(count) => {
            transfer_progress(ui, &format!("Экспорт CSV: {count} строк…"));
        }
        Event::SqlBatchProgress(index, total) => {
            transfer_progress(ui, &format!("Markdown: запрос {index} из {total}…"));
        }
        Event::SqlBatchFinished(path, result) => {
            let outcome = match &result {
                Ok(summary) if summary.stopped => Outcome::PartialReport,
                Ok(summary) if summary.failed > 0 => Outcome::WithErrors,
                _ => Outcome::for_result(&result),
            };
            finish_transfer(ui, outcome);
            match result {
                Ok(summary) => {
                    let text = format!(
                        "{}: {}\nУспешно: {}. Ошибок: {}. Не выполнено: {}.",
                        if summary.stopped {
                            "Частичный отчёт сохранён"
                        } else {
                            "Отчёт сохранён"
                        },
                        path.display(),
                        summary.succeeded,
                        summary.failed,
                        summary.skipped
                    );
                    ui.status.set_text(&text);
                    let dialog = gtk::MessageDialog::builder()
                        .transient_for(&ui.window)
                        .buttons(gtk::ButtonsType::Close)
                        .text("Markdown-отчёт")
                        .secondary_text(&text)
                        .build();
                    dialogs::install(&dialog);
                    dialog.connect_response(|dialog, _| dialog.close());
                    dialog.present();
                }
                Err(error) => {
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Отчёт не сохранён", &error);
                }
            }
        }
        Event::CsvExported(rows, result) => {
            finish_transfer(ui, Outcome::for_result(&result));
            match result {
                Ok(()) => ui.status.set_text(&format!("CSV сохранён: {rows} строк")),
                Err(error) => {
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Не удалось экспортировать CSV", &error);
                }
            }
        }
    }
}

fn query_result_summary(result: &database::QueryResult) -> String {
    if result.data.columns.is_empty() {
        return format!("Выполнено · изменено строк: {}", result.rows_affected);
    }
    let mut details = vec![format!("{} строк", result.row_count)];
    if result.data.truncated {
        details.push(format!(
            "показаны первые {} (лимит предпросмотра)",
            result.data.rows.len()
        ));
    }
    if result.data.truncated_columns {
        details.push(format!("показаны первые {} столбцов", 128));
    }
    format!("Выполнено · {}", details.join(" · "))
}

fn grid_rows(data: &TableData) -> Vec<Vec<Option<String>>> {
    grid_rows_from_rows(&data.rows)
}

fn grid_rows_from_rows(rows: &[Vec<CellValue>]) -> Vec<Vec<Option<String>>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|value| match value {
                    CellValue::Text(text)
                    | CellValue::Truncated(text)
                    | CellValue::DecodeError(text) => Some(text.clone()),
                    CellValue::Null => None,
                })
                .collect()
        })
        .collect()
}

fn grid_blocked_cells(data: &TableData) -> Vec<(usize, usize)> {
    grid_blocked_cells_from(data, 0)
}

fn grid_blocked_cells_from(data: &TableData, offset: usize) -> Vec<(usize, usize)> {
    data.rows[offset..]
        .iter()
        .enumerate()
        .flat_map(|(row, cells)| {
            cells.iter().enumerate().filter_map(move |(column, value)| {
                matches!(value, CellValue::Truncated(_) | CellValue::DecodeError(_))
                    .then_some((offset + row, column))
            })
        })
        .collect()
}

fn show_grid_cell(ui: &Rc<Ui>, table_key: &str, row: usize, column: usize) {
    let tables = ui.table_data.borrow();
    let Some(data) = tables.get(table_key) else {
        return;
    };
    let (Some(name), Some(cells)) = (data.columns.get(column), data.rows.get(row)) else {
        return;
    };
    let Some(value) = cells.get(column) else {
        return;
    };
    let pending = ui
        .table_edit_states
        .borrow()
        .get(table_key)
        .and_then(|state| state.pending.borrow().get(&(row, column)).cloned());
    let (text, note) = match (pending.as_ref(), value) {
        (Some(Some(text)), _) | (None, CellValue::Text(text)) => (text.as_str(), None),
        (Some(None), _) | (None, CellValue::Null) => ("NULL", Some("Значение SQL NULL")),
        (None, CellValue::Truncated(text)) => (
            text.as_str(),
            Some("Показана только загруженная часть значения"),
        ),
        (None, CellValue::DecodeError(text)) => {
            (text.as_str(), Some("Не удалось прочитать значение"))
        }
    };

    let window = adw::Window::builder()
        .title(format!("Значение ячейки · {name}"))
        .transient_for(&ui.window)
        .destroy_with_parent(true)
        .default_width(680)
        .default_height(360)
        .build();
    add_escape_to_window(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&adw::HeaderBar::new());
    let note = note.map(|note| {
        let label = gtk::Label::new(Some(note));
        label.set_wrap(true);
        label.set_margin_top(8);
        label.set_margin_bottom(8);
        content.append(&label);
        label
    });
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    let buffer = view.buffer();
    buffer.set_text(&text.replace('\0', "␀"));
    buffer.place_cursor(&buffer.start_iter());
    if let Some(note) = note
        && pending.is_none()
        && matches!(value, CellValue::Truncated(_))
        && !table_key.starts_with("sql::")
        && let Some(object) = ui.table_objects.borrow().get(table_key).cloned()
        && object.kind == database::DbObjectKind::Table
        && let Some(key) = data.cell_detail_key(cells)
    {
        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        navigation.set_halign(gtk::Align::Center);
        let previous = gtk::Button::with_label("Предыдущая");
        previous.set_visible(false);
        let next = gtk::Button::with_label("Загрузить из БД");
        navigation.append(&previous);
        navigation.append(&next);
        content.append(&navigation);
        let table_key = table_key.to_owned();
        let snapshot = Arc::downgrade(data);
        let owner = Rc::downgrade(ui);
        let column = name.clone();
        let page = Rc::new(Cell::new(None::<usize>));
        let request_page = Rc::new({
            let previous = previous.downgrade();
            let next = next.downgrade();
            let page = page.clone();
            let view = view.downgrade();
            move |requested_page| {
                let (Some(previous), Some(next), Some(view)) =
                    (previous.upgrade(), next.upgrade(), view.upgrade())
                else {
                    return;
                };
                let pool = owner.upgrade().and_then(|ui| {
                    let tables = ui.table_data.borrow();
                    let current = tables.get(&table_key)?;
                    if !std::ptr::eq(snapshot.as_ptr(), Arc::as_ptr(current)) {
                        return None;
                    }
                    let profiles = ui.table_profiles.borrow();
                    let session_key = profile_session_key(profiles.get(&table_key)?);
                    ui.sessions
                        .borrow()
                        .get(&session_key)
                        .map(|(_, pool)| (ui.runtime.clone(), pool.clone()))
                });
                let Some((runtime, pool)) = pool else {
                    note.set_text("Данные таблицы изменились или подключение закрыто");
                    previous.set_sensitive(false);
                    next.set_sensitive(false);
                    return;
                };
                let (object, column, key) = (object.clone(), column.clone(), key.clone());
                load_grid_cell(
                    &runtime,
                    async move {
                        pool.read_table_cell(&object, &column, &key, requested_page)
                            .await
                    },
                    &note,
                    &view,
                    (&previous, &next),
                    &page,
                    requested_page,
                );
            }
        });
        next.connect_clicked({
            let request_page = request_page.clone();
            let page = page.clone();
            move |button| {
                if button.is_sensitive() {
                    request_page(page.get().map_or(0, |page| page + 1));
                }
            }
        });
        previous.connect_clicked(move |button| {
            if button.is_sensitive()
                && let Some(page) = page.get().and_then(|page| page.checked_sub(1))
            {
                request_page(page);
            }
        });
    }
    content.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&view)
            .build(),
    );
    window.set_content(Some(&content));
    window.present();
    view.grab_focus();
}

fn load_grid_cell(
    runtime: &tokio::runtime::Runtime,
    read: impl std::future::Future<Output = Result<CellValue, db_error::DbError>> + Send + 'static,
    note: &gtk::Label,
    view: &gtk::TextView,
    (previous, next): (&gtk::Button, &gtk::Button),
    page: &Rc<Cell<Option<usize>>>,
    requested_page: usize,
) {
    let previous_enabled = previous.is_sensitive();
    let next_enabled = next.is_sensitive();
    previous.set_sensitive(false);
    next.set_sensitive(false);
    note.set_text("Загрузка значения из БД…");
    let worker = runtime
        .spawn(async move { tokio::time::timeout(std::time::Duration::from_secs(15), read).await });
    let (note, view, previous, next) = (
        note.downgrade(),
        view.downgrade(),
        previous.downgrade(),
        next.downgrade(),
    );
    let page = page.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        let result = worker.await;
        let (Some(note), Some(view), Some(previous), Some(next)) = (
            note.upgrade(),
            view.upgrade(),
            previous.upgrade(),
            next.upgrade(),
        ) else {
            return;
        };
        let buffer = view.buffer();
        let has_next = matches!(&result, Ok(Ok(Ok(CellValue::Truncated(_)))));
        let status = match result {
            Ok(Ok(Ok(CellValue::Text(text) | CellValue::Truncated(text)))) => {
                page.set(Some(requested_page));
                buffer.set_text(&text.replace('\0', "␀"));
                buffer.place_cursor(&buffer.start_iter());
                view.scroll_to_mark(&buffer.get_insert(), 0.0, true, 0.0, 0.0);
                let paged = requested_page > 0 || has_next;
                previous.set_visible(paged);
                next.set_visible(paged);
                previous.set_sensitive(requested_page > 0);
                next.set_sensitive(has_next);
                next.set_label("Следующая");
                if paged {
                    format!("Текущее значение из БД · страница {}", requested_page + 1)
                } else {
                    "Текущее значение из БД".to_owned()
                }
            }
            Ok(Ok(Ok(CellValue::Null))) => {
                page.set(Some(0));
                buffer.set_text("NULL");
                previous.set_visible(false);
                next.set_visible(false);
                "Значение SQL NULL".to_owned()
            }
            error => {
                previous.set_sensitive(previous_enabled);
                next.set_sensitive(next_enabled);
                match error {
                    Ok(Ok(Err(error))) => format!("Не удалось загрузить значение: {error}"),
                    Ok(Err(_)) => "Истекло время ожидания загрузки значения".to_owned(),
                    _ => "Не удалось загрузить значение".to_owned(),
                }
            }
        };
        note.set_text(&status);
    });
}

fn handle_tabulator_message(ui: &Rc<Ui>, table_key: &str, message: &str) {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(message) else {
        return;
    };
    match message.get("type").and_then(serde_json::Value::as_str) {
        Some("viewRelations") => {
            let generation = message["generation"]
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok());
            let row = message["row"]
                .as_u64()
                .and_then(|row| usize::try_from(row).ok());
            if let (Some(generation), Some(row)) = (generation, row) {
                table_relations_ui::show(ui, table_key, generation, row);
            }
        }
        Some("viewCell") => {
            let Some(generation) = message["generation"]
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                return;
            };
            if !ui
                .tabulator_grids
                .borrow()
                .get(table_key)
                .is_some_and(|grid| {
                    grid.generation() == Some(generation) && grid.webview.is_sensitive()
                })
            {
                return;
            }
            if let (Some(row), Some(column)) = (
                message["row"]
                    .as_u64()
                    .and_then(|row| usize::try_from(row).ok()),
                message["column"]
                    .as_str()
                    .and_then(|field| field.strip_prefix('c'))
                    .and_then(|column| column.parse::<usize>().ok()),
            ) {
                show_grid_cell(ui, table_key, row, column);
            }
        }
        Some("edit") => {
            // Ignore an in-flight clipboard/editor message after refresh or save locked the view.
            if ui
                .tabulator_grids
                .borrow()
                .get(table_key)
                .is_some_and(|grid| !grid.webview.is_sensitive())
            {
                return;
            }
            let tables = ui.table_data.borrow();
            let (Some(row), Some(field), Some(data), Some(state)) = (
                message
                    .get("row")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize),
                message.get("column").and_then(serde_json::Value::as_str),
                tables.get(table_key),
                ui.table_edit_states.borrow().get(table_key).cloned(),
            ) else {
                return;
            };
            if state.saving.get() || !can_edit_table_cells(ui, table_key, data) {
                return;
            }
            let Some(column) = field
                .strip_prefix('c')
                .and_then(|v| v.parse::<usize>().ok())
            else {
                return;
            };
            let Some(column_name) = data.columns.get(column) else {
                return;
            };
            if data.primary_key.contains(column_name)
                || table_key.strip_prefix("sql::").is_some_and(|id| {
                    ui.sql_tabs
                        .borrow()
                        .get(id)
                        .is_some_and(|tab| tab.read_only_columns.contains(column_name))
                })
                || !matches!(
                    data.rows.get(row).and_then(|cells| cells.get(column)),
                    Some(CellValue::Text(_) | CellValue::Null)
                )
            {
                return;
            }
            let value = match message.get("value") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(value)) => Some(value.clone()),
                Some(value) => Some(value.to_string()),
            };
            let original = data.rows.get(row).and_then(|cells| cells.get(column));
            let unchanged = match (original, value.as_deref()) {
                (Some(CellValue::Null), None) => true,
                (Some(CellValue::Text(before)), Some(after)) => before == after,
                _ => false,
            };
            if unchanged {
                state.pending.borrow_mut().remove(&(row, column));
            } else {
                state.pending.borrow_mut().insert((row, column), value);
            }
            update_table_edit_controls(&state, None);
        }
        Some("dataReady") => {
            if let Some(grid) = ui.tabulator_grids.borrow().get(table_key) {
                grid.set_locked(false);
            }
        }
        Some("editing") => {
            if let Some(state) = ui.table_edit_states.borrow().get(table_key) {
                state.grid_editing.set(
                    message["active"].as_bool().unwrap_or(false)
                        && message["dirty"].as_bool().unwrap_or(false),
                );
                update_table_edit_controls(state, None);
            }
        }
        Some("editAction") => match message.get("action").and_then(serde_json::Value::as_str) {
            Some("save") => save_table_edits(ui, table_key),
            Some("discard") => discard_table_edits(ui, table_key),
            _ => {}
        },
        Some("selection") => {
            let (Some(top), Some(bottom), Some(left), Some(right), Some(state)) = (
                message
                    .get("top")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize),
                message
                    .get("bottom")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize),
                message.get("left").and_then(serde_json::Value::as_str),
                message.get("right").and_then(serde_json::Value::as_str),
                ui.table_edit_states.borrow().get(table_key).cloned(),
            ) else {
                return;
            };
            let column_index = |field: &str| field.strip_prefix('c')?.parse::<usize>().ok();
            let (Some(left), Some(right)) = (column_index(left), column_index(right)) else {
                return;
            };
            if message
                .get("fullColumn")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                select_table_column(ui, table_key, left);
            } else {
                state.selection_anchor.set(Some((top, left)));
                state.selection_extent.set(Some((bottom, right)));
                state.selected_cell.set(Some((top, left)));
                state.selected_column.set(None);
                state.bulk_allowed.set(false);
            }
            update_table_edit_controls(&state, None);
        }
        Some("clearSelection") => clear_cell_selection(ui, table_key),
        Some("setNull") => stage_selection_null(ui, table_key),
        Some("rowAction") => {
            let (Some(row), Some(action)) = (
                message
                    .get("row")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize),
                message.get("action").and_then(serde_json::Value::as_str),
            ) else {
                return;
            };
            if table_key.starts_with("sql::") && action != "delete" {
                return;
            }
            let action = match action {
                "add" => "Добавить строку",
                "clone" => "Клонировать строку",
                "delete" => "Удалить строку",
                _ => return,
            };
            select_table_row(ui, table_key, row);
            run_table_row_action(ui, table_key, row, action);
        }
        Some("loadMore") => {
            let Some(generation) = message["generation"]
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                return;
            };
            let Some(grid) = ui.tabulator_grids.borrow().get(table_key).cloned() else {
                return;
            };
            if grid.generation() != Some(generation) {
                return;
            }
            if !grid.webview.is_sensitive() {
                grid.reset_page_request();
                return;
            }
            let view_state = ui
                .table_edit_states
                .borrow()
                .get(table_key)
                .and_then(|edit| {
                    if edit.load_started.get().is_some() {
                        return None;
                    }
                    edit.loaded_view.borrow().clone()
                });
            let (Some(profile), Some(object), Some(view_state), Some(current)) = (
                ui.table_profiles.borrow().get(table_key).cloned(),
                ui.table_objects.borrow().get(table_key).cloned(),
                view_state,
                ui.table_data.borrow().get(table_key).cloned(),
            ) else {
                return;
            };
            let offset = current.rows.len();
            if !current.truncated
                || current.primary_key.is_empty()
                || offset >= database::MAX_BROWSE_ROWS
            {
                return;
            }
            let Some((_, pool)) = ui
                .sessions
                .borrow()
                .get(&profile_session_key(&profile))
                .cloned()
            else {
                grid.reset_page_request();
                return;
            };
            let key = table_key.to_owned();
            let Some(request_id) = ui.table_requests.borrow().get(table_key).cloned() else {
                return;
            };
            if !ui.table_more_loading.borrow_mut().insert(key.clone()) {
                return;
            }
            let sender = ui.sender.clone();
            let session_key = profile_session_key(&profile);
            set_table_spinner(ui, table_key, true);
            let guard = ui.activity.start();
            ui.runtime.spawn(async move {
                let _guard = guard;
                let result = pool
                    .read_next_table_page(&object, &view_state, &current)
                    .await;
                drop(current);
                let result = report_lost(&pool, &sender, &session_key, result).await;
                let _ = sender.send(Event::TablePageLoaded(key, request_id, offset, result));
            });
        }
        Some("sort") => {
            let (Some(field), Some(direction), Some(data)) = (
                message.get("column").and_then(serde_json::Value::as_str),
                message.get("direction").and_then(serde_json::Value::as_str),
                ui.table_data.borrow().get(table_key).cloned(),
            ) else {
                return;
            };
            let Some(column_index) = field
                .strip_prefix('c')
                .and_then(|value| value.parse::<usize>().ok())
            else {
                return;
            };
            let Some(column) = data.columns.get(column_index).cloned() else {
                return;
            };
            if let Some(state) = ui.table_edit_states.borrow().get(table_key)
                && (!state.pending.borrow().is_empty() || state.bulk_column.borrow().is_some())
            {
                state
                    .status
                    .set_text("Сохраните или отмените изменения перед сортировкой");
                return;
            }
            let Some(mut view_state) = ui.table_view_states.borrow().get(table_key).cloned() else {
                return;
            };
            view_state.sort = Some(TableSort {
                column,
                descending: direction == "desc",
            });
            ui.table_view_states
                .borrow_mut()
                .insert(table_key.to_owned(), view_state);
            reload_table_view(ui, table_key);
        }
        Some("sortClear") => {
            if let Some(state) = ui.table_edit_states.borrow().get(table_key)
                && (!state.pending.borrow().is_empty() || state.bulk_column.borrow().is_some())
            {
                state
                    .status
                    .set_text("Сохраните или отмените изменения перед сортировкой");
                return;
            }
            let cleared =
                if let Some(view_state) = ui.table_view_states.borrow_mut().get_mut(table_key) {
                    view_state.sort = None;
                    true
                } else {
                    false
                };
            if cleared {
                reload_table_view(ui, table_key);
            }
        }
        _ => {}
    }
}

fn open_table(ui: &Rc<Ui>, object: DbObject, open_in_new_tab: bool) {
    let (Some(pool), Some(profile)) = (
        ui.pool.borrow().clone(),
        ui.current_profile.borrow().clone(),
    ) else {
        return;
    };
    if matches!(profile.kind, DatabaseKind::MySql | DatabaseKind::MariaDb)
        && profile.database != object.schema
        && !object.schema.trim().is_empty()
    {
        let mut target = profile;
        target.database = object.schema.clone();
        with_database_session(ui, target, pool, move |ui, _| {
            open_table(ui, object, open_in_new_tab)
        });
        return;
    }
    if profile.database.trim().is_empty() {
        ui.status
            .set_text("Выберите конкретную базу данных перед открытием таблицы");
        return;
    }
    tab_strip::sync_connection(ui);
    let name = format!("{}.{}", object.schema, object.name);
    let table_name = object.name.clone();
    let session_key = profile_session_key(&profile);
    if block_if_broken(ui, &session_key, None) {
        return;
    }
    let group_key = workspace_ui::ensure(ui, &session_key, Some(&object.schema));
    workspace_ui::select(ui, &group_key);
    let tab_key = format!("{}:{name}", session_key);
    let notebook = ensure_session_table_tabs(ui, &group_key);
    if let Some((_, page, _)) = find_table_page(ui, &tab_key) {
        ui.active_table_by_session
            .borrow_mut()
            .insert(group_key.clone(), tab_key);
        ui.table_tabs.set_visible_child_name(&group_key);
        notebook.set_current_page(Some(page));
        ui.table_tabs.set_visible(true);
        ui.empty_session.set_visible(false);
        return;
    }
    let mut replace_page = None;
    if !open_in_new_tab {
        let previous_key = ui.active_table_by_session.borrow().get(&group_key).cloned();
        if let Some(previous_key) = previous_key
            && let Some((_, page, widget)) = find_table_page(ui, &previous_key)
        {
            if let Some(state) = ui.table_edit_states.borrow().get(&previous_key)
                && (!state.pending.borrow().is_empty() || state.bulk_column.borrow().is_some())
            {
                state
                    .status
                    .set_text("Сохраните или отмените изменения перед открытием другой таблицы");
                return;
            }
            ui.tab_switching.set(true);
            notebook.remove_page(Some(page));
            ui.tab_switching.set(false);
            ui.table_grids.borrow_mut().remove(&previous_key);
            ui.tabulator_grids.borrow_mut().remove(&previous_key);
            ui.table_more_loading.borrow_mut().remove(&previous_key);
            ui.table_profiles.borrow_mut().remove(&previous_key);
            ui.table_objects.borrow_mut().remove(&previous_key);
            ui.table_view_states.borrow_mut().remove(&previous_key);
            ui.table_filter_panels.borrow_mut().remove(&previous_key);
            ui.table_requests.borrow_mut().remove(&previous_key);
            ui.table_data.borrow_mut().remove(&previous_key);
            ui.table_edit_states.borrow_mut().remove(&previous_key);
            ui.table_spinners.borrow_mut().remove(&previous_key);
            drop(widget);
            replace_page = Some(page);
        }
    }
    let grid = gtk::Grid::new();
    grid.set_column_spacing(0);
    grid.set_row_spacing(0);
    grid.set_hexpand(true);
    grid.set_vexpand(true);
    let tabulator_key = tab_key.clone();
    let weak = Rc::downgrade(ui);
    let tabulator = TabulatorGrid::with_relations(
        true,
        object.kind == database::DbObjectKind::Table,
        move |message| {
            if let Some(ui) = weak.upgrade() {
                handle_tabulator_message(&ui, &tabulator_key, &message);
            }
        },
    );
    tabulator.set_theme(&theme::grid_css(ui.active_theme.get()));
    grid.set_visible(false);
    let page_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let export_csv = gtk::MenuButton::builder()
        .label("Экспорт CSV")
        .tooltip_text("Выбрать экспорт показанных или всех строк")
        .build();
    let export_menu = gtk::Popover::new();
    let export_actions = gtk::Box::new(gtk::Orientation::Vertical, 2);
    export_actions.set_margin_top(4);
    export_actions.set_margin_bottom(4);
    export_actions.set_margin_start(4);
    export_actions.set_margin_end(4);
    let export_shown = gtk::Button::with_label("Показанные строки (до 200)…");
    export_shown.add_css_class("flat");
    let weak = Rc::downgrade(ui);
    let export_key = tab_key.clone();
    let export_name = table_name.clone();
    export_shown.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_table_csv_export(&ui, &export_key, &export_name);
        }
    });
    export_actions.append(&export_shown);
    let export_all = gtk::Button::with_label("Все строки…");
    export_all.add_css_class("flat");
    let weak = Rc::downgrade(ui);
    let export_profile = profile.clone();
    let export_object = object.clone();
    let export_name = table_name.clone();
    export_all.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            choose_full_table_csv_export(&ui, &export_profile, &export_object, &export_name);
        }
    });
    export_actions.append(&export_all);
    export_menu.set_child(Some(&export_actions));
    export_csv.set_popover(Some(&export_menu));
    export_csv.set_valign(gtk::Align::Center);
    let filter_panel = create_table_filter_panel(ui, &tab_key);
    let table_edit_state = create_table_edit_state();
    let edit_toolbar = table_edit_state.toolbar.clone();
    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&tabulator.webview)
        .build();
    let database_label = if profile.database.trim().is_empty() {
        profile.host.as_str()
    } else {
        profile.database.as_str()
    };
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    footer.add_css_class("table-footer");

    footer.append(&table_edit_state.summary);
    table_edit_state
        .add_row_button
        .set_visible(!profile.read_only);
    footer.append(&table_edit_state.add_row_button);
    let weak = Rc::downgrade(ui);
    let add_row_key = tab_key.clone();
    table_edit_state.add_row_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            run_table_row_action(&ui, &add_row_key, usize::MAX, "Добавить строку");
        }
    });
    let footer_space = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    footer_space.set_hexpand(true);
    footer_space.append(&edit_toolbar);
    footer.append(&footer_space);
    footer.append(&tabulator.column_chooser());
    footer.append(&export_csv);
    page_content.append(&filter_panel.0);
    page_content.append(&scroll);
    page_content.append(&footer);
    page_content.set_hexpand(true);
    page_content.set_vexpand(true);
    page_content.set_focusable(true);
    let outside_click = gtk::GestureClick::new();
    outside_click.set_button(1);
    outside_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let edit_key = tab_key.clone();
    let grid_for_focus = grid.clone();
    let weak = Rc::downgrade(ui);
    outside_click.connect_pressed(move |gesture, _, x, y| {
        let Some(root) = gesture.widget() else {
            return;
        };
        let picked = root.pick(x, y, gtk::PickFlags::DEFAULT);
        let clicked_in_grid = picked
            .as_ref()
            .is_some_and(|widget| widget != &grid_for_focus && widget.is_ancestor(&grid_for_focus));
        if clicked_in_grid {
            return;
        }
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let editing = ui.table_edit_states.borrow().get(&edit_key).cloned();
        if let Some(state) = editing {
            if state.editing_cell.get().is_some() {
                finish_inline_cell_edit(&ui, &edit_key);
            }
            if state.editing_column.get().is_some() {
                finish_inline_column_edit(&ui, &edit_key);
            }
        }
        root.grab_focus();
    });
    page_content.add_controller(outside_click);
    ui.table_grids.borrow_mut().insert(tab_key.clone(), grid);
    ui.tabulator_grids
        .borrow_mut()
        .insert(tab_key.clone(), tabulator.clone());
    ui.table_profiles
        .borrow_mut()
        .insert(tab_key.clone(), profile.clone());
    ui.table_objects
        .borrow_mut()
        .insert(tab_key.clone(), object.clone());
    ui.table_view_states
        .borrow_mut()
        .insert(tab_key.clone(), TableViewState::default());
    ui.table_filter_panels
        .borrow_mut()
        .insert(tab_key.clone(), filter_panel.1);
    ui.table_edit_states
        .borrow_mut()
        .insert(tab_key.clone(), table_edit_state.clone());
    let request_id = Uuid::new_v4().to_string();
    ui.table_requests
        .borrow_mut()
        .insert(tab_key.clone(), request_id.clone());
    let tab_label_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_label_box.add_css_class("compact-tab-label");
    tab_label_box.set_tooltip_text(Some(&tab_key));
    tab_label_box.append(&profile_tab_icon("entity-table", &profile.color));
    let tab_title = gtk::Label::new(Some(&object.name));
    tab_title.set_tooltip_text(Some(&format!(
        "{} · {} · {}",
        profile.name, database_label, name
    )));
    tab_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    tab_title.set_width_chars(12);
    tab_title.set_max_width_chars(28);
    let close_tab = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&close_tab, 14);
    close_tab.add_css_class("flat");
    close_tab.add_css_class("circular");
    close_tab.add_css_class("compact-tab-close");
    close_tab.set_tooltip_text(Some("Закрыть вкладку"));
    tab_label_box.append(&tab_title);
    let tab_spinner = adw::Spinner::new();
    tab_spinner.set_size_request(14, 14);
    ui.table_spinners
        .borrow_mut()
        .insert(tab_key.clone(), tab_spinner.clone());
    tab_label_box.append(&tab_spinner);
    tab_label_box.append(&close_tab);
    let tab_index = if let Some(position) = replace_page {
        notebook.insert_page(&page_content, Some(&tab_label_box), Some(position))
    } else {
        notebook.append_page(&page_content, Some(&tab_label_box))
    };
    ui.active_table_by_session
        .borrow_mut()
        .insert(group_key.clone(), tab_key.clone());
    ui.table_tabs.set_visible_child_name(&group_key);
    notebook.set_current_page(Some(tab_index));
    notebook.set_tab_reorderable(&page_content, true);
    let weak = Rc::downgrade(ui);
    let key = tab_key.clone();
    close_tab.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            close_table_tab(&ui, &key);
        }
    });
    let save_grid = tabulator.clone();
    table_edit_state
        .save_button
        .connect_clicked(move |_| save_grid.finish_edit(false));
    let discard_grid = tabulator.clone();
    table_edit_state
        .discard_button
        .connect_clicked(move |_| discard_grid.finish_edit(true));
    let weak = Rc::downgrade(ui);
    let key_for_bulk = tab_key.clone();
    table_edit_state.bulk_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            edit_selected_table_column(&ui, &key_for_bulk);
        }
    });
    ui.table_tabs.set_visible(true);
    ui.empty_session.set_visible(false);
    ui.status
        .set_text(&format!("Загружаю {}.{}…", object.schema, object.name));
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let request_id_for_event = request_id.clone();
    let guard = ui.activity.start();
    start_table_load_timer(ui, &tab_key);
    runtime.spawn(async move {
        let _guard = guard;
        let result = pool.read_table(&object).await;
        let result = report_lost(&pool, &sender, &session_key, result).await;
        let _ = sender.send(Event::TableLoaded(
            tab_key,
            request_id_for_event,
            None,
            result,
        ));
    });
}

fn create_table_filter_panel(ui: &Rc<Ui>, table_key: &str) -> (gtk::Box, Rc<TableFilterPanel>) {
    let panel = Rc::new(TableFilterPanel {
        toggle: gtk::ToggleButton::new(),
        summary: gtk::Label::new(None),
        rows: gtk::Box::new(gtk::Orientation::Vertical, 4),
        controls: RefCell::new(Vec::new()),
        columns: RefCell::new(Vec::new()),
        add: gtk::Button::from_icon_name("list-add-symbolic"),
        apply: gtk::Button::from_icon_name("system-search-symbolic"),
        clear: gtk::Button::from_icon_name("edit-clear-symbolic"),
        sql_mode: gtk::ToggleButton::with_label("SQL WHERE"),
        raw: gtk::Entry::new(),
        logic_width: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
        column_width: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
        operator_width: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
        row_height: gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical),
    });
    let container = gtk::Box::new(gtk::Orientation::Vertical, 4);
    container.add_css_class("table-filter-panel");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    panel.add.add_css_class("flat");
    panel.apply.add_css_class("suggested-action");
    panel.clear.add_css_class("flat");
    panel.add.set_sensitive(false);
    panel.apply.set_sensitive(false);
    panel.clear.set_sensitive(false);
    panel
        .sql_mode
        .set_tooltip_text(Some("Сырое SQL-условие WHERE"));
    panel.sql_mode.add_css_class("table-sql-mode");
    panel.sql_mode.set_valign(gtk::Align::Center);
    panel.add.set_valign(gtk::Align::Center);
    panel.apply.set_valign(gtk::Align::Center);
    panel.clear.set_valign(gtk::Align::Center);
    panel.add.set_tooltip_text(Some("Добавить условие"));
    panel
        .apply
        .set_tooltip_text(Some("Применить фильтр (Enter)"));
    panel.clear.set_tooltip_text(Some("Сбросить фильтр"));
    panel
        .raw
        .set_placeholder_text(Some("WHERE x != 4 AND (status = 'open' OR priority > 2)"));
    panel.raw.set_hexpand(true);
    panel.raw.set_visible(false);
    panel.raw.add_css_class("sql-condition-entry");
    let apply = panel.apply.clone();
    panel.raw.connect_activate(move |_| apply.emit_clicked());
    let filter_rows_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .min_content_height(0)
        .max_content_height(160)
        .valign(gtk::Align::Start)
        .propagate_natural_height(true)
        .hexpand(true)
        .child(&panel.rows)
        .build();
    toolbar.append(&filter_rows_scroll);
    toolbar.append(&panel.raw);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_valign(gtk::Align::Start);
    panel.row_height.add_widget(&actions);
    actions.append(&panel.add);
    actions.append(&panel.apply);

    toolbar.append(&actions);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    heading.add_css_class("table-filter-summary");
    panel.summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    panel.summary.set_max_width_chars(48);
    panel.summary.set_visible(false);
    let toggle_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let arrow = gtk::Image::from_icon_name("pan-end-symbolic");
    toggle_content.append(&arrow);
    toggle_content.append(&gtk::Label::new(Some("Фильтры")));
    toggle_content.append(&panel.summary);
    panel.toggle.set_child(Some(&toggle_content));
    panel.toggle.set_valign(gtk::Align::Center);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let views = saved_table_views::menu(ui, table_key);
    views.set_valign(gtk::Align::Center);
    panel.clear.set_label("Сбросить");
    heading.append(&panel.toggle);
    heading.append(&spacer);
    heading.append(&views);
    heading.append(&panel.clear);
    container.append(&heading);
    actions.append(&panel.sql_mode);
    toolbar.set_visible(false);
    panel
        .toggle
        .update_relation(&[gtk::accessible::Relation::Controls(&[toolbar.upcast_ref()])]);
    panel
        .toggle
        .update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    let toolbar_for_expand = toolbar.clone();
    panel.toggle.connect_toggled(move |toggle| {
        let expanded = toggle.is_active();
        toolbar_for_expand.set_visible(expanded);
        toggle.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
        arrow.set_icon_name(Some(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    });
    container.append(&toolbar);
    let weak_panel = Rc::downgrade(&panel);
    panel.sql_mode.connect_toggled(move |button| {
        if let Some(panel) = weak_panel.upgrade() {
            panel.toggle.set_active(true);
            let raw = button.is_active();
            filter_rows_scroll.set_visible(!raw);
            panel.raw.set_visible(raw);
            panel.add.set_visible(!raw);
            if raw {
                panel.raw.grab_focus();
            }
        }
    });

    let weak = Rc::downgrade(ui);
    let panel_for_add = panel.clone();
    panel.add.connect_clicked(move |_| {
        panel_for_add.toggle.set_active(true);
        let columns = panel_for_add.columns.borrow().clone();
        if !columns.is_empty() {
            append_filter_row(&panel_for_add, &columns, None);
        }
        if let Some(ui) = weak.upgrade() {
            ui.window.queue_draw();
        }
    });

    let weak = Rc::downgrade(ui);
    let key_for_apply = table_key.to_owned();
    let panel_for_apply = panel.clone();
    panel.apply.connect_clicked(move |_| {
        let columns = panel_for_apply.columns.borrow().clone();
        let result = if panel_for_apply.sql_mode.is_active() {
            database::validate_raw_condition(&panel_for_apply.raw.text())
                .map(|condition| (Vec::new(), Some(condition)))
        } else {
            read_filter_rows(&panel_for_apply, &columns).map(|filters| (filters, None))
        };
        match result {
            Ok((filters, raw_condition)) => {
                if let Some(ui) = weak.upgrade() {
                    if ui
                        .table_edit_states
                        .borrow()
                        .get(&key_for_apply)
                        .is_some_and(|state| {
                            state.saving.get()
                                || state.grid_editing.get()
                                || !state.pending.borrow().is_empty()
                                || state.bulk_column.borrow().is_some()
                        })
                    {
                        show_error_dialog(
                            &ui,
                            "Есть несохранённые изменения",
                            "Сохраните или отмените изменения перед сменой фильтра",
                        );
                        return;
                    }
                    let mut states = ui.table_view_states.borrow_mut();
                    let state = states.entry(key_for_apply.clone()).or_default();
                    state.filters = filters.clone();
                    state.raw_condition = raw_condition;
                    let summary = table_filter_summary(state);
                    panel_for_apply.set_summary(&summary);
                    panel_for_apply.toggle.set_active(false);
                    drop(states);
                    reload_table_view(&ui, &key_for_apply);
                }
            }
            Err(error) => {
                if let Some(ui) = weak.upgrade() {
                    show_error_dialog(&ui, "Не удалось применить фильтры", &error);
                }
            }
        }
    });

    let weak = Rc::downgrade(ui);
    let key_for_clear = table_key.to_owned();
    let panel_for_clear = panel.clone();
    panel.clear.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            if ui
                .table_edit_states
                .borrow()
                .get(&key_for_clear)
                .is_some_and(|state| {
                    state.saving.get()
                        || state.grid_editing.get()
                        || !state.pending.borrow().is_empty()
                        || state.bulk_column.borrow().is_some()
                })
            {
                show_error_dialog(
                    &ui,
                    "Есть несохранённые изменения",
                    "Сохраните или отмените изменения перед сменой фильтра",
                );
                return;
            }
            let mut states = ui.table_view_states.borrow_mut();
            let state = states.entry(key_for_clear.clone()).or_default();
            state.filters.clear();
            state.raw_condition = None;
            drop(states);
            panel_for_clear.set_summary("");
            panel_for_clear.raw.set_text("");
            let columns = panel_for_clear.columns.borrow().clone();
            clear_filter_rows(&panel_for_clear);
            if !columns.is_empty() {
                append_filter_row(&panel_for_clear, &columns, None);
            }
            reload_table_view(&ui, &key_for_clear);
        }
    });
    (container, panel)
}

fn table_filter_summary(state: &TableViewState) -> String {
    if let Some(raw) = &state.raw_condition {
        format!("WHERE {raw}")
    } else {
        state
            .filters
            .iter()
            .enumerate()
            .map(|(index, filter)| {
                let operators = [
                    FilterOperator::Equal,
                    FilterOperator::NotEqual,
                    FilterOperator::Contains,
                    FilterOperator::NotContains,
                    FilterOperator::StartsWith,
                    FilterOperator::EndsWith,
                    FilterOperator::Greater,
                    FilterOperator::GreaterEqual,
                    FilterOperator::Less,
                    FilterOperator::LessEqual,
                    FilterOperator::In,
                    FilterOperator::NotIn,
                    FilterOperator::IsNull,
                    FilterOperator::IsNotNull,
                ];
                let operator = operators
                    .iter()
                    .position(|op| *op == filter.operator)
                    .unwrap();
                let logic = if index == 0 {
                    ""
                } else if filter.logic == FilterLogic::Or {
                    " OR "
                } else {
                    " AND "
                };
                format!(
                    "{logic}{} {} {}",
                    filter.column,
                    filter_operator_labels()[operator],
                    filter.value
                )
            })
            .collect::<String>()
    }
}

fn filter_operator_labels() -> [&'static str; 14] {
    [
        "равно",
        "не равно",
        "содержит",
        "не содержит",
        "начинается с",
        "заканчивается на",
        ">",
        ">=",
        "<",
        "<=",
        "в списке",
        "не в списке",
        "IS NULL",
        "IS NOT NULL",
    ]
}

fn filter_operator(index: u32) -> FilterOperator {
    match index {
        0 => FilterOperator::Equal,
        1 => FilterOperator::NotEqual,
        2 => FilterOperator::Contains,
        3 => FilterOperator::NotContains,
        4 => FilterOperator::StartsWith,
        5 => FilterOperator::EndsWith,
        6 => FilterOperator::Greater,
        7 => FilterOperator::GreaterEqual,
        8 => FilterOperator::Less,
        9 => FilterOperator::LessEqual,
        10 => FilterOperator::In,
        11 => FilterOperator::NotIn,
        12 => FilterOperator::IsNull,
        _ => FilterOperator::IsNotNull,
    }
}

fn append_filter_row(
    panel: &Rc<TableFilterPanel>,
    columns: &[String],
    initial: Option<&TableFilter>,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("table-filter-row");
    panel.row_height.add_widget(&row);
    let column = gtk::ComboBoxText::new();
    column.set_size_request(120, -1);
    for name in columns {
        column.append_text(name);
    }
    column.set_active(Some(
        initial
            .and_then(|filter| columns.iter().position(|name| name == &filter.column))
            .unwrap_or(0) as u32,
    ));
    let logic = gtk::ComboBoxText::new();
    logic.set_tooltip_text(Some("Связь с предыдущим условием"));
    logic.append_text("И");
    logic.append_text("ИЛИ");
    logic.set_active(Some(
        initial.is_some_and(|filter| filter.logic == FilterLogic::Or) as u32,
    ));
    let operator = gtk::ComboBoxText::new();
    operator.set_size_request(136, -1);
    for label in filter_operator_labels() {
        operator.append_text(label);
    }
    let operator_index = initial.map_or(2, |filter| match filter.operator {
        FilterOperator::Equal => 0,
        FilterOperator::NotEqual => 1,
        FilterOperator::Contains => 2,
        FilterOperator::NotContains => 3,
        FilterOperator::StartsWith => 4,
        FilterOperator::EndsWith => 5,
        FilterOperator::Greater => 6,
        FilterOperator::GreaterEqual => 7,
        FilterOperator::Less => 8,
        FilterOperator::LessEqual => 9,
        FilterOperator::In => 10,
        FilterOperator::NotIn => 11,
        FilterOperator::IsNull => 12,
        FilterOperator::IsNotNull => 13,
    });
    operator.set_active(Some(operator_index));
    let value = gtk::Entry::new();
    value.set_hexpand(true);
    value.set_placeholder_text(Some("Значение"));
    if let Some(initial) = initial {
        value.set_text(&initial.value);
    }
    value.set_sensitive(operator_index < 12);
    let value_for_operator = value.clone();
    operator.connect_changed(move |operator| {
        value_for_operator.set_sensitive(operator.active().is_some_and(|index| index < 12));
    });
    let apply_button = panel.apply.clone();
    value.connect_activate(move |_| apply_button.emit_clicked());
    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&remove, 16);
    remove.add_css_class("flat");
    remove.set_size_request(24, -1);
    remove.set_tooltip_text(Some("Удалить условие"));
    let logic_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    logic_slot.set_size_request(56, -1);
    logic_slot.append(&logic);
    panel.logic_width.add_widget(&logic_slot);
    panel.column_width.add_widget(&column);
    panel.operator_width.add_widget(&operator);
    row.append(&column);
    row.append(&operator);
    row.append(&value);
    row.append(&logic_slot);
    row.append(&remove);
    let controls = FilterRowWidgets {
        row: row.clone(),
        column,
        operator,
        logic,
        value,
        remove: remove.clone(),
    };
    panel.rows.append(&row);
    panel.controls.borrow_mut().push(controls);
    update_filter_remove_buttons(panel);
    let weak_panel = Rc::downgrade(panel);
    let row_id = row.as_ptr() as usize;
    remove.connect_clicked(move |_| {
        if let Some(panel) = weak_panel.upgrade() {
            let removed = {
                let mut controls = panel.controls.borrow_mut();
                controls
                    .iter()
                    .position(|controls| controls.row.as_ptr() as usize == row_id)
                    .map(|index| controls.remove(index))
            };
            if let Some(controls) = removed {
                panel.rows.remove(&controls.row);
            }
            update_filter_remove_buttons(&panel);
            normalize_filter_logic_controls(&panel);
        }
    });
    normalize_filter_logic_controls(panel);
}

fn update_filter_remove_buttons(panel: &TableFilterPanel) {
    let controls = panel.controls.borrow();
    let allow_remove = controls.len() > 1;
    for row in controls.iter() {
        row.remove.set_sensitive(allow_remove);
    }
}

fn normalize_filter_logic_controls(panel: &TableFilterPanel) {
    for (index, controls) in panel.controls.borrow().iter().enumerate() {
        controls.logic.set_visible(index > 0);
        controls.logic.set_sensitive(index > 0);
        if index == 0 {
            controls.logic.set_active(Some(0));
        }
    }
}

fn clear_filter_rows(panel: &TableFilterPanel) {
    while let Some(row) = panel.rows.first_child() {
        panel.rows.remove(&row);
    }
    panel.controls.borrow_mut().clear();
}

fn initialize_filter_panel(panel: &Rc<TableFilterPanel>, columns: &[String]) {
    let mut available_columns = panel.columns.borrow_mut();
    if available_columns.is_empty() {
        *available_columns = columns.to_vec();
        if panel.controls.borrow().is_empty() && !columns.is_empty() {
            append_filter_row(panel, columns, None);
        }
    }
    drop(available_columns);
    panel.add.set_sensitive(!columns.is_empty());
    panel.apply.set_sensitive(!columns.is_empty());
    panel.clear.set_sensitive(!columns.is_empty());
}

fn read_filter_rows(
    panel: &TableFilterPanel,
    columns: &[String],
) -> Result<Vec<TableFilter>, String> {
    let mut filters = Vec::new();
    for controls in panel.controls.borrow().iter() {
        let Some(column_index) = controls.column.active() else {
            continue;
        };
        let Some(column) = columns.get(column_index as usize) else {
            continue;
        };
        let operator = filter_operator(controls.operator.active().unwrap_or(2));
        let value = controls.value.text().to_string();
        if matches!(
            operator,
            FilterOperator::Contains
                | FilterOperator::NotContains
                | FilterOperator::StartsWith
                | FilterOperator::EndsWith
        ) && value.is_empty()
        {
            continue;
        }
        if matches!(operator, FilterOperator::In | FilterOperator::NotIn)
            && value.split(',').all(|part| part.trim().is_empty())
        {
            return Err(format!(
                "Для столбца «{column}» перечислите значения через запятую"
            ));
        }
        filters.push(TableFilter {
            column: column.clone(),
            logic: if controls.logic.active() == Some(1) {
                FilterLogic::Or
            } else {
                FilterLogic::And
            },
            operator,
            value,
        });
    }
    Ok(filters)
}

fn create_table_edit_state() -> Rc<TableEditState> {
    let edit_toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    edit_toolbar.add_css_class("table-edit-toolbar");
    edit_toolbar.set_visible(false);
    let table_edit_state = Rc::new(TableEditState {
        pending: RefCell::new(HashMap::new()),
        bulk_column: RefCell::new(None),
        selected_column: Cell::new(None),
        selected_cell: Cell::new(None),
        selection_anchor: Cell::new(None),
        selection_extent: Cell::new(None),
        copied_bounds: Cell::new(None),
        editing_cell: Cell::new(None),
        editing_column: Cell::new(None),
        bulk_allowed: Cell::new(false),
        saving: Cell::new(false),
        grid_editing: Cell::new(false),
        cell_edit_allowed: Cell::new(false),
        toolbar: edit_toolbar.clone(),
        save_button: gtk::Button::with_label("Проверить правки…"),
        discard_button: gtk::Button::with_label("Откатить"),
        bulk_button: gtk::Button::with_label("Заменить столбец…"),
        status: gtk::Label::new(Some(
            "Клик — выделить; двойной клик — редактировать; Ctrl+C/V — буфер",
        )),
        summary: gtk::Label::new(None),
        add_row_button: gtk::Button::with_label("+ строка"),
        load_started: Cell::new(None),
        load_ms: Cell::new(None),
        loaded_view: RefCell::new(None),
    });
    table_edit_state.summary.add_css_class("table-rows-summary");
    table_edit_state.add_row_button.add_css_class("flat");
    table_edit_state
        .add_row_button
        .add_css_class("table-add-row-button");
    table_edit_state
        .add_row_button
        .set_tooltip_text(Some("Добавить строку со значениями по умолчанию"));
    table_edit_state.add_row_button.set_visible(false);
    table_edit_state
        .save_button
        .add_css_class("suggested-action");
    table_edit_state
        .save_button
        .add_css_class("table-save-button");
    table_edit_state.save_button.set_sensitive(false);
    table_edit_state.discard_button.set_sensitive(false);
    table_edit_state.discard_button.add_css_class("flat");
    table_edit_state.bulk_button.set_sensitive(false);
    table_edit_state.status.set_xalign(0.0);
    table_edit_state.status.set_hexpand(true);
    table_edit_state
        .status
        .set_ellipsize(gtk::pango::EllipsizeMode::End);
    table_edit_state.status.set_width_chars(1);
    table_edit_state.status.add_css_class("dim-label");
    edit_toolbar.append(&dialogs::copyable_label(&table_edit_state.status));
    edit_toolbar.append(&table_edit_state.discard_button);
    edit_toolbar.append(&table_edit_state.save_button);
    table_edit_state
}

fn update_table_summary(state: &TableEditState, data: &TableData) {
    let mut parts = vec![format!("{} строк", data.rows.len())];
    if data.truncated && !data.primary_key.is_empty() {
        parts.push("есть ещё".to_owned());
    }
    if let Some(ms) = state.load_ms.get() {
        parts.push(format!("{ms} мс"));
    }
    state.summary.set_text(&parts.join(" · "));
}

fn start_table_load_timer(ui: &Ui, table_key: &str) {
    if let Some(state) = ui.table_edit_states.borrow().get(table_key) {
        state.load_started.set(Some(std::time::Instant::now()));
    }
}

fn update_table_edit_controls(state: &TableEditState, message: Option<&str>) {
    let cell_count = state.pending.borrow().len();
    let bulk_pending = state.bulk_column.borrow().is_some();
    let dirty = cell_count > 0 || bulk_pending || state.grid_editing.get();
    state.toolbar.set_visible(dirty || state.saving.get());
    let can_save = dirty && !state.saving.get();
    state.save_button.set_sensitive(can_save);
    state.save_button.set_label(&if cell_count > 0 {
        format!("Проверить правки ({cell_count})…")
    } else {
        "Проверить правки…".to_owned()
    });
    if can_save {
        state.save_button.add_css_class("table-save-ready");
    } else {
        state.save_button.remove_css_class("table-save-ready");
    }
    state
        .discard_button
        .set_sensitive(dirty && !state.saving.get());
    state
        .bulk_button
        .set_sensitive(state.bulk_allowed.get() && !dirty && !state.saving.get());
    if let Some(message) = message {
        state.status.set_text(message);
    } else if bulk_pending {
        state
            .status
            .set_text("Несохранённая массовая замена: будут обновлены все строки таблицы");
    } else if dirty {
        state
            .status
            .set_text(&format!("Несохранённых ячеек: {cell_count}"));
    } else if state.bulk_allowed.get() && !state.cell_edit_allowed.get() {
        state
            .status
            .set_text("Столбец выделен; можно заменить значения во всех строках");
    } else if !state.cell_edit_allowed.get() {
        state
            .status
            .set_text("Редактирование отдельных ячеек недоступно");
    } else {
        state.status.set_text(
            "Клик выделяет; двойной клик или Enter редактирует; Shift/перетаскивание — диапазон; Escape — снять выделение",
        );
    }
}

fn select_table_column(ui: &Rc<Ui>, table_key: &str, column: usize) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    state.selected_column.set(Some(column));
    state.selected_cell.set(None);
    state.selection_anchor.set(None);
    state.selection_extent.set(None);
    state.copied_bounds.set(None);
    let is_primary_key = ui
        .table_data
        .borrow()
        .get(table_key)
        .and_then(|data| {
            data.columns
                .get(column)
                .map(|name| data.primary_key.contains(name))
        })
        .unwrap_or(true);
    let profile_read_only = ui
        .table_profiles
        .borrow()
        .get(table_key)
        .is_none_or(|profile| profile.read_only);
    let is_table = ui
        .table_objects
        .borrow()
        .get(table_key)
        .is_some_and(|object| object.kind == database::DbObjectKind::Table);
    state
        .bulk_allowed
        .set(!table_key.starts_with("sql::") && is_table && !profile_read_only && !is_primary_key);
    if let Some(grid) = ui.table_grids.borrow().get(table_key) {
        let mut child = grid.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            widget.remove_css_class("grid-column-selected");
            widget.remove_css_class("grid-cell-selected");
            widget.remove_css_class("grid-cell-range");
            widget.remove_css_class("grid-cell-copied");
            widget.remove_css_class("grid-row-header-selected");
        }
        let mut row = 0;
        while let Some(widget) = grid.child_at(column as i32 + 1, row) {
            widget.add_css_class("grid-column-selected");
            row += 1;
        }
        if let Some(cell) = grid.child_at(column as i32 + 1, 1) {
            cell.grab_focus();
        }
    }
    update_table_edit_controls(&state, None);
}

fn select_table_row(ui: &Rc<Ui>, table_key: &str, row: usize) {
    let column_count = ui
        .table_data
        .borrow()
        .get(table_key)
        .map_or(0, |data| data.columns.len());
    if column_count == 0 {
        return;
    }
    update_cell_selection(ui, table_key, row, 0, false);
    update_cell_selection(ui, table_key, row, column_count - 1, true);
    if let Some(state) = ui.table_edit_states.borrow().get(table_key) {
        state.selected_cell.set(Some((row, 0)));
    }
    if let Some(cell) = ui
        .table_grids
        .borrow()
        .get(table_key)
        .and_then(|grid| grid.child_at(1, row as i32 + 1))
    {
        cell.grab_focus();
    }
}

fn update_cell_selection(ui: &Rc<Ui>, table_key: &str, row: usize, column: usize, extend: bool) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    let position = (row, column);
    if !extend || state.selection_anchor.get().is_none() {
        state.selection_anchor.set(Some(position));
    }
    state.selection_extent.set(Some(position));
    state.selected_cell.set(Some(position));
    state.selected_column.set(None);
    state.bulk_allowed.set(false);
    state.copied_bounds.set(None);
    let Some(grid) = ui.table_grids.borrow().get(table_key).cloned() else {
        return;
    };
    let anchor = state.selection_anchor.get().unwrap_or(position);
    let min_row = anchor.0.min(row);
    let max_row = anchor.0.max(row);
    let min_column = anchor.1.min(column);
    let max_column = anchor.1.max(column);
    let mut child = grid.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        widget.remove_css_class("grid-cell-selected");
        widget.remove_css_class("grid-cell-range");
        widget.remove_css_class("grid-cell-copied");
        widget.remove_css_class("grid-column-selected");
        widget.remove_css_class("grid-row-header-selected");
    }
    for selected_row in min_row..=max_row {
        for selected_column in min_column..=max_column {
            if let Some(widget) = grid.child_at(selected_column as i32 + 1, selected_row as i32 + 1)
            {
                if (selected_row, selected_column) == position {
                    widget.add_css_class("grid-cell-selected");
                } else {
                    widget.add_css_class("grid-cell-range");
                }
            }
        }
        if min_column == 0
            && max_column + 1
                == ui
                    .table_data
                    .borrow()
                    .get(table_key)
                    .map_or(0, |data| data.columns.len())
            && let Some(row_header) = grid.child_at(0, selected_row as i32 + 1)
        {
            row_header.add_css_class("grid-row-header-selected");
        }
    }
    update_table_edit_controls(&state, None);
}

fn clear_cell_selection(ui: &Rc<Ui>, table_key: &str) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.editing_cell.get().is_some() || state.editing_column.get().is_some() {
        return;
    }
    state.selected_cell.set(None);
    state.selected_column.set(None);
    state.selection_anchor.set(None);
    state.selection_extent.set(None);
    state.copied_bounds.set(None);
    state.bulk_allowed.set(false);
    if let (Some(grid), Some(data)) = (
        ui.table_grids.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
    ) {
        render_table(ui, table_key, &grid, &data);
    }
}

fn selected_cell_bounds(state: &TableEditState) -> Option<((usize, usize), (usize, usize))> {
    let anchor = state.selection_anchor.get().or(state.selected_cell.get())?;
    let extent = state.selection_extent.get().or(state.selected_cell.get())?;
    Some((
        (anchor.0.min(extent.0), anchor.1.min(extent.1)),
        (anchor.0.max(extent.0), anchor.1.max(extent.1)),
    ))
}

fn can_edit_table_cells(ui: &Ui, table_key: &str, data: &TableData) -> bool {
    if table_key
        .strip_prefix("sql::")
        .and_then(|id| {
            ui.sql_tabs
                .borrow()
                .get(id)
                .map(|t| t.transaction_state.manual || t.session.has_pending_transaction())
        })
        .unwrap_or(false)
    {
        return false;
    }
    let is_table = ui
        .table_objects
        .borrow()
        .get(table_key)
        .is_some_and(|object| object.kind == database::DbObjectKind::Table);
    let can_write = ui
        .table_profiles
        .borrow()
        .get(table_key)
        .is_some_and(|profile| !profile.read_only);
    is_table
        && can_write
        && !data.primary_key.is_empty()
        && data
            .primary_key
            .iter()
            .all(|key| data.columns.contains(key))
}

fn run_table_row_action(ui: &Rc<Ui>, table_key: &str, row: usize, action: &'static str) {
    run_table_row_action_authorized(ui, table_key, row, action, false);
}
fn run_table_row_action_authorized(
    ui: &Rc<Ui>,
    table_key: &str,
    row: usize,
    action: &'static str,
    approved: bool,
) {
    let (Some(data), Some(object), Some(profile)) = (
        ui.table_data.borrow().get(table_key).cloned(),
        ui.table_objects.borrow().get(table_key).cloned(),
        ui.table_profiles.borrow().get(table_key).cloned(),
    ) else {
        return;
    };
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.saving.get()
        || !state.pending.borrow().is_empty()
        || state.bulk_column.borrow().is_some()
    {
        state
            .status
            .set_text("Сначала сохраните или отмените текущие изменения");
        return;
    }
    if profile.read_only
        || object.kind != database::DbObjectKind::Table
        || (table_key.starts_with("sql::")
            && (action != "Удалить строку" || !can_edit_table_cells(ui, table_key, &data)))
    {
        state
            .status
            .set_text("Операция доступна только для таблиц в подключении с правом записи");
        return;
    }
    let session_key = profile_session_key(&profile);
    let Some((_, pool)) = ui.sessions.borrow().get(&session_key).cloned() else {
        return;
    };
    let primary_key_values = if let Some(row_data) = data.rows.get(row) {
        let Some(values) = data
            .primary_key
            .iter()
            .map(|key| {
                data.columns
                    .iter()
                    .position(|column| column == key)
                    .and_then(|index| row_data.get(index).cloned())
            })
            .collect::<Option<Vec<_>>>()
        else {
            state
                .status
                .set_text("Не удалось определить первичный ключ строки");
            return;
        };
        values
    } else if action == "Добавить строку" {
        Vec::new()
    } else {
        return;
    };
    if matches!(profile.environment, EnvironmentTag::Prod) && !approved {
        let owner = Rc::downgrade(ui);
        let key = table_key.to_owned();
        let snapshot = data.clone();
        let details = format!(
            "{action}: {}.{}\nКлюч строки: {:?}\nОперация затрагивает одну строку.",
            object.schema, object.name, primary_key_values
        );
        production_ui::confirm(ui, &profile, action, &details, move || {
            if let Some(ui) = owner.upgrade() {
                if ui
                    .table_data
                    .borrow()
                    .get(&key)
                    .is_some_and(|data| Arc::ptr_eq(data, &snapshot))
                {
                    run_table_row_action_authorized(&ui, &key, row, action, true);
                } else {
                    ui.status
                        .set_text("Таблица изменилась после подтверждения. Повторите действие.");
                }
            }
        });
        return;
    }
    state.saving.set(true);
    if let Some(grid) = ui.tabulator_grids.borrow().get(table_key) {
        grid.set_locked(true);
    }
    let sql_session = table_key
        .strip_prefix("sql::")
        .and_then(|id| ui.sql_tabs.borrow().get(id).map(|tab| tab.session.clone()));
    let sender = ui.sender.clone();
    let table_key = table_key.to_owned();
    let action_name = action.to_owned();
    let production = matches!(profile.environment, EnvironmentTag::Prod);
    let runtime = ui.runtime.clone();
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let result = match action {
            "Добавить строку" => match pool.insert_default_table_row(&object).await {
                Ok(count) => Ok(count),
                Err(default_error) if !production && !primary_key_values.is_empty() => pool
                    .clone_table_row(&object, &primary_key_values)
                    .await
                    .map_err(|clone_error| {
                        format!("{default_error}; копирование выбранной строки тоже не удалось: {clone_error}")
                    }),
                Err(error) => Err(error),
            },
            "Клонировать строку" => {
                pool.clone_table_row(&object, &primary_key_values).await
            }
            "Удалить строку" => {
                match sql_session { Some(session) => if approved {session.delete_row_authorized(&object,&primary_key_values,true).await}else{session.delete_row(&object, &primary_key_values).await}, None => pool.delete_table_row(&object, &primary_key_values).await }
            }
            _ => Err("Неизвестная операция".to_owned()),
        };
        let _ = sender.send(Event::TableRowChanged(table_key, action_name, result));
    });
}

fn discard_table_edits(ui: &Rc<Ui>, table_key: &str) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.saving.get() {
        return;
    }
    state.editing_cell.set(None);
    state.editing_column.set(None);
    state.pending.borrow_mut().clear();
    state.bulk_column.borrow_mut().take();
    state.grid_editing.set(false);
    clear_cell_selection(ui, table_key);
    update_table_edit_controls(&state, None);
    if let (Some(tabulator), Some(data)) = (
        ui.tabulator_grids.borrow().get(table_key),
        ui.table_data.borrow().get(table_key),
    ) {
        tabulator.set_data(
            &data.columns,
            &grid_rows(data),
            can_edit_table_cells(ui, table_key, data),
            !table_key.starts_with("sql::") && data.truncated && !data.primary_key.is_empty(),
            !table_key.starts_with("sql::"),
            &table_key
                .strip_prefix("sql::")
                .and_then(|id| {
                    ui.sql_tabs
                        .borrow()
                        .get(id)
                        .map(|tab| tab.read_only_columns.clone())
                })
                .unwrap_or_else(|| data.primary_key.clone()),
            &grid_blocked_cells(data),
        );
    }
}

fn edit_preview_value(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "NULL".to_owned();
    };
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(240).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix:?} (фрагмент)")
    } else {
        format!("{prefix:?}")
    }
}

fn edit_preview_original(value: &CellValue) -> String {
    match value {
        CellValue::Text(text) => edit_preview_value(Some(text)),
        CellValue::Null => edit_preview_value(None),
        CellValue::Truncated(text) => format!("Усечено: {}", edit_preview_value(Some(text))),
        CellValue::DecodeError(_) => "Ошибка чтения".to_owned(),
    }
}

fn save_table_edits(ui: &Rc<Ui>, table_key: &str) {
    save_table_edits_confirmed(ui, table_key, false);
}

fn save_table_edits_confirmed(ui: &Rc<Ui>, table_key: &str, confirmed: bool) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.saving.get() || state.grid_editing.get() {
        return;
    }
    if state.editing_cell.get().is_some() {
        finish_inline_cell_edit(ui, table_key);
    }
    if state.editing_column.get().is_some() {
        finish_inline_column_edit(ui, table_key);
    }
    let (Some(object), Some(data), Some(profile)) = (
        ui.table_objects.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
        ui.table_profiles.borrow().get(table_key).cloned(),
    ) else {
        return;
    };
    if profile.read_only {
        state
            .status
            .set_text("Профиль подключения работает в режиме только чтение");
        return;
    }
    let session_key = profile_session_key(&profile);
    let Some(pool) = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(_, pool)| pool.clone())
    else {
        state.status.set_text("Нет активного подключения к БД");
        return;
    };
    let bulk_column = state.bulk_column.borrow().clone();
    let cell_changes = state.pending.borrow().clone();
    if bulk_column.is_none() && cell_changes.is_empty() {
        return;
    }
    let sql_id = table_key.strip_prefix("sql::");
    let sql_session =
        sql_id.and_then(|id| ui.sql_tabs.borrow().get(id).map(|tab| tab.session.clone()));
    let production = matches!(profile.environment, EnvironmentTag::Prod);
    if !confirmed {
        const PREVIEW_EDITS: usize = 50;

        let mut details = format!("Таблица {:?}.{:?}\n\n", object.schema, object.name);
        if let Some((column, value)) = &bulk_column {
            let Some(name) = data.columns.get(*column) else {
                state
                    .status
                    .set_text("Выбранный столбец больше не доступен");
                return;
            };
            details.push_str(&format!(
                "Массовая замена столбца {name:?}\nНовое значение: {}\nБудут обновлены все строки таблицы, включая строки вне текущего фильтра.",
                edit_preview_value(value.as_deref())
            ));
        } else {
            if !can_edit_table_cells(ui, table_key, &data) {
                state.status.set_text(
                    "Правки ячеек недоступны. Проверьте первичный ключ и режим транзакции.",
                );
                return;
            }
            let mut changes = cell_changes.iter().collect::<Vec<_>>();
            changes.sort_unstable_by_key(|(position, _)| **position);
            let row_count = changes
                .chunk_by(|(left, _), (right, _)| left.0 == right.0)
                .count();
            details.push_str(&format!(
                "Строк в наборе правок: {row_count} · Ячеек: {}\n",
                changes.len()
            ));
            if changes.len() > PREVIEW_EDITS {
                details.push_str(&format!(
                    "Показаны первые {PREVIEW_EDITS} правок. Будут применены все правки.\n"
                ));
            }
            let keys = data
                .primary_key
                .iter()
                .filter_map(|key| {
                    data.columns
                        .iter()
                        .position(|name| name == key)
                        .map(|index| (key, index))
                })
                .collect::<Vec<_>>();
            for ((row, column), value) in changes.into_iter().take(PREVIEW_EDITS) {
                let (Some(values), Some(name)) = (data.rows.get(*row), data.columns.get(*column))
                else {
                    state
                        .status
                        .set_text("Таблица обновилась; отмените и повторите изменения");
                    return;
                };
                let Some(original) = values.get(*column) else {
                    state
                        .status
                        .set_text("Таблица обновилась; отмените и повторите изменения");
                    return;
                };
                let key = keys
                    .iter()
                    .filter_map(|(name, index)| {
                        values
                            .get(*index)
                            .map(|value| format!("{name:?} = {}", edit_preview_original(value)))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                details.push_str(&format!(
                    "\nСтрока {} · {key} · {name:?}\n  Было: {}\n  Станет: {}\n",
                    row + 1,
                    edit_preview_original(original),
                    edit_preview_value(value.as_deref())
                ));
            }
        }
        let owner = Rc::downgrade(ui);
        let key = table_key.to_owned();
        let snapshot = Arc::downgrade(&data);
        let sql_snapshot = sql_session
            .as_ref()
            .map(|session| (session.identity(), session.revision()));
        production_ui::confirm_table_edits(ui, &profile, &details, move || {
            let Some(ui) = owner.upgrade() else {
                return;
            };
            let unchanged = ui
                .table_data
                .borrow()
                .get(&key)
                .is_some_and(|current| snapshot.ptr_eq(&Arc::downgrade(current)))
                && ui
                    .table_edit_states
                    .borrow()
                    .get(&key)
                    .is_some_and(|current| Rc::ptr_eq(current, &state))
                && !state.saving.get()
                && !state.grid_editing.get()
                && state.editing_cell.get().is_none()
                && state.editing_column.get().is_none()
                && *state.pending.borrow() == cell_changes
                && *state.bulk_column.borrow() == bulk_column
                && ui.table_objects.borrow().get(&key).is_some_and(|current| {
                    current.schema == object.schema
                        && current.name == object.name
                        && current.kind == object.kind
                })
                && ui.table_profiles.borrow().get(&key).is_some_and(|current| {
                    !current.read_only
                        && production == matches!(current.environment, EnvironmentTag::Prod)
                        && profile_session_key(current) == session_key
                })
                && ui
                    .sessions
                    .borrow()
                    .get(&session_key)
                    .is_some_and(|(_, current)| pool.same_pool(current))
                && key.strip_prefix("sql::").and_then(|id| {
                    ui.sql_tabs
                        .borrow()
                        .get(id)
                        .map(|tab| (tab.session.identity(), tab.session.revision()))
                }) == sql_snapshot;
            if unchanged {
                save_table_edits_confirmed(&ui, &key, true);
            } else {
                ui.status
                    .set_text("Данные или правки изменились. Откройте предпросмотр заново.");
            }
        });
        return;
    }

    if let Some((column, value)) = bulk_column {
        let Some(name) = data.columns.get(column).cloned() else {
            state
                .status
                .set_text("Выбранный столбец больше не доступен");
            return;
        };
        state.saving.set(true);
        state.save_button.set_sensitive(false);
        state.save_button.remove_css_class("table-save-ready");
        state.discard_button.set_sensitive(false);
        state.bulk_button.set_sensitive(false);
        if let Some(grid) = ui.tabulator_grids.borrow().get(table_key) {
            grid.set_locked(true);
        }
        state
            .status
            .set_text("Проверяю и сохраняю массовое изменение…");
        let sender = ui.sender.clone();
        let table_key = table_key.to_owned();
        let runtime = ui.runtime.clone();
        let guard = ui.activity.start();
        runtime.spawn(async move {
            let _guard = guard;
            let result = pool
                .update_table_column(&object, &name, value.as_deref())
                .await;
            let _ = sender.send(Event::TableEditsSaved(table_key, result));
        });
        return;
    }
    if cell_changes.is_empty() {
        return;
    }
    if !can_edit_table_cells(ui, table_key, &data) {
        state
            .status
            .set_text("Без первичного ключа строки нельзя безопасно обновить");
        return;
    }
    let key_indices = data
        .primary_key
        .iter()
        .filter_map(|key| data.columns.iter().position(|name| name == key))
        .collect::<Vec<_>>();
    if key_indices.len() != data.primary_key.len() {
        state
            .status
            .set_text("Первичный ключ не полностью загружен");
        return;
    }
    let mut edits = Vec::with_capacity(cell_changes.len());
    for ((row_index, column_index), value) in cell_changes {
        let (Some(row), Some(column)) = (data.rows.get(row_index), data.columns.get(column_index))
        else {
            state
                .status
                .set_text("Таблица обновилась; отмените и повторите изменения");
            return;
        };
        let primary_key_values = key_indices
            .iter()
            .map(|index| row[*index].clone())
            .collect();
        edits.push(TableCellEdit {
            primary_key_values,
            column: column.clone(),
            original_value: row[column_index].clone(),
            value,
        });
    }
    state.saving.set(true);
    state.save_button.set_sensitive(false);
    state.save_button.remove_css_class("table-save-ready");
    state.discard_button.set_sensitive(false);
    state.bulk_button.set_sensitive(false);
    state
        .status
        .set_text("Проверяю типы и ограничения БД, сохраняю изменения…");
    if let Some(grid) = ui.tabulator_grids.borrow().get(table_key) {
        grid.set_locked(true);
    }
    let sender = ui.sender.clone();
    let table_key = table_key.to_owned();
    let runtime = ui.runtime.clone();
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let result = match sql_session {
            Some(session) => {
                if production {
                    session.update_cells_authorized(&object, &edits, true).await
                } else {
                    session.update_cells(&object, &edits).await
                }
            }
            None => pool.update_table_cells(&object, &edits).await,
        }
        .map(|count| count as u64);
        let _ = sender.send(Event::TableEditsSaved(table_key, result));
    });
}

fn finish_inline_cell_edit(ui: &Rc<Ui>, table_key: &str) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.editing_cell.take().is_none() {
        return;
    }
    update_table_edit_controls(&state, None);
    if let (Some(grid), Some(data)) = (
        ui.table_grids.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
    ) {
        render_table(ui, table_key, &grid, &data);
    }
}

fn stage_selection_null(ui: &Rc<Ui>, table_key: &str) {
    let (Some(data), Some(state)) = (
        ui.table_data.borrow().get(table_key).cloned(),
        ui.table_edit_states.borrow().get(table_key).cloned(),
    ) else {
        return;
    };
    if !can_edit_table_cells(ui, table_key, &data)
        || state.saving.get()
        || state.bulk_column.borrow().is_some()
        || data.rows.is_empty()
        || data.columns.is_empty()
    {
        return;
    }
    let bounds = selected_cell_bounds(&state)
        .or_else(|| {
            state
                .selected_column
                .get()
                .map(|column| ((0, column), (data.rows.len().saturating_sub(1), column)))
        })
        .or_else(|| {
            state
                .selected_cell
                .get()
                .map(|(row, column)| ((row, column), (row, column)))
        });
    let Some(((min_row, min_column), (max_row, max_column))) = bounds else {
        return;
    };
    let mut changed = 0usize;
    for row in min_row..=max_row.min(data.rows.len().saturating_sub(1)) {
        for column in min_column..=max_column.min(data.columns.len().saturating_sub(1)) {
            let Some(column_name) = data.columns.get(column) else {
                continue;
            };
            if data.primary_key.contains(column_name) {
                continue;
            }
            if !matches!(
                data.rows[row].get(column),
                Some(CellValue::Text(_) | CellValue::Null)
            ) {
                continue;
            }
            let already_null = data.rows[row]
                .get(column)
                .is_some_and(|value| matches!(value, CellValue::Null));
            if already_null {
                state.pending.borrow_mut().remove(&(row, column));
            } else {
                state.pending.borrow_mut().insert((row, column), None);
            }
            changed += 1;
        }
    }
    state.copied_bounds.set(None);
    update_table_edit_controls(
        &state,
        Some(&format!(
            "Очищено ячеек: {changed}; первичные ключи пропущены"
        )),
    );
    if let (Some(grid), Some(data)) = (
        ui.table_grids.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
    ) {
        render_table(ui, table_key, &grid, &data);
    }
}

fn edit_selected_table_column(ui: &Rc<Ui>, table_key: &str) {
    if table_key.starts_with("sql::") {
        return;
    }
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.saving.get()
        || !state.pending.borrow().is_empty()
        || state.bulk_column.borrow().is_some()
    {
        return;
    }
    if state.editing_cell.get().is_some() {
        finish_inline_cell_edit(ui, table_key);
    }
    let Some(column) = state.selected_column.get() else {
        return;
    };
    if !state.bulk_allowed.get() {
        return;
    }
    let Some(data) = ui.table_data.borrow().get(table_key).cloned() else {
        return;
    };
    if data.columns.get(column).is_none() {
        return;
    }
    let Some(grid) = ui.table_grids.borrow().get(table_key).cloned() else {
        return;
    };
    let Some(old_header) = grid.child_at(column as i32 + 1, 0) else {
        return;
    };
    let entry = gtk::Entry::new();
    entry.set_hexpand(false);
    entry.set_width_chars(1);
    entry.set_max_width_chars(1);
    entry.set_size_request(old_header.width(), old_header.height());
    entry.add_css_class("grid-header");
    entry.add_css_class("grid-cell-inline-editor");
    entry.add_css_class("grid-column-selected");
    entry.set_placeholder_text(Some("Значение для всех строк; Ctrl+Delete — NULL"));
    grid.remove(&old_header);
    grid.attach(&entry, column as i32 + 1, 0, 1, 1);
    state.editing_column.set(Some(column));

    let weak = Rc::downgrade(ui);
    let edit_key = table_key.to_owned();
    entry.connect_changed(move |entry| {
        if let Some(ui) = weak.upgrade() {
            preview_bulk_column(&ui, &edit_key, column, Some(entry.text().to_string()));
        }
    });

    let weak = Rc::downgrade(ui);
    let edit_key = table_key.to_owned();
    entry.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            finish_inline_column_edit(&ui, &edit_key);
        }
    });

    let key_controller = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    let edit_key = table_key.to_owned();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Escape {
            if let Some(ui) = weak.upgrade()
                && let Some(state) = ui.table_edit_states.borrow().get(&edit_key).cloned()
            {
                state.bulk_column.borrow_mut().take();
                finish_inline_column_edit(&ui, &edit_key);
            }
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Delete && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            if let Some(ui) = weak.upgrade() {
                preview_bulk_column(&ui, &edit_key, column, None);
                finish_inline_column_edit(&ui, &edit_key);
            }
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    entry.add_controller(key_controller);

    let focus_controller = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(ui);
    let edit_key = table_key.to_owned();
    focus_controller.connect_leave(move |_| {
        let weak = weak.clone();
        let edit_key = edit_key.clone();
        gtk::glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                finish_inline_column_edit(&ui, &edit_key);
            }
        });
    });
    entry.add_controller(focus_controller);
    entry.grab_focus();
}

fn preview_bulk_column(ui: &Rc<Ui>, table_key: &str, column: usize, value: Option<String>) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    *state.bulk_column.borrow_mut() = Some((column, value.clone()));
    if let (Some(grid), Some(data)) = (
        ui.table_grids.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
    ) {
        for row in 0..data.rows.len() {
            if let Some(cell) = grid
                .child_at(column as i32 + 1, row as i32 + 1)
                .and_then(|widget| widget.downcast::<gtk::Label>().ok())
            {
                cell.set_text(value.as_deref().unwrap_or("NULL"));
                cell.add_css_class("grid-cell-edited");
                if value.is_none() {
                    cell.add_css_class("null-cell");
                } else {
                    cell.remove_css_class("null-cell");
                }
            }
        }
    }
    update_table_edit_controls(&state, None);
}

fn finish_inline_column_edit(ui: &Rc<Ui>, table_key: &str) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.editing_column.take().is_none() {
        return;
    }
    update_table_edit_controls(&state, None);
    if let (Some(grid), Some(data)) = (
        ui.table_grids.borrow().get(table_key).cloned(),
        ui.table_data.borrow().get(table_key).cloned(),
    ) {
        render_table(ui, table_key, &grid, &data);
    }
}

fn reload_table_view(ui: &Rc<Ui>, table_key: &str) {
    if let Some(id) = table_key.strip_prefix("sql::") {
        let snapshot = ui
            .sql_tabs
            .borrow()
            .get(id)
            .and_then(|tab| tab.executed_sql.clone());
        if let Some(snapshot) = snapshot {
            run_sql_tab_with_text(ui, id, Some(snapshot));
        }
        return;
    }
    load_table_view(ui, table_key, None);
}

fn load_table_view(ui: &Rc<Ui>, table_key: &str, saved_view: Option<SavedViewLayout>) -> bool {
    if let Some(state) = ui.table_edit_states.borrow().get(table_key)
        && (state.saving.get()
            || state.grid_editing.get()
            || !state.pending.borrow().is_empty()
            || state.bulk_column.borrow().is_some())
    {
        state
            .status
            .set_text("Сначала сохраните или отмените изменения перед обновлением таблицы");
        return false;
    }
    let profile = ui.table_profiles.borrow().get(table_key).cloned();
    let object = ui.table_objects.borrow().get(table_key).cloned();
    let (Some(profile), Some(object)) = (profile, object) else {
        return false;
    };
    let session_key = profile_session_key(&profile);
    if block_if_broken(
        ui,
        &session_key,
        Some(RetryAction::LoadTable(table_key.to_owned())),
    ) {
        return false;
    }
    let pool = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(_, pool)| pool.clone());
    let Some(pool) = pool else {
        return false;
    };
    let state = saved_view.as_ref().map_or_else(
        || {
            ui.table_view_states
                .borrow()
                .get(table_key)
                .cloned()
                .unwrap_or_default()
        },
        |layout| layout.view.clone(),
    );
    if let Some(grid) = ui.tabulator_grids.borrow().get(table_key) {
        grid.set_locked(true);
    }
    let request_id = Uuid::new_v4().to_string();
    ui.table_requests
        .borrow_mut()
        .insert(table_key.to_owned(), request_id.clone());
    ui.table_more_loading.borrow_mut().remove(table_key);
    ui.status
        .set_text(&format!("Обновляю {}.{}…", object.schema, object.name));
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let table_key = table_key.to_owned();
    set_table_spinner(ui, &table_key, true);
    start_table_load_timer(ui, &table_key);
    let guard = ui.activity.start();
    runtime.spawn(async move {
        let _guard = guard;
        let result = pool
            .read_table_view_with_condition(
                &object,
                &state.filters,
                state.raw_condition.as_deref(),
                state.sort.as_ref(),
                0,
            )
            .await;
        let result = report_lost(&pool, &sender, &session_key, result).await;
        let saved_view = saved_view.map(|layout| (layout, pool));
        let _ = sender.send(Event::TableLoaded(
            table_key, request_id, saved_view, result,
        ));
    });
    true
}

fn render_query_result(grid: &gtk::Grid, _data: &TableData) {
    clear_grid(grid);
}

fn render_table(_ui: &Rc<Ui>, _table_key: &str, grid: &gtk::Grid, _data: &TableData) {
    clear_grid(grid);
}

fn install_css() -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "window { font-size: 13px; }\
         .navigation-rail { padding: 10px 0; }\
         .sidebar-nav-button { min-width: 28px; min-height: 28px; padding: 2px; border-radius: 8px; }\
         .sidebar-panel { padding: 10px 8px; }\
         .add-connection-button { min-width: 28px; min-height: 28px; padding: 2px; border: 0; border-radius: 50%; background: transparent; box-shadow: none; }\
         .connection-action { min-width: 28px; min-height: 28px; padding: 2px; }\
         .penpot-icon { min-width: 28px; min-height: 28px; }\
         .grid-sort-button { min-width: 24px; min-height: 24px; padding: 2px 4px; margin: 0; border-radius: 4px; }\
         .grid-header-label { min-height: 0; padding: 3px 6px; margin: 0; }\
         .grid-column-resize-handle { min-width: 6px; background: transparent; }\
         .grid-column-resize-handle:hover { background: color-mix(in srgb, var(--accent-color) 22%, transparent); }\
         .sidebar-title { margin: 0 0 2px; }\
         .connection-card { border-radius: 8px; padding: 7px 8px; margin: 1px 0; }\
         .connection-row { border-left: 3px solid transparent; border-radius: 4px 6px 6px 4px; padding: 0 2px 0 8px; margin: 0; }\
         .connection-row-line { min-height: 30px; }\
         .connection-row:hover { background: alpha(currentColor, 0.06); }\
         .connection-row-active { background: alpha(currentColor, 0.10); }\
         .connection-row:focus-visible { outline: 1px solid alpha(currentColor, 0.4); outline-offset: -1px; }\
         .connection-row .connection-name { font-size: 13px; }\
         .connection-row .connection-action { min-width: 24px; min-height: 24px; padding: 1px; }\
         .connection-row menubutton > button { min-width: 24px; min-height: 24px; padding: 1px; }\
         .connection-row .connection-hover-action { opacity: 0; }\
         .connection-row:hover .connection-hover-action, .connection-row:focus-within .connection-hover-action { opacity: 1; }\
         .connection-row .connection-status { margin: 0 4px 6px 22px; }\
         .connection-group-header { min-height: 22px; padding: 0 6px; margin-top: 8px; font-size: 10px; font-weight: 700; opacity: 0.65; }\
         .active-connection-header { border-left-width: 4px; border-radius: 8px; padding: 8px 12px; }\
         .connection-details { font-size: 0.84em; }\
         .app-footer { min-height: 32px; padding: 0 14px; }\
         .app-header-tabs > header.top > tabs { min-height: 32px; }\
         .app-header-tabs > header.top > tabs > tab { min-height: 32px; }\
         .compact-tab-label { min-height: 18px; }\
         .compact-tab-close { min-width: 18px; min-height: 18px; padding: 0; }\
         .tab-object-icon { min-width: 18px; min-height: 18px; }\
         .schema-tree-row { min-height: 25px; padding: 0 3px; border-radius: 4px; font-weight: 400; }\
         .schema-tree-row label, .table-nav-item label { font-weight: 400; }\
         .table-nav-item { min-height: 24px; padding: 0 3px; border-radius: 4px; font-weight: 400; }\
         .table-filter-panel { padding: 6px 8px; }\
         .table-filter-panel button { min-height: 26px; padding: 2px 7px; border-radius: 5px; }\
         .table-edit-toolbar button { min-height: 26px; padding: 2px 8px; border-radius: 5px; }\
         .table-filter-row { padding: 1px 0; }\
         .table-filter-row entry, .table-filter-row combobox { min-height: 26px; padding: 3px 7px; border-radius: 5px; }\
         .table-filter-row button { min-height: 26px; padding: 2px 7px; border-radius: 5px; }\
         button.grid-header { min-height: 36px; padding: 7px 10px; border-radius: 0; font-weight: 700; }\
         .table-context-popover > contents { border-radius: 8px; padding: 4px; }\
         .table-context-actions { min-width: 250px; }\
         .table-context-action { min-height: 32px; padding: 4px 9px; border-radius: 5px; font-size: 0.9em; }\
         .color-option { min-width: 34px; min-height: 30px; }\
         .color-option-selected { outline: 2px solid; outline-offset: 1px; }\
         .tag { border-radius: 8px; padding: 3px 6px; font-size: 10px; font-weight: bold; }\
         .connection-status, .test-feedback { font-size: 0.84em; }\
         .grid-header { min-width: 0; font-weight: 700; padding: 0; }\
         .grid-cell { min-width: 48px; padding: 4px 8px; border: 1px solid transparent; }\
         .grid-cell.null-cell { font-style: italic; }",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let theme_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &theme_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    theme_provider
}

fn table_object_button(object: &DbObject) -> gtk::Button {
    let button = gtk::Button::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.set_halign(gtk::Align::Start);
    let icon = if object.schema == "information_schema" {
        let icon = penpot_icon_image("entity-list");
        icon.remove_css_class("penpot-icon");
        icon.set_pixel_size(16);
        icon.set_size_request(16, 16);
        icon
    } else if object.kind == database::DbObjectKind::Table {
        let icon = penpot_icon_image("entity-table");
        icon.remove_css_class("penpot-icon");
        icon.set_pixel_size(16);
        icon.set_size_request(16, 16);
        icon
    } else {
        let icon = gtk::Image::from_icon_name(object.kind.icon_name());
        icon.set_pixel_size(16);
        icon
    };
    let label = gtk::Label::new(Some(&object.name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&icon);
    row.append(&label);
    button.set_child(Some(&row));
    button.set_halign(gtk::Align::Fill);
    button.add_css_class("flat");
    button
}

fn navigation_rail_button(icon_name: &'static str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    let icon = penpot_icon_image(icon_name);
    button.set_child(Some(&icon));
    button.add_css_class("flat");
    button.set_size_request(28, 28);
    button.set_halign(gtk::Align::Center);
    button.set_tooltip_text(Some(label));
    button
}

fn update_navigation_rail_icon(button: &gtk::Button, icon_name: &'static str) {
    let Some(image) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Image>().ok())
    else {
        return;
    };

    let state = if !button.is_sensitive() {
        "-inactive"
    } else if button.has_css_class("sidebar-nav-active") {
        "-active"
    } else {
        "-unselected"
    };
    set_penpot_icon(&image, &format!("{icon_name}{state}"), false);
    image.set_pixel_size(28);
    image.set_size_request(28, 28);
}

fn set_penpot_icon(image: &gtk::Image, name: &str, hovered: bool) {
    icons::set(image, name, hovered);
}

fn penpot_icon_image(name: &str) -> gtk::Image {
    let image = gtk::Image::new();
    set_penpot_icon(&image, name, false);
    image.set_pixel_size(28);
    image.set_size_request(28, 28);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image.add_css_class("penpot-icon");
    image
}

fn profile_tab_icon(name: &str, profile_color: &str) -> gtk::Image {
    let accent = match profile_color_class(profile_color) {
        "connection-color-teal" => "#42b8b0",
        "connection-color-green" => "#55b879",
        "connection-color-yellow" => "#e4b84f",
        "connection-color-orange" => "#e58b4f",
        "connection-color-red" => "#df626b",
        "connection-color-purple" => "#a57be8",
        _ if profile_color == "#e2c94f" => "#e4b84f",
        _ => "#4f8fe8",
    };
    let image = icons::profile(name, accent);
    image.set_pixel_size(18);
    image.set_size_request(18, 18);
    image.add_css_class("tab-object-icon");
    image
}

fn set_button_icon_size(button: &gtk::Button, size: i32) {
    if let Some(image) = button
        .child()
        .and_then(|child| child.downcast::<gtk::Image>().ok())
    {
        image.set_pixel_size(size);
    }
}

fn set_penpot_icon_hover(widget: &impl IsA<gtk::Widget>, image: &gtk::Image, name: &'static str) {
    let motion = gtk::EventControllerMotion::new();
    let hover_image = image.clone();
    motion.connect_enter(move |_, _, _| {
        set_penpot_icon(&hover_image, name, true);
    });
    let normal_image = image.clone();
    motion.connect_leave(move |_| {
        set_penpot_icon(&normal_image, name, false);
    });
    widget.add_controller(motion);
}

fn navigation_section(title: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_margin_top(8);
    label.set_margin_bottom(2);
    label.add_css_class("navigation-section");
    label
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}
fn clear_grid(widget: &gtk::Grid) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;

    fn sql(needed_approval: bool, in_transaction: bool) -> RetryAction {
        RetryAction::Sql {
            tab_id: "tab".to_owned(),
            dialect: sql_risk::Dialect::MySql,
            needed_approval,
            in_transaction,
        }
    }

    #[test]
    fn plain_read_sql_can_be_retried() {
        assert!(retry_allowed(&sql(false, false)));
    }

    #[test]
    fn postgres_sql_stays_out_of_reconnect_retries() {
        assert!(!retry_allowed(&RetryAction::Sql {
            tab_id: "tab".to_owned(),
            dialect: sql_risk::Dialect::PostgreSql,
            needed_approval: false,
            in_transaction: false,
        }));
    }

    #[test]
    fn sql_that_needed_write_approval_is_never_retried() {
        assert!(!retry_allowed(&sql(true, false)));
    }

    #[test]
    fn sql_inside_open_transaction_is_never_retried() {
        assert!(!retry_allowed(&sql(false, true)));
    }

    #[test]
    fn table_load_can_be_retried() {
        assert!(retry_allowed(&RetryAction::LoadTable("t".to_owned())));
    }

    #[test]
    fn idle_check_is_not_due_at_exactly_sixty_seconds() {
        let last = std::time::Instant::now();
        assert!(!idle_check_due(last, last + Duration::from_secs(60)));
    }

    #[test]
    fn idle_check_is_due_after_sixty_seconds() {
        let last = std::time::Instant::now();
        assert!(idle_check_due(last, last + Duration::from_secs(61)));
    }
}

#[cfg(test)]
#[path = "../tests/support/grid_app.rs"]
mod grid_app_tests;

#[cfg(test)]
#[path = "../tests/support/export_app.rs"]
mod export_app_tests;

#[cfg(test)]
#[path = "../tests/support/production_app.rs"]
mod production_app_tests;

#[cfg(test)]
#[path = "../tests/support/batch_app.rs"]
mod batch_app_tests;

#[cfg(test)]
#[path = "../tests/support/messages_app.rs"]
mod messages_app_tests;

#[cfg(test)]
#[path = "../tests/support/groups_app.rs"]
mod groups_app_tests;

#[cfg(test)]
#[path = "../tests/support/schema_app.rs"]
mod schema_app_tests;

#[cfg(test)]
#[path = "../tests/support/namespace_app.rs"]
mod namespace_app_tests;

#[cfg(test)]
#[path = "../tests/support/schema_open_app.rs"]
mod schema_open_app_tests;
