//! One tab strip in the header bar over the per-group notebooks, plus the
//! connection block of the status bar. The notebooks keep owning pages and all
//! selection logic; the strip only mirrors them and forwards clicks.
use super::*;

pub(super) struct TabStrip {
    pub root: gtk::Box,
    pub(super) scroller: gtk::ScrolledWindow,
    pub(super) row: gtk::Box,
    pub(super) add_button: gtk::Button,
    owner: RefCell<std::rc::Weak<Ui>>,
    pending: Cell<bool>,
}

struct Item {
    group: String,
    key: Option<String>,
    icon: &'static str,
    text: String,
    subtitle: String,
    tooltip: String,
    color: String,
    active: bool,
    broken: bool,
}

impl TabStrip {
    pub(super) fn new(app_title: &str) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&row)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .build();
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.add_css_class("flat");
        add_button.add_css_class("tab-strip-add");
        add_button.set_valign(gtk::Align::Center);
        add_button.set_tooltip_text(Some("Новая SQL-вкладка"));
        add_button.set_sensitive(false);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        root.add_css_class("tab-strip");
        root.set_hexpand(true);
        root.set_tooltip_text(Some(app_title));
        root.append(&scroller);
        root.append(&add_button);
        Self {
            root,
            scroller,
            row,
            add_button,
            owner: RefCell::new(std::rc::Weak::new()),
            pending: Cell::new(false),
        }
    }
}

/// Connects the strip to the group notebook and the "+" button.
pub(super) fn bind(ui: &Rc<Ui>) {
    *ui.tab_strip.owner.borrow_mut() = Rc::downgrade(ui);
    watch_notebook(ui, &ui.session_tabs);
    let weak = Rc::downgrade(ui);
    ui.tab_strip.add_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            new_sql_tab(&ui);
        }
    });
}

/// Rebuilds the strip whenever pages of `notebook` change.
pub(super) fn watch_notebook(ui: &Rc<Ui>, notebook: &gtk::Notebook) {
    let weak = Rc::downgrade(ui);
    let notify = move || {
        if let Some(ui) = weak.upgrade() {
            refresh(&ui);
        }
    };
    let f = notify.clone();
    notebook.connect_page_added(move |_, _, _| f());
    let f = notify.clone();
    notebook.connect_page_removed(move |_, _, _| f());
    let f = notify.clone();
    notebook.connect_page_reordered(move |_, _, _| f());
    notebook.connect_switch_page(move |_, _, _| notify());
}

/// Schedules a rebuild; bursts of signals and calls from inside widget
/// handlers collapse into one idle pass, after notebook state has settled.
pub(super) fn refresh(ui: &Ui) {
    if ui.tab_strip.pending.replace(true) {
        return;
    }
    let weak = ui.tab_strip.owner.borrow().clone();
    gtk::glib::idle_add_local_once(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.tab_strip.pending.set(false);
        rebuild(&ui);
    });
}

fn rebuild(ui: &Rc<Ui>) {
    let strip = &ui.tab_strip;
    while let Some(child) = strip.row.first_child() {
        strip.row.remove(&child);
    }
    let mut active_widget = None;
    for item in collect(ui) {
        let widget = build(ui, &item);
        if item.active {
            active_widget = Some(widget.clone());
        }
        strip.row.append(&widget);
    }
    strip
        .add_button
        .set_sensitive(ui.current_profile.borrow().is_some() || ui.session_tabs.n_pages() > 0);
    if let Some(widget) = active_widget {
        let scroller = strip.scroller.clone();
        let row = strip.row.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(40), move || {
            let Some(bounds) = widget.compute_bounds(&row) else {
                return;
            };
            let adjustment = scroller.hadjustment();
            let (x, width) = (f64::from(bounds.x()), f64::from(bounds.width()));
            if x < adjustment.value() {
                adjustment.set_value(x);
            } else if x + width > adjustment.value() + adjustment.page_size() {
                adjustment.set_value(x + width - adjustment.page_size());
            }
        });
    }
}

