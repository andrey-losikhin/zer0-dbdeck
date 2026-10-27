use directories::ProjectDirs;
use gtk::prelude::*;
use relm4::gtk;
use std::{cell::OnceCell, fs};

thread_local! {
    static INSTALLED: OnceCell<bool> = const { OnceCell::new() };
}

const SOURCES: &[(&str, &str)] = &[
    (
        "database",
        include_str!("../assets/icons/penpot/database.svg"),
    ),
    ("tables", include_str!("../assets/icons/penpot/tables.svg")),
    (
        "settings",
        include_str!("../assets/icons/penpot/settings.svg"),
    ),
    ("add", include_str!("../assets/icons/penpot/add.svg")),
    ("edit", include_str!("../assets/icons/penpot/edit.svg")),
    ("delete", include_str!("../assets/icons/penpot/delete.svg")),
    (
        "status-connected",
        include_str!("../assets/icons/penpot/status-connected.svg"),
    ),
    (
        "status-idle",
        include_str!("../assets/icons/penpot/status-idle.svg"),
    ),
    (
        "folder-open",
        include_str!("../assets/icons/penpot/folder-open.svg"),
    ),
    (
        "folder-closed",
        include_str!("../assets/icons/penpot/folder-closed.svg"),
    ),
    (
        "entity-table",
        include_str!("../assets/icons/penpot/entity-table.svg"),
    ),
    (
        "entity-list",
        include_str!("../assets/icons/penpot/entity-list.svg"),
    ),
    (
        "sql-query",
        include_str!("../assets/icons/penpot/sql-query.svg"),
    ),
];

const DATABASE_ICONS: &[(&str, &str)] = &[
    (
        "postgresql",
        include_str!("../assets/icons/vendors/postgresql.svg"),
    ),
    ("mysql", include_str!("../assets/icons/vendors/mysql.svg")),
    (
        "mariadb",
        include_str!("../assets/icons/vendors/mariadb.svg"),
    ),
    (
        "database-open",
        include_str!("../assets/icons/vendors/database-open.svg"),
    ),
    (
        "database-closed",
        include_str!("../assets/icons/vendors/database-closed.svg"),
    ),
];

// Original Penpot paths, normalized to 32px and GTK's symbolic stroke format.
const SYMBOLIC_SOURCES: &[&str] = &[
    include_str!("../assets/icons/symbolic/database.svg"),
    include_str!("../assets/icons/symbolic/tables.svg"),
    include_str!("../assets/icons/symbolic/settings.svg"),
    include_str!("../assets/icons/symbolic/add.svg"),
    include_str!("../assets/icons/symbolic/edit.svg"),
    include_str!("../assets/icons/symbolic/delete.svg"),
    include_str!("../assets/icons/symbolic/status-connected.svg"),
    include_str!("../assets/icons/symbolic/status-idle.svg"),
    include_str!("../assets/icons/symbolic/folder-open.svg"),
    include_str!("../assets/icons/symbolic/folder-closed.svg"),
    include_str!("../assets/icons/symbolic/entity-table.svg"),
    include_str!("../assets/icons/symbolic/entity-list.svg"),
    include_str!("../assets/icons/symbolic/sql-query.svg"),
];

fn recolor(svg: &str, color: &str) -> String {
    // The supplied icons are monochrome. Preserve all paths and viewBox values.
    let mut result = String::with_capacity(svg.len());
    let mut offset = 0;
    while let Some(index) = svg[offset..].find('#') {
        let start = offset + index;
        result.push_str(&svg[offset..start]);
        if svg
            .get(start + 1..start + 7)
            .is_some_and(|s| s.bytes().all(|ch| ch.is_ascii_hexdigit()))
        {
            result.push_str(color);
            offset = start + 7;
        } else {
            result.push('#');
            offset = start + 1;
        }
    }
    result.push_str(&svg[offset..]);
    result
}

fn install() -> bool {
    INSTALLED.with(|installed| *installed.get_or_init(|| {
        let write = || -> Result<(), String> {
            let root = ProjectDirs::from("io", "zer0", "zer0-dbdeck").ok_or("Нет каталога иконок")?
                .cache_dir().join("icons");
            let directory = root.join("hicolor/scalable/actions");
            fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            let mut files = vec![(root.join("hicolor/index.theme"), "[Icon Theme]\nName=DBDeck\nDirectories=scalable/actions\n\n[scalable/actions]\nSize=32\nType=Scalable\nMinSize=8\nMaxSize=256\nContext=Actions\n".to_owned())];
            for ((name, svg), symbolic) in SOURCES.iter().zip(SYMBOLIC_SOURCES) {
                files.push((directory.join(format!("dbdeck-{name}-symbolic.svg")), (*symbolic).to_owned()));
                if matches!(*name, "database" | "entity-table" | "sql-query") {
                    for (_, color) in crate::theme::connection_colors() {
                        files.push((directory.join(format!("dbdeck-{name}-{}.svg", &color[1..])), recolor(svg, color)));
                    }
                }
            }
            for (name, svg) in DATABASE_ICONS {
                files.push((directory.join(format!("dbdeck-{name}-symbolic.svg")), (*svg).to_owned()));
            }
            for (file, content) in files {
                if fs::read_to_string(&file).ok().as_deref() != Some(&content) {
                    fs::write(file, content).map_err(|e| e.to_string())?;
                }
            }
            let display = gtk::gdk::Display::default().ok_or("Нет GTK display")?;
            gtk::IconTheme::for_display(&display).add_search_path(root);
            Ok(())
        };
        if let Err(error) = write() { eprintln!("Не удалось загрузить SVG-иконки DBDeck: {error}"); false } else { true }
    }))
}

pub fn set(image: &gtk::Image, name: &str, hovered: bool) {
    let (base, state) = ["-inactive", "-unselected", "-active"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix).map(|base| (base, *suffix)))
        .unwrap_or((name, ""));
    for class in [
        "icon-accent",
        "icon-muted",
        "icon-disabled",
        "icon-success",
        "icon-danger",
    ] {
        image.remove_css_class(class);
    }
    let class = if state == "-inactive" {
        "icon-disabled"
    } else if state == "-active" || (hovered && base != "delete") || base == "add" {
        "icon-accent"
    } else if hovered && base == "delete" {
        "icon-danger"
    } else if base == "status-connected" {
        "icon-success"
    } else {
        "icon-muted"
    };
    image.add_css_class(class);
    let icon = if install() {
        format!("dbdeck-{base}-symbolic")
    } else {
        "image-missing-symbolic".to_owned()
    };
    image.set_icon_name(Some(&icon));
}

pub fn profile(name: &str, color: &str) -> gtk::Image {
    let icon = if install() {
        format!("dbdeck-{name}-{}", &color[1..])
    } else {
        "image-missing-symbolic".to_owned()
    };
    gtk::Image::from_icon_name(&icon)
}
