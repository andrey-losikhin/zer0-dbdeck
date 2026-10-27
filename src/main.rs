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
mod dialogs;
mod icons;
mod native_backup;
mod output_file;
mod preferences_ui;
mod production_ui;
mod secret_store;
mod settings;
mod shortcuts;
mod sql_editor;
mod sql_history_ui;
mod sql_risk;
mod sql_session;
mod sql_workspace;
mod storage_ui;
mod structure_ui;
mod tabulator;
mod theme;
mod transfer;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use database::{
    CellValue, DbObject, DbPool, FilterLogic, FilterOperator, TableCatalog, TableCellEdit,
    TableData, TableFilter, TableSort,
};
use relm4::adw::prelude::*;
use relm4::{adw, gtk};
use settings::{
    ConnectionProfile, DatabaseKind, EnvironmentTag, ProfileSaveError, SettingsStore, TlsMode,
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
    TablesLoaded(u64, u64, Result<TableCatalog, String>),
    DatabasesLoaded(u64, u64, Result<Vec<String>, String>),
    TableLoaded(String, String, String, Result<TableData, String>),
    TablePageLoaded(String, String, usize, Result<TableData, String>),
    TableEditsSaved(String, Result<u64, String>),
    TableRowChanged(String, String, Result<u64, String>),
    SqlQueryFinished(
        String,
        String,
        Result<database::QueryResult, String>,
        sql_session::TransactionState,
        u128,
    ),
    SqlTransactionFinished(String, Result<sql_session::TransactionState, String>),
    SqlCompletionsLoaded(String, Result<Vec<String>, String>),
    CsvExported(usize, Result<(), String>),
    ExportProgress(usize),
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
    Error(String),
}

struct Ui {
    remember_unlock_button: gtk::Button,
    unlock_provider: gtk::ComboBoxText,
    storage_path_label: gtk::Label,
    transfer_cancel: RefCell<Option<Arc<transfer::Cancellation>>>,
    transfer_cancel_button: gtk::Button,
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
    connections_nav: gtk::Button,
    tables_nav: gtk::Button,
    profiles: gtk::Box,
    objects: gtk::Box,
    table_filter: gtk::SearchEntry,
    table_list_items: RefCell<Vec<(gtk::Button, DbObject)>>,
    landing: gtk::Box,
    empty_session: gtk::Box,
    active_header: gtk::Box,
    active_details: gtk::Label,
    session_tabs: gtk::Notebook,
    table_tabs: gtk::Stack,
    session_table_tabs: RefCell<HashMap<String, gtk::Notebook>>,
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
}

struct SqlTab {
    session_key: String,
    profile_id: String,
    session: sql_session::SqlSession,
    transaction_state: sql_session::TransactionState,
    transaction_label: gtk::Label,
    transaction_buttons: Vec<gtk::Button>,
    actions_menu: gtk::MenuButton,
    page: gtk::Box,
    buffer: sourceview5::Buffer,
    editor: sourceview5::View,
    grid: gtk::Grid,
    tabulator: TabulatorGrid,
    status: gtk::Label,
    export_button: gtk::Button,
    copy_button: gtk::Button,
    result_stack: gtk::Stack,
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
}

#[derive(Clone, Default)]
struct TableViewState {
    filters: Vec<TableFilter>,
    raw_condition: Option<String>,
    sort: Option<TableSort>,
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
}

fn main() {
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
        .application_id("io.github.andrey-losikhin.zer0-dbdeck")
        .build();
    app.connect_activate(move |app| {
        build_window(app, runtime.clone());
    });
    app.run();
}