fn collect(ui: &Rc<Ui>) -> Vec<Item> {
    let current = ui.session_tabs.current_page();
    let mut items = Vec::new();
    for index in 0..ui.session_tabs.n_pages() {
        let Some(group) = ui
            .session_tabs
            .nth_page(Some(index))
            .and_then(|page| page.tooltip_text())
            .map(|key| key.to_string())
        else {
            continue;
        };
        let Some(schema_group) = ui.workspace_groups.borrow().get(&group).cloned() else {
            continue;
        };
        let Some(profile) = ui
            .sessions
            .borrow()
            .get(&schema_group.session_key)
            .map(|(profile, _)| profile.clone())
        else {
            continue;
        };
        let group_active = current == Some(index);
        let mut tabs = Vec::new();
        let notebook = ui.session_table_tabs.borrow().get(&group).cloned();
        if let Some(notebook) = notebook {
            for page_index in 0..notebook.n_pages() {
                let Some(key) = notebook
                    .nth_page(Some(page_index))
                    .and_then(|page| table_key_from_page(&notebook, &page))
                else {
                    continue;
                };
                let active = group_active && notebook.current_page() == Some(page_index);
                if let Some(item) = page_item(ui, &group, &profile, &key, active) {
                    tabs.push(item);
                }
            }
        }
        append_group(&mut items, tabs, || {
            let database = match &schema_group.schema {
                Some(schema) => format!("{}.{schema}", profile.database),
                None => profile.database.clone(),
            };
            Item {
                group,
                key: None,
                icon: "database",
                tooltip: tooltip(&profile, &database),
                subtitle: profile.name.clone(),
                text: database,
                color: profile.color.clone(),
                active: group_active,
                broken: ui
                    .broken_sessions
                    .borrow()
                    .contains(&profile_session_key(&profile)),
            }
        });
    }
    items
}

/// Keeps group order and per-group tab order; a group without tabs is shown
/// as one placeholder item so it stays reachable and closable.
fn append_group(items: &mut Vec<Item>, tabs: Vec<Item>, placeholder: impl FnOnce() -> Item) {
    if tabs.is_empty() {
        items.push(placeholder());
    } else {
        items.extend(tabs);
    }
}

fn tooltip(profile: &ConnectionProfile, database: &str) -> String {
    format!(
        "{} · {}:{} · {}",
        profile.name, profile.host, profile.port, database
    )
}

fn page_item(
    ui: &Ui,
    group: &str,
    group_profile: &ConnectionProfile,
    key: &str,
    active: bool,
) -> Option<Item> {
    let (icon, text, profile, session_key) = if let Some(id) = key.strip_prefix("sql::") {
        let tabs = ui.sql_tabs.borrow();
        let tab = tabs.get(id)?;
        (
            "sql-query",
            tab.title.text().to_string(),
            group_profile.clone(),
            tab.session_key.clone(),
        )
    } else {
        let object = ui.table_objects.borrow().get(key).cloned()?;
        let profile = ui
            .table_profiles
            .borrow()
            .get(key)
            .cloned()
            .unwrap_or_else(|| group_profile.clone());
        let text = if object.schema.trim().is_empty() {
            object.name.clone()
        } else {
            format!("{}.{}", object.schema, object.name)
        };
        let session_key = profile_session_key(&profile);
        ("entity-table", text, profile, session_key)
    };
    let database = if profile.database.trim().is_empty() {
        profile.host.clone()
    } else {
        profile.database.clone()
    };
    Some(Item {
        group: group.to_owned(),
        key: Some(key.to_owned()),
        icon,
        tooltip: tooltip(&profile, &database),
        subtitle: format!("{} / {database}", profile.name),
        text,
        color: profile.color.clone(),
        active,
        broken: ui.broken_sessions.borrow().contains(&session_key),
    })
}

