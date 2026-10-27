//! PostgreSQL server tree. Metadata and expansion belong to a database session.
use super::*;

#[derive(Default)]
pub(super) struct State {
    databases: HashMap<String, Vec<String>>,
    catalogs: HashMap<String, TableCatalog>,
    expanded: HashMap<String, bool>,
    pub schema_expanded: HashMap<(String, String), bool>,
}

fn root_key(profile: &ConnectionProfile) -> String {
    let mut root = profile.clone();
    root.database.clear();
    profile_session_key(&root)
}

pub(super) fn prepare(ui: &Rc<Ui>, profile: &ConnectionProfile) {
    {
        let mut state = ui.catalog_tree.borrow_mut();
        if !profile.database.is_empty() {
            state
                .databases
                .entry(root_key(profile))
                .or_insert_with(|| vec![profile.database.clone()]);
            let key = profile_session_key(profile);
            state.expanded.entry(key).or_insert(true);
        }
    }
    render(ui);
}

pub(super) fn databases_loaded(ui: &Rc<Ui>, profile: &ConnectionProfile, databases: Vec<String>) {
    ui.catalog_tree
        .borrow_mut()
        .databases
        .insert(root_key(profile), databases);
    render(ui);
    ui.status
        .set_text("Раскройте базу данных, чтобы увидеть её схемы и таблицы");
}

pub(super) fn loaded(ui: &Rc<Ui>, profile: &ConnectionProfile, catalog: TableCatalog) {
    ui.catalog_tree
        .borrow_mut()
        .catalogs
        .insert(profile_session_key(profile), catalog);
    render(ui);
}

pub(super) fn forget(ui: &Ui, key: &str) {
    let mut state = ui.catalog_tree.borrow_mut();
    state.catalogs.remove(key);
    state.expanded.remove(key);
    state
        .schema_expanded
        .retain(|(session, _), _| session != key);
}

fn restore_focus(widget: &gtk::Widget, tooltip: &str) -> bool {
    if widget.tooltip_text().as_deref() == Some(tooltip)
        && widget.is_visible()
        && widget.is_sensitive()
        && widget.grab_focus()
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if restore_focus(&widget, tooltip) {
            return true;
        }
    }
    false
}

pub(super) fn render(ui: &Rc<Ui>) {
    let Some(profile) = ui.current_profile.borrow().clone() else {
        return;
    };
    if !matches!(profile.kind, DatabaseKind::PostgreSql) {
        return;
    }
    let focused = gtk::prelude::RootExt::focus(&ui.window).and_then(|w| w.tooltip_text());
    let rows: Vec<_> = {
        let state = ui.catalog_tree.borrow();
        state
            .databases
            .get(&root_key(&profile))
            .into_iter()
            .flatten()
            .map(|database| {
                let mut selected = profile.clone();
                selected.database = database.clone();
                let key = profile_session_key(&selected);
                (
                    selected,
                    state.expanded.get(&key).copied().unwrap_or(false),
                    state.catalogs.get(&key).cloned(),
                )
            })
            .collect()
    };
    clear_box(&ui.objects);
    ui.table_list_items.borrow_mut().clear();
    ui.selected_table_button.borrow_mut().take();
    ui.objects.set_sensitive(true);
    ui.objects_available.set(true);
    set_tables_nav_sensitive(ui, true);
    show_sidebar(ui, "tables");
    ui.table_filter.set_visible(true);
    ui.objects.append(&navigation_section(&format!(
        "БАЗЫ ДАННЫХ · {}",
        rows.len()
    )));
    let source_pool = ui.pool.borrow().clone();
    for (selected, expanded, catalog) in rows {
        let key = profile_session_key(&selected);
        let active = !profile.database.is_empty() && profile_session_key(&profile) == key;
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let disclosure = gtk::Label::new(Some(if expanded { "▾" } else { "▸" }));
        disclosure.set_width_chars(2);
        row.append(&disclosure);
        let icon = penpot_icon_image(if expanded {
            "database-open"
        } else {
            "database-closed"
        });
        icon.remove_css_class("penpot-icon");
        icon.set_pixel_size(16);
        icon.set_size_request(16, 16);
        row.append(&icon);
        let label = gtk::Label::new(Some(&selected.database));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);
        let button = gtk::Button::new();
        button.set_child(Some(&row));
        button.add_css_class("flat");
        button.add_css_class("schema-tree-row");
        button.add_css_class("database-tree-row");
        if active {
            button.add_css_class("database-tree-row-active");
        }
        button.set_tooltip_text(Some(&format!("Открыть базу {}", selected.database)));
        let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
        children.set_margin_start(16);
        children.set_visible(expanded);
        if let Some(catalog) = catalog {
            append_schema_tree(ui, &children, &selected, catalog);
        } else {
            children.append(&gtk::Label::new(Some("Загрузка схем…")));
        }
        let weak = Rc::downgrade(ui);
        let source = source_pool.clone();
        let children_toggle = children.clone();
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let now_expanded = {
                let mut state = ui.catalog_tree.borrow_mut();
                let expanded = state.expanded.entry(key.clone()).or_insert(false);
                *expanded = !*expanded;
                *expanded
            };
            children_toggle.set_visible(now_expanded);
            disclosure.set_text(if now_expanded { "▾" } else { "▸" });
            set_penpot_icon(
                &icon,
                if now_expanded {
                    "database-open"
                } else {
                    "database-closed"
                },
                false,
            );
            if let Some(pool) = source.clone() {
                open_database_session(&ui, selected.clone(), pool, &selected.database);
            }
        });
        ui.objects.append(&button);
        ui.objects.append(&children);
    }
    apply_table_list_filter(ui);
    if let Some(tooltip) = focused {
        restore_focus(ui.objects.upcast_ref(), &tooltip);
    }
}