fn build_window(app: &adw::Application, runtime: Arc<tokio::runtime::Runtime>) -> Rc<Ui> {
    dialogs::watch_toplevels();
    let (sender, receiver) = mpsc::channel();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DBDeck")
        .default_width(1280)
        .default_height(820)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some("DBDeck"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));
    let application_menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Меню приложения (Alt+M)")
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
    header.pack_end(&application_menu);
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
    root.append(&header);

    let body = gtk::Paned::new(gtk::Orientation::Horizontal);
    body.set_vexpand(true);
    body.set_position(318);
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
    tables_nav.set_sensitive(false);
    update_navigation_rail_icon(&tables_nav, "tables");
    let settings_button = navigation_rail_button("settings", "Общие настройки");
    settings_button.add_css_class("sidebar-nav-button");
    update_navigation_rail_icon(&settings_button, "settings");
    navigation_rail.append(&connections_nav);
    navigation_rail.append(&tables_nav);
    navigation_rail.append(&settings_button);
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
    let profiles = gtk::Box::new(gtk::Orientation::Vertical, 6);
    profiles.set_spacing(12);
    let profile_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&profiles)
        .build();
    sidebar.append(&profile_scroll);
    let objects_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    objects_page.add_css_class("sidebar-panel");
    let table_filter = gtk::SearchEntry::new();
    table_filter.set_placeholder_text(Some("Фильтр таблиц"));
    table_filter.set_visible(false);
    objects_page.append(&table_filter);
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

    let workspace = gtk::Box::new(gtk::Orientation::Vertical, 8);
    workspace.set_margin_start(12);
    workspace.set_margin_end(14);
    let session_tabs = gtk::Notebook::new();
    session_tabs.set_scrollable(true);
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
    active_header.set_visible(false);
    workspace.append(&active_header);

    let table_tabs = gtk::Stack::new();
    table_tabs.set_transition_type(gtk::StackTransitionType::None);
    table_tabs.set_hexpand(true);
    table_tabs.set_vexpand(true);
    table_tabs.set_visible(false);
    let landing = gtk::Box::new(gtk::Orientation::Vertical, 14);
    landing.set_halign(gtk::Align::Center);
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
    let landing_description = gtk::Label::new(Some(
        "Создайте профиль PostgreSQL или MySQL/MariaDB,\nлибо выберите сохранённое подключение слева.",
    ));
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
    empty_session.set_halign(gtk::Align::Center);
    empty_session.set_valign(gtk::Align::Center);
    empty_session.set_hexpand(true);
    empty_session.set_vexpand(true);
    empty_session.set_visible(false);
    workspace.append(&empty_session);
    workspace.append(&table_tabs);
    body.set_end_child(Some(&workspace));
    root.append(&body);
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    footer.set_margin_start(14);
    footer.set_margin_end(14);
    footer.add_css_class("app-footer");
    let status = gtk::Label::new(Some("Готово"));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    footer.append(&status);
    let transfer_cancel_button = gtk::Button::with_label("Отменить операцию");
    transfer_cancel_button.set_visible(false);
    footer.append(&transfer_cancel_button);
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
        transfer_cancel_button,
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
        connections_nav: connections_nav.clone(),
        tables_nav: tables_nav.clone(),
        profiles,
        objects,
        table_filter,
        table_list_items: RefCell::new(Vec::new()),
        landing,
        empty_session,
        active_header,
        active_details,
        session_tabs,
        table_tabs,
        session_table_tabs: RefCell::new(HashMap::new()),
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
    let weak = Rc::downgrade(&ui);
    ui.transfer_cancel_button.connect_clicked(move |button| {
        if let Some(ui) = weak.upgrade() {
            if let Some(cancel) = ui.transfer_cancel.borrow().as_ref() {
                let accepted = cancel.request();
                ui.status.set_text(if accepted {
                    "Отменяю экспорт…"
                } else {
                    "Завершаю запись файла…"
                });
            }
            button.set_sensitive(false);
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
        if let Some(ui) = weak.upgrade()
            && ui.objects_available.get()
        {
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
        select_session(&ui, session_key.as_str());
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
    gtk::glib::timeout_add_local(Duration::from_millis(80), move || {
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
    if profiles.is_empty() {
        let empty = gtk::Label::new(Some("Подключений пока нет\nНажмите «＋ Подключение»"));
        empty.set_wrap(true);
        empty.set_margin_top(18);
        empty.set_margin_bottom(18);
        empty.add_css_class("dim-label");
        ui.profiles.append(&empty);
        return;
    }
    for profile in profiles {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("connection-card");
        if ui
            .current_profile
            .borrow()
            .as_ref()
            .is_some_and(|current| current.id == profile.id)
        {
            card.add_css_class("connection-card-active");
            card.add_css_class(profile_color_class(&profile.color));
        }
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        let dot = gtk::Label::new(Some("●"));
        dot.set_markup(&format!(
            "<span foreground=\"{}\">●</span>",
            escape_markup(&profile.color)
        ));
        let name = gtk::Label::new(Some(&profile.name));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.add_css_class("heading");
        name.set_markup(&format!(
            "<span foreground=\"{}\">{}</span>",
            escape_markup(&profile.color),
            escape_markup(&profile.name)
        ));
        let default_brand = match profile.kind {
            DatabaseKind::PostgreSql => "postgresql",
            DatabaseKind::MySql => "mysql",
            DatabaseKind::MariaDb => "mariadb",
        };
        let brand = penpot_icon_image(
            ui.server_icons
                .borrow()
                .get(&profile_session_key(&profile))
                .copied()
                .unwrap_or(default_brand),
        );
        brand.set_pixel_size(22);
        brand.set_size_request(22, 22);
        brand.remove_css_class("penpot-icon");
        brand.set_tooltip_text(Some(profile.kind.label()));
        top.append(&brand);
        top.append(&dot);
        top.append(&name);
        let tag = gtk::Label::new(Some(profile.environment.label()));
        tag.add_css_class("tag");
        tag.add_css_class("environment-tag");
        match profile.environment {
            EnvironmentTag::Prod => tag.add_css_class("tag-prod"),
            EnvironmentTag::Stage => tag.add_css_class("tag-stage"),
            EnvironmentTag::Local => tag.add_css_class("tag-local"),
        }
        top.append(&tag);
        if ui
            .current_profile
            .borrow()
            .as_ref()
            .is_some_and(|current| current.id == profile.id)
        {
            let active = gtk::Label::new(Some("ОТКРЫТО"));
            active.add_css_class("tag");
            active.add_css_class("tag-active");
            top.append(&active);
        }
        if is_profile_connected(ui, &profile.id) {
            top.add_css_class("profile-card-navigable");
            install_profile_card_navigation(ui, &top, &profile);
        }
        card.append(&top);
        let address = gtk::Label::new(Some(&format!(
            "{} · {}:{}",
            profile.kind.label(),
            profile.host,
            profile.port
        )));
        address.set_xalign(0.0);
        address.set_single_line_mode(true);
        address.set_ellipsize(gtk::pango::EllipsizeMode::End);
        address.set_tooltip_text(Some(&format!(
            "{} · {}:{}",
            profile.kind.label(),
            profile.host,
            profile.port
        )));
        address.add_css_class("dim-label");
        if is_profile_connected(ui, &profile.id) {
            address.add_css_class("profile-card-navigable");
            install_profile_card_navigation(ui, &address, &profile);
        }
        card.append(&address);
        let profile_state = ui.profile_status.borrow().get(&profile.id).cloned();
        if let Some(state) = profile_state.as_ref() {
            if !matches!(state, ProfileStatus::Connected) {
                let (text, style) = match state {
                    ProfileStatus::Connecting => ("Подключение…".to_owned(), "status-pending"),
                    ProfileStatus::Verified => {
                        ("Проверка подключения успешна".to_owned(), "status-success")
                    }
                    ProfileStatus::Connected => unreachable!(),
                    ProfileStatus::Error(error) => (format!("Ошибка: {error}"), "status-error"),
                };
                let status = gtk::Label::new(Some(&text));
                status.set_xalign(0.0);
                status.set_wrap(true);
                status.add_css_class("connection-status");
                status.add_css_class(style);
                card.append(&status);
            }
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        actions.set_homogeneous(false);
        actions.set_margin_top(6);
        let is_connected = matches!(profile_state.as_ref(), Some(ProfileStatus::Connected));
        let is_connecting = matches!(profile_state.as_ref(), Some(ProfileStatus::Connecting));
        if is_connected {
            let connected_icon = penpot_icon_image("status-connected");
            connected_icon.remove_css_class("penpot-icon");
            connected_icon.set_pixel_size(22);
            connected_icon.set_size_request(22, 22);
            let disconnect = gtk::Button::new();
            disconnect.set_child(Some(&connected_icon));
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
            actions.append(&disconnect);
        } else {
            let connect = gtk::Button::new();
            let connect_icon = penpot_icon_image("status-idle");
            connect_icon.remove_css_class("penpot-icon");
            connect_icon.set_pixel_size(22);
            connect_icon.set_size_request(22, 22);
            connect.set_child(Some(&connect_icon));
            set_penpot_icon_hover(&connect, &connect_icon, "status-idle");
            connect.add_css_class("flat");
            connect.add_css_class("connection-action");
            if is_connecting {
                connect.set_tooltip_text(Some("Устанавливается подключение"));
                connect.set_sensitive(false);
            } else {
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
            actions.append(&connect);
        }
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&spacer);
        let edit = gtk::Button::new();
        let edit_icon = penpot_icon_image("edit");
        edit_icon.remove_css_class("penpot-icon");
        edit_icon.set_pixel_size(22);
        edit_icon.set_size_request(22, 22);
        edit.set_child(Some(&edit_icon));
        set_penpot_icon_hover(&edit, &edit_icon, "edit");
        edit.add_css_class("flat");
        edit.add_css_class("connection-action");
        edit.set_tooltip_text(Some("Изменить подключение"));
        edit.set_size_request(28, 28);
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
        actions.append(&edit);
        let delete = gtk::Button::new();
        let delete_icon = penpot_icon_image("delete");
        delete_icon.remove_css_class("penpot-icon");
        delete_icon.set_pixel_size(22);
        delete_icon.set_size_request(22, 22);
        delete.set_child(Some(&delete_icon));
        set_penpot_icon_hover(&delete, &delete_icon, "delete");
        delete.add_css_class("flat");
        delete.add_css_class("connection-action");
        delete.set_tooltip_text(Some("Удалить подключение"));
        delete.set_size_request(28, 28);
        let weak = Rc::downgrade(ui);
        let profile_id = profile.id.clone();
        let profile_name = profile.name.clone();
        delete.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let after_unlock = ui.clone();
                let profile_id = profile_id.clone();
                let profile_name = profile_name.clone();
                ensure_vault_unlocked(&ui, move || {
                    show_delete_profile_dialog(&after_unlock, &profile_id, &profile_name)
                });
            }
        });
        shortcuts::button(&card, &delete, "<Alt>Delete");
        actions.append(&delete);
        backup_ui::install(ui, &card, &actions, &profile);
        card.append(&actions);
        ui.profiles.append(&card);
    }
}

fn is_profile_connected(ui: &Ui, profile_id: &str) -> bool {
    ui.sessions
        .borrow()
        .values()
        .any(|(profile, _)| profile.id == profile_id)
}

fn install_profile_card_navigation(
    ui: &Rc<Ui>,
    widget: &impl IsA<gtk::Widget>,
    profile: &ConnectionProfile,
) {
    widget.set_cursor_from_name(Some("pointer"));
    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);
    let weak = Rc::downgrade(ui);
    let profile = profile.clone();
    gesture.connect_released(move |_, _, _, _| {
        if let Some(ui) = weak.upgrade() {
            select_connection_database(&ui, &profile);
        }
    });
    widget.add_controller(gesture);
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
    let session_key = profile_session_key(profile);
    if let Some(index) = session_page_index(ui, &session_key) {
        ui.session_tabs.set_current_page(Some(index));
        return;
    }
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_size_request(1, 1);
    page.set_tooltip_text(Some(&session_key));
    let database_name = if profile.database.trim().is_empty() {
        "Все доступные базы"
    } else {
        profile.database.as_str()
    };
    let title_text = format!("{} · {database_name}", profile.name);
    let title = gtk::Label::new(Some(&title_text));
    title.set_tooltip_text(Some(&format!(
        "{} · {}:{} · {}",
        profile.kind.label(),
        profile.host,
        profile.port,
        profile.database
    )));
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(22);
    title.set_max_width_chars(32);
    let tab_label = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_label.add_css_class("compact-tab-label");
    tab_label.append(&profile_tab_icon("database", &profile.color));
    tab_label.append(&title);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&close, 14);
    close.add_css_class("flat");
    close.add_css_class("circular");
    close.add_css_class("compact-tab-close");
    close.set_tooltip_text(Some("Отключить и закрыть вкладку"));
    tab_label.append(&close);
    ui.session_tabs.append_page(&page, Some(&tab_label));
    let weak = Rc::downgrade(ui);
    let profile_id = session_key.clone();
    close.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            close_session(&ui, &profile_id);
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
        if let Some(query_id) = key.strip_prefix("sql::") {
            ui.active_table_by_session
                .borrow_mut()
                .insert(name.clone(), key.clone());
            if let Some((profile, _)) = ui.sessions.borrow().get(&name).cloned() {
                *ui.current_profile.borrow_mut() = Some(profile);
            }
            ui.table_tabs.set_visible_child_name(&name);
            ui.table_tabs.set_visible(true);
            ui.empty_session.set_visible(false);
            ui.active_header.set_visible(false);
            ui.status.set_text(&format!("SQL-запрос {}", query_id));
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
        if let Some((profile, _)) = ui.sessions.borrow().get(&name).cloned() {
            *ui.current_profile.borrow_mut() = Some(profile);
        }
        ui.table_tabs.set_visible_child_name(&name);
        ui.table_tabs.set_visible(true);
        ui.empty_session.set_visible(false);
        ui.tab_switching.set(false);
    });
    ui.session_table_tabs
        .borrow_mut()
        .insert(session_key.to_owned(), notebook.clone());
    let sql_button = gtk::Button::new();
    let profile_color = ui
        .sessions
        .borrow()
        .get(session_key)
        .map(|(profile, _)| profile.color.clone())
        .unwrap_or_else(|| "#4f8fe8".to_owned());
    let sql_icon = profile_tab_icon("sql-query", &profile_color);
    sql_icon.set_pixel_size(20);
    sql_icon.set_size_request(20, 20);
    sql_button.set_child(Some(&sql_icon));
    sql_button.set_tooltip_text(Some("Открыть новую вкладку SQL-запроса"));
    sql_button.add_css_class("flat");
    sql_button.add_css_class("circular");
    sql_button.add_css_class("sql-tab-add");
    let weak = Rc::downgrade(ui);
    let query_session_key = session_key.to_owned();
    sql_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            create_sql_tab(&ui, &query_session_key);
        }
    });
    notebook.set_action_widget(&sql_button, gtk::PackType::End);
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
    let Some((profile, pool)) = ui.sessions.borrow().get(session_key).cloned() else {
        return;
    };
    if profile.database.trim().is_empty() {
        database_target_ui::choose(ui, &profile, move |ui, target| {
            let key = profile_session_key(&target);
            open_database_session(ui, target.clone(), pool.clone(), &target.database);
            let owner = Rc::downgrade(ui);
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
                    create_sql_tab(&ui, &key);
                    return gtk::glib::ControlFlow::Break;
                }
                if ui.connect_requests.borrow().contains_key(&key) {
                    gtk::glib::ControlFlow::Continue
                } else {
                    gtk::glib::ControlFlow::Break
                }
            });
        });
        return;
    }
    // Count disconnected drafts too: the encrypted workspace has one global limit.
    if !ui.restoring_drafts.get() {
        let tabs = ui.sql_tabs.borrow();
        let profiles: std::collections::HashSet<_> =
            tabs.values().map(|t| t.session_key.as_str()).collect();
        let saved = ui.store.borrow();
        let inactive = saved
            .as_ref()
            .map(|s| {
                s.sql_workspace()
                    .drafts
                    .iter()
                    .filter(|d| !profiles.contains(d.connection_key.as_str()))
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
    let notebook = ensure_session_table_tabs(ui, session_key);
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

    let completion_provider = sql_editor::SqlProvider::new();
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
    toolbar.append(&status);
    let risk_indicator = gtk::Label::new(None);
    risk_indicator.add_css_class("query-risk-indicator");
    toolbar.append(&risk_indicator);
    let transaction_label = gtk::Label::new(Some("Auto Commit"));
    transaction_label.add_css_class("dim-label");
    transaction_label.set_tooltip_text(Some(
        "Режим транзакций этой SQL-вкладки. Ctrl+Alt+T — переключить.",
    ));
    toolbar.append(&transaction_label);
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
            run_sql_tab_policy(&ui, &write_id, None, false, true);
        }
    });

    let open_button = sql_menu_action(&menu, &popover, "Открыть SQL…", "document-open-symbolic");
    let save_button = sql_menu_action(&menu, &popover, "Сохранить SQL", "document-save-symbolic");
    let rename_button =
        sql_menu_action(&menu, &popover, "Переименовать…", "document-edit-symbolic");
    let directory_button =
        sql_menu_action(&menu, &popover, "Папка запросов…", "folder-open-symbolic");
    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let export_button =
        sql_menu_action(&menu, &popover, "Экспорт CSV…", "document-save-as-symbolic");
    export_button.set_sensitive(false);
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
    editor_region.append(&error_message);
    let syntax_message = gtk::Label::new(None);
    syntax_message.set_xalign(0.0);
    syntax_message.set_ellipsize(gtk::pango::EllipsizeMode::End);
    syntax_message.set_max_width_chars(100);
    syntax_message.add_css_class("sql-syntax-diagnostic");
    syntax_message.set_visible(false);
    syntax_message.set_tooltip_text(Some("Локальная проверка синтаксиса. Существование таблиц и полей проверяет сервер при выполнении."));
    editor_region.append(&syntax_message);
    let diagnostics = sql_editor::Diagnostics::install(
        &buffer,
        &syntax_message,
        ui.runtime.clone(),
        match profile.kind {
            DatabaseKind::PostgreSql => sql_editor::Dialect::PostgreSql,
            DatabaseKind::MySql | DatabaseKind::MariaDb => sql_editor::Dialect::MySql,
        },
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
    let tabulator = TabulatorGrid::new(move |message| {
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
    let copy_result = sql_menu_action(&menu, &popover, "Копировать ячейки", "edit-copy-symbolic");
    copy_result.set_sensitive(false);
    let copy_grid = tabulator.clone();
    copy_result.connect_clicked(move |_| copy_grid.copy_selection());
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
    result_stack.set_visible_child_name("empty");
    result_region.append(&result_stack);
    result_region.append(&edit_state.toolbar);
    split.set_end_child(Some(&result_region));
    split.set_position(340);
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
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(8);
    title.set_max_width_chars(20);
    tab_label.append(&title);
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
            profile_id: profile.id.clone(),
            session: sql_session::SqlSession::with_profile(pool.clone(), &profile),
            transaction_state: sql_session::TransactionState::default(),
            transaction_label,
            transaction_buttons: vec![
                manual_button.clone(),
                commit_button.clone(),
                rollback_button.clone(),
            ],
            actions_menu: actions_button,
            page: page.clone(),
            buffer: buffer.clone(),
            editor: editor.clone(),
            grid,
            tabulator,
            status,
            export_button: export_button.clone(),
            copy_button: copy_result,
            result_stack,
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
        },
    );
    let tab_index = notebook.append_page(&page, Some(&tab_label));
    notebook.set_tab_reorderable(&page, true);
    notebook.set_current_page(Some(tab_index));
    ui.active_table_by_session
        .borrow_mut()
        .insert(session_key.to_owned(), page_key);
    ui.table_tabs.set_visible_child_name(session_key);
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
            stop_sql_tab(&ui, &stop_id);
        }
    });
    let weak = Rc::downgrade(ui);
    let reset_id = id.clone();
    reset_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            reset_sql_session(&ui, &reset_id);
        }
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
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let completion_id = id.clone();
    runtime.spawn(async move {
        let result = pool.list_sql_completions(&profile).await;
        let _ = sender.send(Event::SqlCompletionsLoaded(completion_id, result));
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
    run_sql_tab_policy(ui, id, snapshot, false, false);
}
fn run_sql_tab_policy(
    ui: &Rc<Ui>,
    id: &str,
    snapshot: Option<String>,
    approved: bool,
    force_write: bool,
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
        .zip(ui.sql_tabs.borrow().get(id).map(|tab| {
            let text = tab
                .buffer
                .text(&tab.buffer.start_iter(), &tab.buffer.end_iter(), true);
            let selected = tab
                .buffer
                .selection_bounds()
                .map(|(start, end)| tab.buffer.text(&start, &end, true).to_string());
            selected
                .filter(|selection| !selection.trim().is_empty())
                .unwrap_or_else(|| text.to_string())
        }))
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
    let Some((session, state)) = ui
        .sql_tabs
        .borrow()
        .get(id)
        .map(|t| (t.session.clone(), t.transaction_state))
    else {
        return;
    };
    let needs_approval = session.requires_write_approval(&sql, state)
        || (force_write
            && matches!(profile.environment, EnvironmentTag::Prod)
            && !profile.read_only);
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
                        run_sql_tab_policy(&ui, &id, Some(sql.clone()), true, force_write);
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
    let session = tab.session.clone();
    let timeout = tab.timeout.value_as_int() as u64;
    tab.editor.completion().hide();
    tab.executed_sql = Some(sql.clone());
    tab.request_id = request_id.clone();
    tab.status.set_text("Выполняю запрос…");
    tab.run_button.set_sensitive(false);
    tab.stop_button.set_sensitive(true);
    for button in &tab.transaction_buttons {
        button.set_sensitive(false);
    }
    tab.export_button.set_sensitive(false);
    tab.copy_button.set_sensitive(false);
    tab.result_stack.set_visible_child_name("empty");
    tab.error_message.set_text("");
    tab.error_message.set_visible(false);
    tab.result = None;
    ui.table_data.borrow_mut().remove(&key);
    ui.table_objects.borrow_mut().remove(&key);
    tab.tabulator.set_locked(true);
    while let Some(child) = tab.grid.first_child() {
        tab.grid.remove(&child);
    }
    drop(tabs);
    ui.status.set_text("Выполняю SQL-запрос…");

    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    let tab_id = id.to_owned();
    runtime.spawn(async move {
        let started = std::time::Instant::now();
        let result = if approved {
            session.execute_authorized(&sql, timeout, true).await
        } else {
            session.execute_with_timeout(&sql, timeout).await
        };
        let state = session.state().await;
        let _ = sender.send(Event::SqlQueryFinished(
            tab_id,
            request_id,
            result,
            state,
            started.elapsed().as_millis(),
        ));
    });
}

fn stop_sql_tab(ui: &Rc<Ui>, id: &str) {
    if let Some(tab) = ui.sql_tabs.borrow().get(id) {
        if tab.stop_button.is_sensitive() {
            tab.stop_button.set_sensitive(false);
            tab.status.set_text("Отменяю запрос на сервере…");
            tab.session.request_cancel();
        }
    }
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
            sql_session::SqlSession::with_profile(pool, &profile),
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
    let id = id.to_owned();
    let sender = ui.sender.clone();
    ui.runtime.spawn(async move {
        let result = session.transaction_with_approval(action, approved).await;
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
    let session_key = tab.session_key.clone();
    tab.editor.completion().hide();
    tab.diagnostics.close();
    let page = tab.page.clone();
    let style_manager = tab.style_manager.clone();
    let style_notify = tab.style_notify.take();
    let page_key = format!("sql::{id}");
    let was_active = ui
        .active_table_by_session
        .borrow()
        .get(&session_key)
        .is_some_and(|key| key == &page_key);
    drop(tabs);
    if let Some(style_notify) = style_notify {
        style_manager.disconnect(style_notify);
    }
    let notebook = ui.session_table_tabs.borrow().get(&session_key).cloned();
    if let Some(notebook) = notebook
        && let Some(index) = notebook.page_num(&page)
    {
        notebook.remove_page(Some(index));
        if was_active && notebook.n_pages() == 0 {
            ui.active_table_by_session.borrow_mut().remove(&session_key);
            if ui
                .current_profile
                .borrow()
                .as_ref()
                .is_some_and(|profile| profile_session_key(profile) == session_key)
            {
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

fn start_transfer(ui: &Rc<Ui>) -> Option<Arc<transfer::Cancellation>> {
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
    ui.transfer_cancel_button.set_sensitive(true);
    ui.transfer_cancel_button.set_visible(true);
    Some(cancel)
}
fn finish_transfer(ui: &Ui) {
    ui.transfer_cancel.borrow_mut().take();
    ui.transfer_cancel_button.set_visible(false);
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
                let Some(cancel) = start_transfer(&ui) else {
                    dialog.destroy();
                    return;
                };
                runtime.spawn_blocking(move || {
                    let rows = data.rows.len();
                    let result = if cancel.is_cancelled() {
                        Err("Экспорт отменён".into())
                    } else {
                        write_snapshot_csv(&path, &data, &cancel, |_| {})
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
    if let Some(index) = session_page_index(ui, session_key) {
        ui.session_tabs.remove_page(Some(index));
    }
    ui.sessions.borrow_mut().remove(session_key);
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
    if let Some(notebook) = ui.session_table_tabs.borrow_mut().remove(session_key) {
        ui.table_tabs.remove(&notebook);
    }
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
        ui.active_header.set_visible(false);
        ui.table_tabs.set_visible(false);
        ui.empty_session.set_visible(false);
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
    let next_id = ui.sessions.borrow().keys().next().cloned();
    if let Some(next_id) = next_id {
        if let Some(index) = session_page_index(ui, &next_id) {
            ui.session_tabs.set_current_page(Some(index));
            select_session(ui, &next_id);
        }
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
    let profile = ui.table_profiles.borrow().get(key).cloned()?;
    let session_key = profile_session_key(&profile);
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
    let session_key = ui.table_profiles.borrow().get(key).map(profile_session_key);
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
                            .map(|tab| tab.session_key.clone())
                    })
                    .as_deref()
                    == session_key.as_deref()
                {
                    return Some((index, candidate_key));
                }
                let candidate_profile = ui.table_profiles.borrow().get(&candidate_key).cloned()?;
                (session_key.as_deref() == Some(profile_session_key(&candidate_profile).as_str()))
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
        ui.table_data.borrow_mut().remove(key);
        ui.table_edit_states.borrow_mut().remove(key);
        drop(widget);
    }
    if let Some(session_key) = session_key {
        if was_active {
            if let Some((_, _, fallback)) = fallback {
                ui.active_table_by_session
                    .borrow_mut()
                    .insert(session_key.clone(), fallback.clone());
                if ui
                    .current_profile
                    .borrow()
                    .as_ref()
                    .is_some_and(|profile| profile_session_key(profile) == session_key)
                {
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
                if ui
                    .current_profile
                    .borrow()
                    .as_ref()
                    .is_some_and(|profile| profile_session_key(profile) == session_key)
                {
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
    if let Some(index) = session_page_index(ui, session_key) {
        if ui.session_tabs.current_page() != Some(index) {
            ui.session_tabs.set_current_page(Some(index));
        }
    }
    let database = if profile.database.is_empty() {
        "Все доступные базы"
    } else {
        profile.database.as_str()
    };
    ui.active.set_text(database);
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
    ui.landing.set_visible(false);
    activate_session_table(ui, session_key);
    refresh_profiles(ui);
    if changed {
        ui.objects_available.set(false);
        set_tables_nav_sensitive(ui, false);
        clear_box(&ui.objects);
        show_table_catalog(ui);
    }
}

fn activate_session_table(ui: &Ui, session_key: &str) {
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
            ui.active_header.set_visible(true);
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
        ui.active_header.set_visible(true);
        return;
    }
    ui.table_tabs.set_visible(false);
    ui.empty_session.set_visible(true);
    ui.active_header.set_visible(false);
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
    let query = ui.table_filter.text().trim().to_lowercase();
    for (button, object) in ui.table_list_items.borrow().iter() {
        let name_matches = object.name.to_lowercase().contains(&query);
        button.set_visible(name_matches);
    }
}

fn install_table_context_menu(
    ui: &Rc<Ui>,
    anchor: &gtk::Button,
    profile: ConnectionProfile,
    object: DbObject,
    table_key: String,
) {
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
    if page == "tables" && !ui.objects_available.get() {
        return;
    }
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

fn set_tables_nav_sensitive(ui: &Ui, sensitive: bool) {
    ui.tables_nav.set_sensitive(sensitive);
    update_navigation_rail_icon(&ui.tables_nav, "tables");
}

fn show_delete_profile_dialog(ui: &Rc<Ui>, profile_id: &str, profile_name: &str) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .buttons(gtk::ButtonsType::YesNo)
        .text(format!("Удалить подключение «{profile_name}»?"))
        .secondary_text("Сохранённый профиль и его пароль будут удалены с этого компьютера.")
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
    let master_password = gtk::PasswordEntry::builder()
        .placeholder_text(if exists {
            "Мастер-пароль"
        } else {
            "Мастер-пароль (минимум 12 символов)"
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
    chooser.set_current_name(&format!("{table_name}.csv"));
    let weak = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) =
                (weak.upgrade(), dialog.file().and_then(|file| file.path()))
            {
                remember_csv_directory(&ui, &path);
                let Some(cancel) = start_transfer(&ui) else {
                    dialog.destroy();
                    return;
                };
                let sender = ui.sender.clone();
                let data = data.clone();
                ui.runtime.spawn_blocking(move || {
                    let rows = data.rows.len();
                    let result = if cancel.is_cancelled() {
                        Err("Экспорт отменён".into())
                    } else {
                        write_snapshot_csv(&path, &data, &cancel, |_| {})
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
    chooser.set_current_name(&format!("{table_name}.csv"));
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
                let Some(cancel) = start_transfer(&ui) else {
                    dialog.destroy();
                    return;
                };
                let task_runtime = runtime.clone();
                runtime.spawn_blocking(move || {
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
        .default_height(680)
        .default_width(600)
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
    password_note.add_css_class("dim-label");
    content.append(&password_note);
    let test_feedback = gtk::Label::new(Some("Проверка соединения ещё не запускалась"));
    test_feedback.set_xalign(0.0);
    test_feedback.set_wrap(true);
    test_feedback.add_css_class("test-feedback");
    content.append(&test_feedback);
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
        runtime.spawn(async move {
            let result = match resolve_profile_password(&profile, password_text).await {
                Ok(secret) => match DbPool::connect(&profile, &secret).await {
                    Ok(pool) => pool.check().await,
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
        content.append(&label);
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
    runtime.spawn(async move {
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

fn show_table_catalog(ui: &Rc<Ui>) {
    let (Some(pool), Some(profile)) = (
        ui.pool.borrow().clone(),
        ui.current_profile.borrow().clone(),
    ) else {
        return;
    };
    ui.status.set_text("Загружаю таблицы и представления…");
    if matches!(profile.kind, DatabaseKind::PostgreSql) {
        catalog_tree::prepare(ui, &profile);
    } else {
        ui.objects.set_sensitive(false);
        clear_box(&ui.objects);
        ui.table_list_items.borrow_mut().clear();
        ui.selected_table_button.borrow_mut().take();
    }
    let generation = ui.connection_generation.get();
    let view_generation = ui.view_generation.get().wrapping_add(1);
    ui.view_generation.set(view_generation);
    let sender = ui.sender.clone();
    let runtime = ui.runtime.clone();
    runtime.spawn(async move {
        let event = if matches!(profile.kind, DatabaseKind::PostgreSql)
            && profile.database.trim().is_empty()
        {
            Event::DatabasesLoaded(generation, view_generation, pool.postgres_databases().await)
        } else {
            Event::TablesLoaded(
                generation,
                view_generation,
                pool.list_tables(&profile).await,
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
    *ui.desired_database_session.borrow_mut() = Some(key.clone());
    ui.pending_database_opens.borrow_mut().insert(key.clone());
    let request = Uuid::new_v4().to_string();
    ui.connect_requests
        .borrow_mut()
        .insert(key, request.clone());
    ui.status.set_text(&format!("Открываю базу {database}…"));
    let sender = ui.sender.clone();
    ui.runtime.spawn(async move {
        let result = pool.connect_database(&profile.database).await;
        let _ = sender.send(Event::Connected(request, profile, result));
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
    for schema in schemas {
        let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
        children.set_margin_start(24);
        let expanded = ui
            .catalog_tree
            .borrow()
            .schema_expanded
            .get(&(session_key.clone(), schema.clone()))
            .copied()
            .unwrap_or(schema == "public" || preferred_schema.as_deref() == Some(schema.as_str()));
        children.set_visible(expanded);
        let disclosure = gtk::Label::new(Some(if expanded { "▾" } else { "▸" }));
        disclosure.set_width_chars(2);
        let database_group = !matches!(profile.kind, DatabaseKind::PostgreSql);
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
        schema_button.set_tooltip_text(Some(&format!("{}: {}", profile.database, schema)));
        let children_for_toggle = children.clone();
        let disclosure_for_toggle = disclosure.clone();
        let folder_for_toggle = folder.clone();
        let weak = Rc::downgrade(ui);
        let selected_schema_button = schema_button.downgrade();
        let schema_identity = (session_key.clone(), schema.clone());
        schema_button.connect_clicked(move |_| {
            let expanded = !children_for_toggle.is_visible();
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
            }
        });
        parent.append(&schema_button);
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
                        .get(&session_key)
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

fn handle_event(ui: &Rc<Ui>, event: Event) {
    match event {
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
            if ui.pending_database_opens.borrow_mut().remove(&session_key)
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
                    let loading = gtk::Label::new(Some("Загружаю таблицы…"));
                    loading.set_xalign(0.0);
                    loading.add_css_class("dim-label");
                    ui.objects.append(&loading);
                    show_sidebar(ui, "tables");
                    *ui.current_profile.borrow_mut() = Some(profile.clone());
                    *ui.pool.borrow_mut() = Some(pool.clone());
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
                    ensure_session_table_tabs(ui, &session_key);
                    add_session_tab(ui, &profile);
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
                    sql_history_ui::restore_drafts(ui, &profile);
                    show_table_catalog(ui);
                }
                Err(error) => {
                    ui.profile_status.borrow_mut().insert(
                        profile.id.clone(),
                        if is_profile_connected(ui, &profile.id) {
                            ProfileStatus::Connected
                        } else {
                            ProfileStatus::Error(error.clone())
                        },
                    );
                    refresh_profiles(ui);
                    if ui.sessions.borrow().is_empty() {
                        ui.active_header.set_visible(false);
                        ui.table_tabs.set_visible(false);
                        ui.landing.set_visible(true);
                        ui.active.set_text("Нет активного подключения");
                    } else {
                    }
                    ui.objects.set_sensitive(true);
                    ui.status.set_text(&error);
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
                    ui.table_filter.set_text("");
                    ui.table_filter.set_visible(true);
                    clear_box(&ui.objects);
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
                    ui.status.set_text(&error);
                    show_error_dialog(ui, "Не удалось загрузить таблицы", &error);
                }
            }
        }
        Event::TableLoaded(tab_key, request_id, name, result) => {
            if ui.table_requests.borrow().get(&tab_key) != Some(&request_id) {
                return;
            }
            if result.is_err() {
                if let Some(grid) = ui.tabulator_grids.borrow().get(&tab_key) {
                    grid.set_locked(false);
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
                    ui.status.set_text(&format!(
                        "{}. {} строк{}",
                        name,
                        data.rows.len(),
                        if details.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", details.join("; "))
                        }
                    ));
                }
                Err(error) => {
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
            let mut next = match result {
                Ok(next) => next,
                Err(error) => {
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
            if current.rows.len() != offset || current.columns != next.columns {
                return;
            }
            current.rows.append(&mut next.rows);
            let limited = current.trim_preview(database::MAX_BROWSE_ROWS)
                || (next.truncated && current.rows.len() >= database::MAX_BROWSE_ROWS);
            let added = current.rows.len() - offset;
            current.truncated = next.truncated && !limited;
            if let Some(tabulator) = ui.tabulator_grids.borrow().get(&table_key) {
                tabulator.append_data(
                    offset,
                    &grid_rows_from_rows(&current.rows[offset..]),
                    current.truncated,
                    &grid_blocked_cells(&current),
                );
            }
            ui.status.set_text(&if limited {
                "Достигнут лимит предпросмотра (10 000 строк / 32 МиБ). Уточните фильтр или экспортируйте всю таблицу.".into()
            } else {format!("Загружено строк: {}",current.rows.len())});
            if added == 0 {
                if let Some(tabulator) = ui.tabulator_grids.borrow().get(&table_key) {
                    tabulator.append_data(offset, &[], false, &[]);
                }
            }
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
        Event::SqlCompletionsLoaded(id, result) => {
            let tabs = ui.sql_tabs.borrow();
            let Some(tab) = tabs.get(&id) else {
                return;
            };
            match result {
                Ok(words) => tab.completion_provider.set_catalog(&words),
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
        Event::SqlQueryFinished(id, request_id, result, state, elapsed_ms) => {
            let Some(mut tab) = ui.sql_tabs.borrow_mut().remove(&id) else {
                return;
            };
            if tab.request_id != request_id {
                ui.sql_tabs.borrow_mut().insert(id, tab);
                return;
            }
            let is_active = ui
                .current_profile
                .borrow()
                .as_ref()
                .is_some_and(|profile| profile_session_key(profile) == tab.session_key)
                && ui
                    .active_table_by_session
                    .borrow()
                    .get(&tab.session_key)
                    .is_some_and(|key| key == &format!("sql::{id}"));
            tab.run_button.set_sensitive(true);
            tab.stop_button.set_sensitive(false);
            update_transaction_controls(&mut tab, state);
            sql_history_ui::record(ui, &tab, &result, elapsed_ms);
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
                    let editable = !state.manual
                        && !state.pending
                        && can_edit_table_cells(ui, &key, &result.data);
                    if let Some(state) = ui.table_edit_states.borrow().get(&key) {
                        state.grid_editing.set(false);
                        state.cell_edit_allowed.set(editable);
                        update_table_edit_controls(state, None);
                    }
                    tab.tabulator.set_data(
                        &result.data.columns,
                        &grid_rows(&result.data),
                        editable,
                        false,
                        false,
                        &result.read_only_columns,
                        &grid_blocked_cells(&result.data),
                    );
                    tab.export_button
                        .set_sensitive(!result.data.columns.is_empty());
                    let has_rows = !result.data.rows.is_empty();
                    tab.copy_button.set_sensitive(has_rows);
                    tab.result_stack.set_visible_child_name(if has_rows {
                        "data"
                    } else {
                        "empty"
                    });
                    let summary = format!("{} · {elapsed_ms} мс", query_result_summary(&result));
                    tab.status
                        .set_text(&if !result.data.columns.is_empty() && !editable {
                            format!("{summary} · только чтение")
                        } else {
                            summary
                        });
                    tab.status.set_tooltip_text((!editable).then_some("Для редактирования нужен SELECT прямых столбцов одной таблицы, полный первичный ключ и профиль с правом записи. JOIN, вычисления и агрегаты доступны для чтения."));
                    tab.error_message.set_text("");
                    tab.error_message.set_visible(false);
                    tab.result = Some(result.data);
                    if is_active {
                        ui.status
                            .set_text(&format!("SQL-запрос: {} строк", result.row_count));
                    }
                }
                Err(error) => {
                    tab.tabulator.set_locked(false);
                    tab.status.set_text("Запрос завершился с ошибкой");
                    tab.error_message
                        .set_text(&format!("Ошибка запроса: {error}"));
                    tab.error_message.set_visible(true);
                    if is_active {
                        ui.status.set_text("Ошибка выполнения SQL-запроса");
                    }
                }
            }
            ui.sql_tabs.borrow_mut().insert(id, tab);
        }
        Event::CsvImportProgress(rows) => ui.status.set_text(&format!(
            "CSV: обработано {rows} строк; транзакция ещё не сохранена…"
        )),
        Event::CsvImported(profile, object, result) => {
            finish_transfer(ui);
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
            finish_transfer(ui);
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
        Event::DumpProgress(bytes) => ui
            .status
            .set_text(&format!("Бэкап: {:.1} МиБ…", bytes as f64 / 1048576.0)),
        Event::DumpFinished(path, kind, result) => {
            finish_transfer(ui);
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
        Event::ExportProgress(count) => ui.status.set_text(&format!("Экспорт CSV: {count} строк…")),
        Event::CsvExported(rows, result) => {
            finish_transfer(ui);
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
    data.rows
        .iter()
        .enumerate()
        .flat_map(|(row, cells)| {
            cells.iter().enumerate().filter_map(move |(column, value)| {
                matches!(value, CellValue::Truncated(_) | CellValue::DecodeError(_))
                    .then_some((row, column))
            })
        })
        .collect()
}

fn handle_tabulator_message(ui: &Rc<Ui>, table_key: &str, message: &str) {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(message) else {
        return;
    };
    match message.get("type").and_then(serde_json::Value::as_str) {
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
            if !ui
                .table_more_loading
                .borrow_mut()
                .insert(table_key.to_owned())
            {
                return;
            }
            let (Some(profile), Some(object), Some(view_state), Some(current)) = (
                ui.table_profiles.borrow().get(table_key).cloned(),
                ui.table_objects.borrow().get(table_key).cloned(),
                ui.table_view_states.borrow().get(table_key).cloned(),
                ui.table_data
                    .borrow()
                    .get(table_key)
                    .map(|data| (data.rows.len(), data.truncated)),
            ) else {
                ui.table_more_loading.borrow_mut().remove(table_key);
                return;
            };
            if !current.1 || current.0 >= database::MAX_BROWSE_ROWS {
                if current.0 >= database::MAX_BROWSE_ROWS {
                    ui.status.set_text("Показаны первые 10 000 строк. Уточните фильтр или экспортируйте всю таблицу потоком.");
                }
                ui.table_more_loading.borrow_mut().remove(table_key);
                return;
            }
            let Some((_, pool)) = ui
                .sessions
                .borrow()
                .get(&profile_session_key(&profile))
                .cloned()
            else {
                ui.table_more_loading.borrow_mut().remove(table_key);
                return;
            };
            let offset = current.0;
            let key = table_key.to_owned();
            let Some(request_id) = ui.table_requests.borrow().get(table_key).cloned() else {
                ui.table_more_loading.borrow_mut().remove(table_key);
                return;
            };
            let sender = ui.sender.clone();
            ui.runtime.spawn(async move {
                let result = pool
                    .read_table_view_with_condition(
                        &object,
                        &view_state.filters,
                        view_state.raw_condition.as_deref(),
                        view_state.sort.as_ref(),
                        offset,
                    )
                    .await;
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
    ui.active_header.set_visible(true);
    let name = format!("{}.{}", object.schema, object.name);
    let table_name = object.name.clone();
    let session_key = profile_session_key(&profile);
    let tab_key = format!("{}:{name}", session_key);
    let notebook = ensure_session_table_tabs(ui, &session_key);
    if let Some((_, page, _)) = find_table_page(ui, &tab_key) {
        ui.active_table_by_session
            .borrow_mut()
            .insert(session_key.clone(), tab_key);
        ui.table_tabs.set_visible_child_name(&session_key);
        notebook.set_current_page(Some(page));
        ui.table_tabs.set_visible(true);
        ui.empty_session.set_visible(false);
        return;
    }
    let mut replace_page = None;
    if !open_in_new_tab {
        let previous_key = ui
            .active_table_by_session
            .borrow()
            .get(&session_key)
            .cloned();
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
    let tabulator = TabulatorGrid::new(move |message| {
        if let Some(ui) = weak.upgrade() {
            handle_tabulator_message(&ui, &tabulator_key, &message);
        }
    });
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
    footer.append(&filter_panel.1.sql_mode);
    let footer_space = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    footer_space.set_hexpand(true);
    footer_space.append(&edit_toolbar);
    footer.append(&footer_space);
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
    tab_label_box.append(&close_tab);
    let tab_index = if let Some(position) = replace_page {
        notebook.insert_page(&page_content, Some(&tab_label_box), Some(position))
    } else {
        notebook.append_page(&page_content, Some(&tab_label_box))
    };
    ui.active_table_by_session
        .borrow_mut()
        .insert(session_key.clone(), tab_key.clone());
    ui.table_tabs.set_visible_child_name(&session_key);
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
    runtime.spawn(async move {
        let result = pool.read_table(&object).await;
        let _ = sender.send(Event::TableLoaded(
            tab_key,
            request_id_for_event,
            name,
            result,
        ));
    });
}

fn create_table_filter_panel(ui: &Rc<Ui>, table_key: &str) -> (gtk::Box, Rc<TableFilterPanel>) {
    let panel = Rc::new(TableFilterPanel {
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
    actions.append(&panel.clear);
    toolbar.append(&actions);
    container.append(&toolbar);
    let weak_panel = Rc::downgrade(&panel);
    panel.sql_mode.connect_toggled(move |button| {
        if let Some(panel) = weak_panel.upgrade() {
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
        save_button: gtk::Button::with_label("Сохранить"),
        discard_button: gtk::Button::with_label("Отменить"),
        bulk_button: gtk::Button::with_label("Заменить столбец…"),
        status: gtk::Label::new(Some(
            "Клик — выделить; двойной клик — редактировать; Ctrl+C/V — буфер",
        )),
    });
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
    edit_toolbar.append(&table_edit_state.status);
    edit_toolbar.append(&table_edit_state.discard_button);
    edit_toolbar.append(&table_edit_state.save_button);
    table_edit_state
}

fn update_table_edit_controls(state: &TableEditState, message: Option<&str>) {
    let cell_count = state.pending.borrow().len();
    let bulk_pending = state.bulk_column.borrow().is_some();
    let dirty = cell_count > 0 || bulk_pending || state.grid_editing.get();
    state.toolbar.set_visible(dirty || state.saving.get());
    let can_save = dirty && !state.saving.get();
    state.save_button.set_sensitive(can_save);
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
    runtime.spawn(async move {
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

fn save_table_edits(ui: &Rc<Ui>, table_key: &str) {
    save_table_edits_authorized(ui, table_key, false);
}
fn save_table_edits_authorized(ui: &Rc<Ui>, table_key: &str, approved: bool) {
    let Some(state) = ui.table_edit_states.borrow().get(table_key).cloned() else {
        return;
    };
    if state.saving.get() {
        return;
    }
    if state.editing_cell.get().is_some() {
        finish_inline_cell_edit(ui, table_key);
    }
    if state.editing_column.get().is_some() {
        finish_inline_column_edit(ui, table_key);
    }
    let Some(object) = ui.table_objects.borrow().get(table_key).cloned() else {
        return;
    };
    let Some(data) = ui.table_data.borrow().get(table_key).cloned() else {
        return;
    };
    let profile_read_only = ui
        .table_profiles
        .borrow()
        .get(table_key)
        .is_none_or(|profile| profile.read_only);
    if profile_read_only {
        state
            .status
            .set_text("Профиль подключения работает в режиме только чтение");
        return;
    }
    let session_key = ui
        .table_profiles
        .borrow()
        .get(table_key)
        .map(profile_session_key);
    let Some(pool) =
        session_key.and_then(|key| ui.sessions.borrow().get(&key).map(|(_, pool)| pool.clone()))
    else {
        state.status.set_text("Нет активного подключения к БД");
        return;
    };
    let bulk_column = state.bulk_column.borrow().clone();
    let cell_changes = state.pending.borrow().clone();
    let Some(profile) = ui.table_profiles.borrow().get(table_key).cloned() else {
        return;
    };
    if matches!(profile.environment, EnvironmentTag::Prod)
        && !approved
        && (bulk_column.is_some() || !cell_changes.is_empty())
    {
        let owner = Rc::downgrade(ui);
        let key = table_key.to_owned();
        let snapshot = data.clone();
        let pending = cell_changes.clone();
        let bulk = bulk_column.clone();
        let details = if let Some((column, value)) = &bulk {
            format!(
                "Массовая замена столбца {} в {}.{}\nНовое значение: {:?}\nБудут изменены ВСЕ строки таблицы, а не только видимые или отфильтрованные.",
                data.columns.get(*column).map(String::as_str).unwrap_or("?"),
                object.schema,
                object.name,
                value
            )
        } else {
            format!(
                "{}.{}: изменено {} ячеек.\n{:?}",
                object.schema,
                object.name,
                pending.len(),
                pending
            )
        };
        production_ui::confirm(
            ui,
            &profile,
            "Сохранить изменения таблицы PROD",
            &details,
            move || {
                if let Some(ui) = owner.upgrade() {
                    let unchanged = ui
                        .table_data
                        .borrow()
                        .get(&key)
                        .is_some_and(|data| Arc::ptr_eq(data, &snapshot))
                        && ui
                            .table_edit_states
                            .borrow()
                            .get(&key)
                            .is_some_and(|state| {
                                *state.pending.borrow() == pending
                                    && *state.bulk_column.borrow() == bulk
                            });
                    if unchanged {
                        save_table_edits_authorized(&ui, &key, true);
                    } else {
                        ui.status.set_text(
                            "Правки изменились после подтверждения. Подтвердите их заново.",
                        );
                    }
                }
            },
        );
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
        runtime.spawn(async move {
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
    let sql_session = table_key
        .strip_prefix("sql::")
        .and_then(|id| ui.sql_tabs.borrow().get(id).map(|tab| tab.session.clone()));
    let sender = ui.sender.clone();
    let table_key = table_key.to_owned();
    let runtime = ui.runtime.clone();
    runtime.spawn(async move {
        let result = match sql_session {
            Some(session) => {
                if approved {
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
    if let Some(state) = ui.table_edit_states.borrow().get(table_key)
        && (state.saving.get()
            || state.grid_editing.get()
            || !state.pending.borrow().is_empty()
            || state.bulk_column.borrow().is_some())
    {
        state
            .status
            .set_text("Сначала сохраните или отмените изменения перед обновлением таблицы");
        return;
    }
    let profile = ui.table_profiles.borrow().get(table_key).cloned();
    let object = ui.table_objects.borrow().get(table_key).cloned();
    let (Some(profile), Some(object)) = (profile, object) else {
        return;
    };
    let session_key = profile_session_key(&profile);
    let pool = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(_, pool)| pool.clone());
    let Some(pool) = pool else {
        return;
    };
    let state = ui
        .table_view_states
        .borrow()
        .get(table_key)
        .cloned()
        .unwrap_or_default();
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
    let name = format!("{}.{}", object.schema, object.name);
    runtime.spawn(async move {
        let result = pool
            .read_table_view_with_condition(
                &object,
                &state.filters,
                state.raw_condition.as_deref(),
                state.sort.as_ref(),
                0,
            )
            .await;
        let _ = sender.send(Event::TableLoaded(table_key, request_id, name, result));
    });
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
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