fn build(ui: &Rc<Ui>, item: &Item) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("tab-strip-item");
    if item.active {
        root.add_css_class("tab-strip-active");
    }
    let mut tooltip = item.tooltip.clone();
    if item.broken {
        root.add_css_class("tab-strip-broken");
        tooltip.push_str("\nСоединение потеряно");
    }
    root.set_tooltip_text(Some(&tooltip));

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let dot = color_dot(&item.color);
    set_profile_accent(&dot, &item.color);
    content.append(&dot);
    content.append(&profile_tab_icon(item.icon, &item.color));
    if item.broken {
        content.append(&gtk::Image::from_icon_name("network-offline-symbolic"));
    }
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
    labels.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(Some(&item.text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(26);
    label.set_xalign(0.0);
    labels.append(&label);
    let subtitle = gtk::Label::new(Some(&item.subtitle));
    subtitle.add_css_class("tab-strip-subtitle");
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.set_max_width_chars(26);
    subtitle.set_xalign(0.0);
    labels.append(&subtitle);
    content.append(&labels);
    let main = gtk::Button::new();
    main.set_child(Some(&content));
    main.add_css_class("flat");
    main.add_css_class("tab-strip-main");
    let weak = Rc::downgrade(ui);
    let group = item.group.clone();
    let key = item.key.clone();
    main.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            activate(&ui, &group, key.as_deref());
        }
    });

    let close = gtk::Button::from_icon_name("window-close-symbolic");
    set_button_icon_size(&close, 14);
    close.add_css_class("flat");
    close.add_css_class("circular");
    close.add_css_class("compact-tab-close");
    close.set_valign(gtk::Align::Center);
    close.set_tooltip_text(Some(if item.key.is_some() {
        "Закрыть вкладку"
    } else {
        "Закрыть группу вкладок"
    }));
    let weak = Rc::downgrade(ui);
    let group = item.group.clone();
    let key = item.key.clone();
    close.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        match key.as_deref() {
            Some(key) => match key.strip_prefix("sql::") {
                Some(id) => close_sql_tab(&ui, id),
                None => close_table_tab(&ui, key),
            },
            None => workspace_ui::close(&ui, &group),
        }
    });
    root.append(&main);
    root.append(&close);
    if let Some(key) = &item.key {
        install_menu(ui, &main, &item.group, key);
    }
    root
}

#[derive(Clone, Copy)]
enum TabAction {
    MoveLeft,
    MoveRight,
    CloseOthers,
    CloseRight,
}

fn group_keys(ui: &Ui, group: &str) -> Vec<String> {
    let Some(notebook) = ui.session_table_tabs.borrow().get(group).cloned() else {
        return Vec::new();
    };
    (0..notebook.n_pages())
        .filter_map(|index| {
            let page = notebook.nth_page(Some(index))?;
            table_key_from_page(&notebook, &page)
        })
        .collect()
}

// Batch operations never open a stack of discard dialogs or discard edits.
// Individual close functions remain responsible for SQL draft persistence.
fn protected(ui: &Ui, key: &str) -> bool {
    if ui.table_edit_states.borrow().get(key).is_some_and(|state| {
        state.saving.get()
            || state.grid_editing.get()
            || !state.pending.borrow().is_empty()
            || state.bulk_column.borrow().is_some()
    }) {
        return true;
    }
    key.strip_prefix("sql::").is_some_and(|id| {
        ui.sql_tabs.borrow().get(id).is_some_and(|tab| {
            !tab.run_button.is_sensitive() || tab.session.has_pending_transaction()
        })
    })
}

fn perform(ui: &Rc<Ui>, group: &str, key: &str, action: TabAction) {
    let keys = group_keys(ui, group);
    let Some(index) = keys.iter().position(|candidate| candidate == key) else {
        return; // A menu may outlive its page.
    };
    match action {
        TabAction::MoveLeft | TabAction::MoveRight => {
            let target = match action {
                TabAction::MoveLeft => index.checked_sub(1),
                _ => (index + 1 < keys.len()).then_some(index + 1),
            };
            let Some(target) = target else { return };
            let notebook = ui.session_table_tabs.borrow().get(group).cloned();
            if let Some(notebook) = notebook
                && let Some(page) = notebook.nth_page(Some(index as u32))
            {
                notebook.reorder_child(&page, Some(target as u32));
            }
        }
        TabAction::CloseOthers | TabAction::CloseRight => {
            let mut skipped = 0;
            // Snapshot keys: removing a page changes notebook indices.
            for candidate in keys
                .iter()
                .enumerate()
                .filter_map(|(at, candidate)| match action {
                    TabAction::CloseOthers => (at != index).then_some(candidate),
                    _ => (at > index).then_some(candidate),
                })
            {
                if protected(ui, candidate) {
                    skipped += 1;
                    continue;
                }
                if let Some(id) = candidate.strip_prefix("sql::") {
                    close_sql_tab(ui, id);
                } else {
                    close_table_tab(ui, candidate);
                }
            }
            if skipped > 0 {
                ui.status.set_text(&format!(
                    "Оставлено вкладок: {skipped} — есть правки, сохранение, запрос или транзакция"
                ));
            }
        }
    }
    refresh(ui);
}

fn menu(ui: &Rc<Ui>, group: &str, key: &str) -> gtk::Popover {
    let popover = gtk::Popover::new();
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let keys = group_keys(ui, group);
    let index = keys.iter().position(|candidate| candidate == key);
    for (label, icon, action, enabled) in [
        (
            "Переместить влево",
            "go-previous-symbolic",
            TabAction::MoveLeft,
            index.is_some_and(|index| index > 0),
        ),
        (
            "Переместить вправо",
            "go-next-symbolic",
            TabAction::MoveRight,
            index.is_some_and(|index| index + 1 < keys.len()),
        ),
        (
            "Закрыть остальные в группе",
            "window-close-symbolic",
            TabAction::CloseOthers,
            index.is_some() && keys.len() > 1,
        ),
        (
            "Закрыть справа в группе",
            "window-close-symbolic",
            TabAction::CloseRight,
            index.is_some_and(|index| index + 1 < keys.len()),
        ),
    ] {
        let button = sql_menu_action(&actions, &popover, label, icon);
        button.set_sensitive(enabled);
        let weak = Rc::downgrade(ui);
        let group = group.to_owned();
        let key = key.to_owned();
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                perform(&ui, &group, &key, action);
            }
        });
    }
    popover.set_child(Some(&actions));
    popover
}

fn install_menu(ui: &Rc<Ui>, anchor: &gtk::Button, group: &str, key: &str) {
    let open_menu = Rc::new(RefCell::new(None::<gtk::Popover>));
    let current = open_menu.clone();
    anchor.connect_unmap(move |_| {
        // Rebuild removes tab buttons even when their menu is still open.
        // GtkButton must never be finalized with a Popover child attached.
        if let Some(popover) = current.borrow_mut().take() {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    });
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    let weak_anchor = anchor.downgrade();
    let group = group.to_owned();
    let key = key.to_owned();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let (Some(ui), Some(anchor)) = (weak.upgrade(), weak_anchor.upgrade()) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(previous) = open_menu.borrow_mut().take() {
            previous.popdown();
        }
        let popover = menu(&ui, &group, &key);
        popover.set_parent(&anchor);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| popover.unparent());
        *open_menu.borrow_mut() = Some(popover.clone());
        popover.popup();
    });
    anchor.add_controller(gesture);
}

/// Group first (runs select_session), then the page, so the notebook handler
/// applies the usual per-table side effects.
fn activate(ui: &Rc<Ui>, group: &str, key: Option<&str>) {
    if ui.tab_switching.get() {
        return;
    }
    if let Some(index) = session_page_index(ui, group)
        && ui.session_tabs.current_page() != Some(index)
    {
        ui.session_tabs.set_current_page(Some(index));
    }
    let Some(key) = key else { return };
    let notebook = ui.session_table_tabs.borrow().get(group).cloned();
    let Some(notebook) = notebook else { return };
    for index in 0..notebook.n_pages() {
        let Some(page) = notebook.nth_page(Some(index)) else {
            continue;
        };
        if table_key_from_page(&notebook, &page).as_deref() == Some(key) {
            if notebook.current_page() != Some(index) {
                notebook.set_current_page(Some(index));
            }
            break;
        }
    }
    refresh(ui);
}

fn new_sql_tab(ui: &Rc<Ui>) {
    let group = ui
        .session_tabs
        .current_page()
        .and_then(|index| ui.session_tabs.nth_page(Some(index)))
        .and_then(|page| page.tooltip_text())
        .map(|key| key.to_string())
        .or_else(|| active_workspace_group(ui));
    if let Some(group) = group {
        create_sql_tab_in_group(ui, &group);
    } else if let Some(profile) = ui.current_profile.borrow().clone() {
        create_sql_tab(ui, &profile_session_key(&profile));
    }
}

/// Only a strict `#RRGGBB` reaches the drawing code; anything else gets the
/// default blue, so a profile string never becomes CSS or a parser input.
fn dot_rgb(color: &str) -> (f64, f64, f64) {
    use crate::noctalia::Rgb;
    let rgb = Rgb::parse(color).unwrap_or(Rgb::new(0x4f, 0x8f, 0xe8));
    // Rgb keeps its channels private; Display is always `#rrggbb`.
    let hex = rgb.to_string();
    let channel =
        |at: usize| f64::from(u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0)) / 255.0;
    (channel(1), channel(3), channel(5))
}

fn color_dot(color: &str) -> gtk::DrawingArea {
    let (red, green, blue) = dot_rgb(color);
    let dot = gtk::DrawingArea::builder()
        .content_width(9)
        .content_height(9)
        .valign(gtk::Align::Center)
        .build();
    dot.add_css_class("tab-strip-dot");
    dot.set_draw_func(move |_, cr, width, height| {
        let (width, height) = (f64::from(width), f64::from(height));
        cr.set_source_rgb(red, green, blue);
        cr.arc(
            width / 2.0,
            height / 2.0,
            width.min(height) / 2.0,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    });
    dot
}

/// Right side of the status bar: which connection the visible tab uses.
pub(super) struct ConnectionBlock {
    pub root: gtk::Box,
    dot: RefCell<Option<gtk::DrawingArea>>,
    name: gtk::Label,
    endpoint: gtk::Label,
    environment: gtk::Label,
    read_only: gtk::Label,
    tls: gtk::Label,
}

impl ConnectionBlock {
    pub(super) fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("status-connection");
        root.set_visible(false);
        let name = gtk::Label::new(None);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(28);
        root.append(&name);
        let endpoint = gtk::Label::new(None);
        endpoint.add_css_class("dim-label");
        endpoint.set_ellipsize(gtk::pango::EllipsizeMode::End);
        endpoint.set_max_width_chars(32);
        root.append(&endpoint);
        let badge = |text: &str| {
            let label = gtk::Label::new(Some(text));
            label.add_css_class("status-badge");
            root.append(&label);
            label
        };
        let environment = badge("");
        let read_only = badge("RO");
        read_only.set_tooltip_text(Some("Только чтение"));
        let tls = badge("");
        Self {
            root,
            dot: RefCell::new(None),
            name,
            endpoint,
            environment,
            read_only,
            tls,
        }
    }

    pub(super) fn set(&self, profile: Option<&ConnectionProfile>) {
        let Some(profile) = profile else {
            self.root.set_visible(false);
            return;
        };
        if let Some(old) = self.dot.borrow_mut().take() {
            self.root.remove(&old);
        }
        let dot = color_dot(&profile.color);
        self.root.prepend(&dot);
        *self.dot.borrow_mut() = Some(dot);
        self.name.set_text(&profile.name);
        self.endpoint
            .set_text(&format!("{}:{}", profile.host, profile.port));
        self.environment.set_text(profile.environment.label());
        if matches!(profile.environment, EnvironmentTag::Prod) {
            self.environment.add_css_class("status-badge-prod");
        } else {
            self.environment.remove_css_class("status-badge-prod");
        }
        self.read_only.set_visible(profile.read_only);
        let (tls_text, tls_warn) = match profile.tls_mode {
            TlsMode::VerifyFull => ("TLS", false),
            TlsMode::Require => ("TLS?", true),
            TlsMode::Disabled => ("БЕЗ TLS", true),
        };
        self.tls.set_text(tls_text);
        self.tls.set_tooltip_text(Some(transport_summary(profile)));
        if tls_warn {
            self.tls.add_css_class("status-badge-warn");
        } else {
            self.tls.remove_css_class("status-badge-warn");
        }
        let database = if profile.database.is_empty() {
            "все доступные базы"
        } else {
            profile.database.as_str()
        };
        self.root.set_tooltip_text(Some(&format!(
            "{} · {}:{} · {}",
            profile.kind.label(),
            profile.host,
            profile.port,
            database
        )));
        self.root.set_visible(true);
    }
}

/// Shows the block for whichever connection is current, or hides it.
pub(super) fn sync_connection(ui: &Ui) {
    ui.connection_block
        .set(ui.current_profile.borrow().as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(group: &str, key: Option<&str>) -> Item {
        Item {
            group: group.to_owned(),
            key: key.map(str::to_owned),
            icon: "database",
            text: String::new(),
            subtitle: String::new(),
            tooltip: String::new(),
            color: String::new(),
            active: false,
            broken: false,
        }
    }

    fn ids(items: &[Item]) -> Vec<(&str, Option<&str>)> {
        items
            .iter()
            .map(|item| (item.group.as_str(), item.key.as_deref()))
            .collect()
    }

    const DEFAULT: (f64, f64, f64) = (
        0x4f as f64 / 255.0,
        0x8f as f64 / 255.0,
        0xe8 as f64 / 255.0,
    );

    #[test]
    fn dot_rgb_converts_valid_color() {
        assert_eq!(dot_rgb("#ff0080"), (1.0, 0.0, 128.0 / 255.0));
    }

    #[test]
    fn dot_rgb_falls_back_to_default_blue_on_invalid_input() {
        for bad in ["", "ff0080", "#12345", "#1234567", "#zzzzzz", "#12345g"] {
            assert_eq!(dot_rgb(bad), DEFAULT, "input {bad:?}");
        }
    }

    #[test]
    fn append_group_keeps_group_and_tab_order() {
        let mut items = Vec::new();
        append_group(
            &mut items,
            vec![item("a", Some("t1")), item("a", Some("t2"))],
            || item("a", None),
        );
        append_group(&mut items, vec![item("b", Some("t3"))], || item("b", None));
        assert_eq!(
            ids(&items),
            [("a", Some("t1")), ("a", Some("t2")), ("b", Some("t3"))]
        );
    }

    #[test]
    fn append_group_adds_placeholder_only_for_empty_group() {
        let mut items = Vec::new();
        append_group(&mut items, vec![item("a", Some("t1"))], || item("a", None));
        append_group(&mut items, Vec::new(), || item("b", None));
        assert_eq!(ids(&items), [("a", Some("t1")), ("b", None)]);
    }

    #[test]
    #[ignore = "requires headless GTK; synthetic tabs, no database servers"]
    fn tab_management_preserves_edits_and_group_boundaries() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        );
        let app = adw::Application::builder()
            .application_id("io.github.dbdeck.tab-management-test")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = build_window(&app, runtime);
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id":"tabs", "name":"Synthetic", "kind":"postgre_sql",
            "host":"127.0.0.1", "port":5432, "database":"sales", "username":"test",
            "environment":"local", "color":"#94e2d5", "read_only":true
        }))
        .unwrap();
        let group = workspace_ui::key(&profile, Some("public"));
        let other_group = workspace_ui::key(&profile, Some("archive"));
        let notebook = gtk::Notebook::new();
        let other = gtk::Notebook::new();
        ui.table_tabs.add_named(&notebook, Some(&group));
        ui.table_tabs.add_named(&other, Some(&other_group));
        ui.session_table_tabs
            .borrow_mut()
            .insert(group.clone(), notebook.clone());
        ui.session_table_tabs
            .borrow_mut()
            .insert(other_group.clone(), other.clone());
        watch_notebook(&ui, &notebook);
        let add = |book: &gtk::Notebook, schema: &str, name: &str| {
            let key = format!("{schema}.{name}");
            let label = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            label.set_tooltip_text(Some(&key));
            book.append_page(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some(&label));
            ui.table_profiles
                .borrow_mut()
                .insert(key.clone(), profile.clone());
            ui.table_objects.borrow_mut().insert(
                key.clone(),
                DbObject {
                    schema: schema.into(),
                    name: name.into(),
                    kind: database::DbObjectKind::Table,
                },
            );
            ui.table_edit_states
                .borrow_mut()
                .insert(key.clone(), create_table_edit_state());
            key
        };
        let a = add(&notebook, "public", "a");
        let b = add(&notebook, "public", "b");
        let c = add(&notebook, "public", "c");
        let unrelated = add(&other, "archive", "other");
        notebook.set_current_page(Some(1));
        let selected = notebook.nth_page(Some(1)).unwrap();
        // Exercise the actual menu buttons, including disabled edge actions.
        let buttons = |popover: &gtk::Popover| {
            let actions = popover.child().unwrap();
            let mut buttons = Vec::new();
            let mut next = actions.first_child();
            while let Some(child) = next {
                next = child.next_sibling();
                buttons.push(child.downcast::<gtk::Button>().unwrap());
            }
            buttons
        };
        assert!(!buttons(&menu(&ui, &group, &a))[0].is_sensitive());
        assert!(!buttons(&menu(&ui, &group, &c))[1].is_sensitive());
        buttons(&menu(&ui, &group, &b))[0].emit_clicked();
        assert_eq!(group_keys(&ui, &group), [b.clone(), a.clone(), c.clone()]);
        assert_eq!(
            notebook.nth_page(notebook.current_page()),
            Some(selected.clone())
        );
        buttons(&menu(&ui, &group, &b))[1].emit_clicked();
        assert_eq!(group_keys(&ui, &group), [a.clone(), b.clone(), c.clone()]);
        assert_eq!(notebook.nth_page(notebook.current_page()), Some(selected));
        let state = ui.table_edit_states.borrow().get(&c).unwrap().clone();
        state
            .pending
            .borrow_mut()
            .insert((0, 0), Some("keep me".into()));
        buttons(&menu(&ui, &group, &b))[3].emit_clicked();
        assert_eq!(group_keys(&ui, &group), [a.clone(), b.clone(), c.clone()]);
        assert_eq!(
            state.pending.borrow().get(&(0, 0)),
            Some(&Some("keep me".into()))
        );
        assert!(ui.status.text().contains("Оставлено вкладок: 1"));
        state.pending.borrow_mut().clear();
        state.bulk_column.replace(Some((1, Some("bulk".into()))));
        perform(&ui, &group, &b, TabAction::CloseRight);
        assert_eq!(notebook.n_pages(), 3);
        state.bulk_column.take();
        state.saving.set(true);
        perform(&ui, &group, &b, TabAction::CloseRight);
        assert_eq!(notebook.n_pages(), 3);
        state.saving.set(false);
        state.grid_editing.set(true);
        perform(&ui, &group, &b, TabAction::CloseRight);
        assert_eq!(notebook.n_pages(), 3);
        state.grid_editing.set(false);
        buttons(&menu(&ui, &group, &b))[3].emit_clicked();
        assert_eq!(group_keys(&ui, &group), [a.clone(), b.clone()]);
        // A stale menu must not apply to the page now occupying the old index.
        perform(&ui, &group, &c, TabAction::CloseOthers);
        assert_eq!(notebook.n_pages(), 2);
        buttons(&menu(&ui, &group, &b))[2].emit_clicked();
        assert_eq!(group_keys(&ui, &group), [b.as_str()]);
        assert_eq!(group_keys(&ui, &other_group), [unrelated]);
        assert!(!buttons(&menu(&ui, &group, &b))[2].is_sensitive());
        assert!(ui.table_edit_states.borrow().contains_key(&b));
        assert!(!ui.table_edit_states.borrow().contains_key(&a));
        assert!(!ui.table_edit_states.borrow().contains_key(&c));
        // Open the installed gesture's real popup, then mimic an asynchronous
        // strip refresh while it is open (e.g. a connection-status update).
        // Flush any earlier refresh before installing the synthetic strip item.
        grid_app_tests::until(|| !ui.tab_strip.pending.get());
        let widget = build(&ui, &item(&group, Some(&b)));
        let anchor = widget
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        ui.tab_strip.row.append(&widget);
        grid_app_tests::until(|| anchor.is_mapped());
        let controllers = anchor.observe_controllers();
        let gesture = (0..controllers.n_items())
            .find_map(|at| {
                controllers
                    .item(at)?
                    .downcast::<gtk::GestureClick>()
                    .ok()
                    .filter(|gesture| gesture.button() == 3)
            })
            .expect("tab context-menu gesture");
        gesture.emit_by_name::<()>("pressed", &[&1_i32, &1.0_f64, &1.0_f64]);
        let popover = anchor
            .last_child()
            .unwrap()
            .downcast::<gtk::Popover>()
            .unwrap();
        assert!(popover.is_visible());
        refresh(&ui);
        grid_app_tests::until(|| !ui.tab_strip.pending.get());
        assert!(
            popover.parent().is_none(),
            "refresh must detach the open menu"
        );
        assert!(!popover.is_visible());
        drop(anchor);
        drop(widget);
        ui.window.close();
    }
}
