use std::{borrow::Cow, fs, path::PathBuf};

use relm4::{adw, gtk};
use serde::{Deserialize, Serialize};

use crate::noctalia::{Rgb, Skin, Theme};

/// Палитры Gruvbox, Nord и Rose Pine; CSS, схема SQL, превью и `Palette` строятся из них.
/// Тинты и hover взяты из прежних готовых CSS, чтобы цвета не изменились.
const GRUVBOX_DARK: Skin = Skin {
    theme: Theme {
        bg: Rgb::hex(0x282828),
        fg: Rgb::hex(0xebdbb2),
        panel: Rgb::hex(0x32302f),
        alternate: Rgb::hex(0x1d2021),
        border: Rgb::hex(0x665c54),
        muted: Rgb::hex(0xa89984),
        accent: Rgb::hex(0x83a598),
        success: Rgb::hex(0xb8bb26),
        warning: Rgb::hex(0xfabd2f),
        error: Rgb::hex(0xfb4934),
        selection: Rgb::hex(0x504945),
        hover: Rgb::hex(0x3c3836),
        secondary: Rgb::hex(0xfb4934),
        tertiary: Rgb::hex(0xb8bb26),
    },
    number: Rgb::hex(0xd3869b),
    function: Rgb::hex(0x8ec07c),
    type_color: Rgb::hex(0xfabd2f),
    highlight: Rgb::hex(0x8ec07c),
    strong_hover: Rgb::hex(0x504945),
    warning_tint: Rgb::hex(0x453d29),
    success_tint: Rgb::hex(0x3c3d28),
    error_tint: Rgb::hex(0x452d2a),
};

const NORD: Skin = Skin {
    theme: Theme {
        bg: Rgb::hex(0x2e3440),
        fg: Rgb::hex(0xd8dee9),
        panel: Rgb::hex(0x3b4252),
        alternate: Rgb::hex(0x2e3440),
        border: Rgb::hex(0x4c566a),
        muted: Rgb::hex(0xb4bccd),
        accent: Rgb::hex(0x88c0d0),
        success: Rgb::hex(0xa3be8c),
        warning: Rgb::hex(0xebcb8b),
        error: Rgb::hex(0xbf616a),
        selection: Rgb::hex(0x434c5e),
        hover: Rgb::hex(0x434c5e),
        secondary: Rgb::hex(0x81a1c1),
        tertiary: Rgb::hex(0xa3be8c),
    },
    number: Rgb::hex(0xb48ead),
    function: Rgb::hex(0x88c0d0),
    type_color: Rgb::hex(0x8fbcbb),
    highlight: Rgb::hex(0x8fbcbb),
    strong_hover: Rgb::hex(0x4c566a),
    warning_tint: Rgb::hex(0x48494b),
    success_tint: Rgb::hex(0x3e474b),
    error_tint: Rgb::hex(0x423a46),
};

const ROSE_PINE: Skin = Skin {
    theme: Theme {
        bg: Rgb::hex(0x191724),
        fg: Rgb::hex(0xe0def4),
        panel: Rgb::hex(0x1f1d2e),
        alternate: Rgb::hex(0x191724),
        border: Rgb::hex(0x524f67),
        muted: Rgb::hex(0x908caa),
        accent: Rgb::hex(0xc4a7e7),
        success: Rgb::hex(0x9ccfd8),
        warning: Rgb::hex(0xf6c177),
        error: Rgb::hex(0xeb6f92),
        selection: Rgb::hex(0x403d52),
        hover: Rgb::hex(0x26233a),
        secondary: Rgb::hex(0xc4a7e7),
        tertiary: Rgb::hex(0xf6c177),
    },
    number: Rgb::hex(0xebbcba),
    function: Rgb::hex(0x9ccfd8),
    type_color: Rgb::hex(0xeb6f92),
    highlight: Rgb::hex(0xebbcba),
    strong_hover: Rgb::hex(0x403d52),
    warning_tint: Rgb::hex(0x382f30),
    success_tint: Rgb::hex(0x2b313d),
    error_tint: Rgb::hex(0x362333),
};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Dark,
    TokyoNight,
    #[default]
    #[serde(alias = "light")]
    CatppuccinMocha,
    GruvboxDark,
    Nord,
    RosePine,
    Noctalia,
    System,
}

impl ThemeMode {
    pub fn id(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::TokyoNight => "tokyo_night",
            Self::CatppuccinMocha => "catppuccin_mocha",
            Self::GruvboxDark => "gruvbox_dark",
            Self::Nord => "nord",
            Self::RosePine => "rose_pine",
            Self::Noctalia => "noctalia",
            Self::System => "system",
        }
    }

    pub fn from_id(id: Option<&str>) -> Self {
        match id {
            Some("dark") => Self::Dark,
            Some("tokyo_night") => Self::TokyoNight,
            Some("gruvbox_dark") => Self::GruvboxDark,
            Some("nord") => Self::Nord,
            Some("rose_pine") => Self::RosePine,
            Some("noctalia") => Self::Noctalia,
            Some("system") => Self::System,
            _ => Self::CatppuccinMocha,
        }
    }
}

fn config_dir() -> Result<PathBuf, String> {
    crate::app_instance::AppInstance::current()
        .project_dirs()
        .map(|dirs| dirs.config_dir().to_owned())
        .ok_or_else(|| "Не удалось определить каталог настроек XDG".to_owned())
}

pub fn load_mode() -> ThemeMode {
    let Ok(path) = config_dir() else {
        return ThemeMode::default();
    };
    let Ok(data) = fs::read(path.join("theme.json")) else {
        return ThemeMode::default();
    };
    serde_json::from_slice::<ThemePreferences>(&data)
        .map(|prefs| prefs.mode)
        .unwrap_or_default()
}

pub fn save_mode(mode: ThemeMode) -> Result<(), String> {
    let path = config_dir()?.join("theme.json");
    let parent = path
        .parent()
        .ok_or_else(|| "Не удалось определить каталог настроек".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("Не удалось создать настройки: {error}"))?;
    let data = serde_json::to_vec_pretty(&ThemePreferences { mode })
        .map_err(|error| format!("Не удалось сериализовать тему: {error}"))?;
    fs::write(path, data).map_err(|error| format!("Не удалось сохранить тему: {error}"))
}

#[derive(Deserialize, Serialize)]
struct ThemePreferences {
    #[serde(default)]
    mode: ThemeMode,
}

fn install_sql_schemes() -> Result<PathBuf, String> {
    let path = crate::app_instance::AppInstance::current()
        .project_dirs()
        .ok_or("Не удалось определить каталог схем SQL")?
        .cache_dir()
        .join("sql-styles");
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let noctalia_xml = crate::noctalia::current().scheme_xml();
    let preset_xml = [
        ("gruvbox-dark", GRUVBOX_DARK),
        ("nord", NORD),
        ("rose-pine", ROSE_PINE),
    ]
    .map(|(slug, skin)| {
        (
            format!("dbdeck-{slug}.xml"),
            skin.scheme_xml(&format!("dbdeck-{slug}")),
        )
    });
    let bundled = [
        (
            "dbdeck-mocha.xml",
            include_str!("../assets/styles/dbdeck-mocha.xml"),
        ),
        (
            "dbdeck-dark.xml",
            include_str!("../assets/styles/dbdeck-dark.xml"),
        ),
        (
            "dbdeck-tokyo-night.xml",
            include_str!("../assets/styles/dbdeck-tokyo-night.xml"),
        ),
        ("dbdeck-noctalia.xml", noctalia_xml.as_str()),
    ];
    let generated = preset_xml
        .iter()
        .map(|(name, xml)| (name.as_str(), xml.as_str()));
    for (name, xml) in bundled.into_iter().chain(generated) {
        let file = path.join(name);
        if fs::read_to_string(&file).ok().as_deref() != Some(xml) {
            fs::write(file, xml).map_err(|e| e.to_string())?;
        }
    }
    Ok(path)
}

pub fn sql_style_schemes() -> sourceview5::StyleSchemeManager {
    let manager = sourceview5::StyleSchemeManager::new();
    match install_sql_schemes() {
        Ok(path) => manager.prepend_search_path(&path.to_string_lossy()),
        Err(error) => eprintln!("Не удалось загрузить палитры SQL: {error}"),
    }
    manager
}

/// Записывает свежую схему Noctalia в кеш и заставляет менеджер перечитать каталог.
pub fn refresh_sql_schemes(manager: &sourceview5::StyleSchemeManager) {
    match install_sql_schemes() {
        Ok(_) => manager.force_rescan(),
        Err(error) => eprintln!("Не удалось обновить палитры SQL: {error}"),
    }
}

pub fn sql_scheme_id(mode: ThemeMode, dark: bool) -> &'static str {
    match mode {
        ThemeMode::CatppuccinMocha => "dbdeck-mocha",
        ThemeMode::Dark => "dbdeck-dark",
        ThemeMode::TokyoNight => "dbdeck-tokyo-night",
        ThemeMode::GruvboxDark => "dbdeck-gruvbox-dark",
        ThemeMode::Nord => "dbdeck-nord",
        ThemeMode::RosePine => "dbdeck-rose-pine",
        ThemeMode::Noctalia => "dbdeck-noctalia",
        ThemeMode::System if dark => "Adwaita-dark",
        ThemeMode::System => "Adwaita",
    }
}

#[derive(Clone)]
pub struct Palette {
    pub bg: String,
    pub fg: String,
    pub panel: String,
    pub alternate: String,
    pub border: String,
    pub muted: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub selection: String,
}

pub fn palette(mode: ThemeMode) -> Palette {
    let system_dark = !gtk::is_initialized_main_thread() || adw::StyleManager::default().is_dark();
    let values = match mode {
        ThemeMode::CatppuccinMocha => [
            "#11111b", "#cdd6f4", "#1e1e2e", "#181825", "#45475a", "#9399b2", "#94e2d5", "#a6e3a1",
            "#f9e2af", "#f38ba8", "#313244",
        ],
        ThemeMode::TokyoNight => [
            "#1a1b26", "#c0caf5", "#1f2335", "#16161e", "#414868", "#9aa5ce", "#7aa2f7", "#9ece6a",
            "#e0af68", "#f7768e", "#292e42",
        ],
        ThemeMode::Dark => [
            "#111923", "#d7e1eb", "#1b2632", "#151f2b", "#3a4a5a", "#b9c5d1", "#42b8b0", "#3fb950",
            "#e3b341", "#ff7b72", "#20383c",
        ],
        ThemeMode::GruvboxDark => return GRUVBOX_DARK.theme.palette(),
        ThemeMode::Nord => return NORD.theme.palette(),
        ThemeMode::RosePine => return ROSE_PINE.theme.palette(),
        ThemeMode::Noctalia => return crate::noctalia::current().palette(),
        ThemeMode::System if system_dark => [
            "#1e1e1e", "#ffffff", "#303030", "#242424", "#555555", "#bdbdbd", "#3584e4", "#2ec27e",
            "#f5c211", "#f66151", "#3d3d3d",
        ],
        ThemeMode::System => [
            "#ffffff", "#2e3436", "#f6f5f4", "#fafafa", "#b6b6b6", "#5e5c64", "#3584e4", "#26a269",
            "#9c6e03", "#c01c28", "#deddda",
        ],
    };
    let [
        bg,
        fg,
        panel,
        alternate,
        border,
        muted,
        accent,
        success,
        warning,
        error,
        selection,
    ] = values.map(str::to_owned);
    let mut palette = Palette {
        bg,
        fg,
        panel,
        alternate,
        border,
        muted,
        accent,
        success,
        warning,
        error,
        selection,
    };
    if mode == ThemeMode::System && gtk::is_initialized_main_thread() {
        use gtk::prelude::*;
        let widget = gtk::Label::new(None);
        let context = widget.style_context();
        for (value, name) in [
            (&mut palette.bg, "view_bg_color"),
            (&mut palette.fg, "view_fg_color"),
            (&mut palette.panel, "headerbar_bg_color"),
            (&mut palette.alternate, "window_bg_color"),
            (&mut palette.accent, "accent_color"),
            (&mut palette.success, "success_color"),
            (&mut palette.warning, "warning_color"),
            (&mut palette.error, "error_color"),
        ] {
            if let Some(color) = context.lookup_color(name) {
                *value = color.to_string().to_string();
            }
        }
    }
    palette
}

pub fn preview_colors(mode: ThemeMode) -> [String; 6] {
    match mode {
        ThemeMode::TokyoNight => [
            "#c0caf5", "#bb9af7", "#9ece6a", "#ff9e64", "#7aa2f7", "#7dcfff",
        ]
        .map(str::to_owned),
        ThemeMode::CatppuccinMocha => [
            "#cdd6f4", "#cba6f7", "#a6e3a1", "#fab387", "#89b4fa", "#94e2d5",
        ]
        .map(str::to_owned),
        ThemeMode::Dark => [
            "#d7e1eb", "#c3a6f5", "#8cceac", "#eab683", "#80b8ed", "#42b8b0",
        ]
        .map(str::to_owned),
        ThemeMode::GruvboxDark => GRUVBOX_DARK.preview_colors(),
        ThemeMode::Nord => NORD.preview_colors(),
        ThemeMode::RosePine => ROSE_PINE.preview_colors(),
        ThemeMode::Noctalia => crate::noctalia::current().skin().preview_colors(),
        ThemeMode::System => {
            let p = palette(mode);
            [
                p.fg,
                p.accent.clone(),
                p.success,
                p.warning,
                p.accent,
                p.muted,
            ]
        }
    }
}

pub fn connection_colors() -> [(&'static str, &'static str); 7] {
    [
        ("Синий", "#4f8fe8"),
        ("Бирюзовый", "#42b8b0"),
        ("Зелёный", "#55b879"),
        ("Жёлтый", "#e4b84f"),
        ("Оранжевый", "#e58b4f"),
        ("Красный", "#df626b"),
        ("Фиолетовый", "#a57be8"),
    ]
}

pub fn grid_css(mode: ThemeMode) -> String {
    let p = palette(mode);
    format!(
        ":root{{--grid-bg:{};--grid-fg:{};--grid-panel:{};--grid-alt:{};--grid-border:{};--grid-muted:{};--grid-accent:{};--grid-warning:{};--grid-selection:{};--grid-range-bg:color-mix(in srgb, {} 12%, transparent);--grid-edited-bg:color-mix(in srgb, {} 14%, transparent);}}",
        p.bg,
        p.fg,
        p.panel,
        p.alternate,
        p.border,
        p.muted,
        p.accent,
        p.warning,
        p.selection,
        p.accent,
        p.warning
    )
}

fn interface_roles(mode: ThemeMode) -> String {
    let p = palette(mode);
    let mut css = format!(
        "@define-color dbdeck_fg {}; @define-color dbdeck_muted {}; @define-color dbdeck_border {}; @define-color dbdeck_success {}; @define-color dbdeck_warning {}; @define-color dbdeck_error {}; @define-color dbdeck_panel {}; @define-color dbdeck_accent {}; @define-color dbdeck_bg {}; @define-color dbdeck_selection {}; @define-color dbdeck_alt {};",
        p.fg,
        p.muted,
        p.border,
        p.success,
        p.warning,
        p.error,
        p.panel,
        p.accent,
        p.bg,
        p.selection,
        p.alternate
    );
    if mode == ThemeMode::System {
        css = "@define-color dbdeck_fg @window_fg_color; @define-color dbdeck_muted @window_fg_color; @define-color dbdeck_border alpha(@window_fg_color, 0.3); @define-color dbdeck_success @success_color; @define-color dbdeck_warning @warning_color; @define-color dbdeck_error @error_color; @define-color dbdeck_panel @window_bg_color; @define-color dbdeck_accent @accent_color; @define-color dbdeck_bg @view_bg_color; @define-color dbdeck_selection alpha(@accent_color, 0.25); @define-color dbdeck_alt @window_bg_color;".to_owned();
    }
    let noctalia = crate::noctalia::current();
    css.push_str(&format!(
        ".dbdeck-settings .settings-preview.noctalia{{background:{};color:{};border-color:{};}}",
        noctalia.bg, noctalia.fg, noctalia.border
    ));
    css.push_str(&format!(
        ".tab-schema-name{{color:{};font-weight:600;}}",
        p.accent
    ));
    for ((_, color), class) in connection_colors()
        .iter()
        .zip(["blue", "teal", "green", "yellow", "orange", "red", "purple"])
    {
        css.push_str(&format!(".connection-card-active.connection-color-{class},.table-context-popover.connection-color-{class}>contents{{border-color:{color};}} .active-connection-header.connection-color-{class}{{border-left-color:{color};}} .connection-row.connection-color-{class}{{border-left-color:{color};}}"));
    }
    css
}

pub fn apply_mode(mode: ThemeMode, provider: &gtk::CssProvider) {
    let style_manager = adw::StyleManager::default();
    style_manager.set_color_scheme(match mode {
        ThemeMode::System => adw::ColorScheme::PreferLight,
        ThemeMode::Noctalia if !crate::noctalia::current().is_dark() => {
            adw::ColorScheme::ForceLight
        }
        ThemeMode::Dark
        | ThemeMode::CatppuccinMocha
        | ThemeMode::TokyoNight
        | ThemeMode::GruvboxDark
        | ThemeMode::Nord
        | ThemeMode::RosePine
        | ThemeMode::Noctalia => adw::ColorScheme::ForceDark,
    });
    let colors: Cow<'static, str> = match mode {
        ThemeMode::Dark => Cow::Borrowed(DARK_CSS),
        ThemeMode::TokyoNight => Cow::Borrowed(include_str!("../assets/themes/tokyo-night.css")),
        ThemeMode::GruvboxDark => Cow::Owned(GRUVBOX_DARK.css("Gruvbox Dark palette")),
        ThemeMode::Nord => Cow::Owned(NORD.css("Nord palette")),
        ThemeMode::RosePine => Cow::Owned(ROSE_PINE.css("Rose Pine palette")),
        ThemeMode::Noctalia => Cow::Owned(crate::noctalia::current().css()),
        ThemeMode::CatppuccinMocha => Cow::Borrowed(CATPPUCCIN_MOCHA_CSS),
        ThemeMode::System => Cow::Borrowed(SYSTEM_CSS),
    };
    provider.load_from_data(&colors);
    let roles = interface_roles(mode);
    let layout = include_str!("../assets/themes/layout.css");
    provider.load_from_data(&format!(
        "{colors}\n{roles}\n{SQL_CONTROLS_CSS}\n{SETTINGS_CSS}\n{THEMED_ICONS_CSS}\n{SELECT_CSS}\n{CATALOG_TREE_CSS}\n{layout}"
    ));
}

const SYSTEM_CSS: &str = r#"
.add-connection-button:hover, .connected-action:hover { background: color-mix(in srgb, var(--window-fg-color) 10%, transparent); }
window { background: var(--window-bg-color); color: var(--window-fg-color); font-family: "Roboto Flex", "Noto Sans", sans-serif; }
headerbar { background: var(--headerbar-bg-color); color: var(--headerbar-fg-color); border-bottom: 0; box-shadow: none; }
.sql-editor { font-family: monospace; font-size: 12px; }
.sql-editor-region, .sql-results-region { background: var(--window-bg-color); }
.sql-toolbar { background: var(--card-bg-color); border-top: 1px solid var(--border-color); padding: 6px 8px; }
.query-risk-indicator { border-radius: 999px; padding: 2px 8px; }
.query-risk-indicator.status-pending { background: color-mix(in srgb, var(--warning-color) 14%, transparent); }
.query-risk-indicator.status-success { background: color-mix(in srgb, var(--success-color) 14%, transparent); }
.query-error-message { color: var(--error-color); background: color-mix(in srgb, var(--error-color) 10%, transparent); border-radius: 6px; }
.table-edit-toolbar { background: var(--card-bg-color); border-top: 1px solid var(--border-color); padding: 6px 8px; }
.grid-cell-edited { color: var(--warning-color); background: color-mix(in srgb, var(--warning-color) 16%, var(--view-bg-color)); border-color: var(--warning-color); }
.grid-column-selected { background: color-mix(in srgb, var(--accent-color) 14%, var(--view-bg-color)); }
button.suggested-action.table-save-ready, button.suggested-action.table-save-ready:hover { background: #227a4b; color: #ffffff; }
.sql-tab-add { min-width: 28px; min-height: 28px; padding: 4px; }
notebook > header.top > tabs { background: var(--window-bg-color); border-bottom: 1px solid var(--border-color); padding: 0; }
notebook > header.top > tabs > tab { background: var(--window-bg-color); color: color-mix(in srgb, var(--window-fg-color) 72%, transparent); padding: 3px 8px; margin: 0; border-radius: 0; border-right: 0; border-bottom: 2px solid transparent; }
notebook > header.top > tabs > tab:hover { background: color-mix(in srgb, var(--window-fg-color) 6%, var(--window-bg-color)); color: var(--window-fg-color); }
notebook > header.top > tabs > tab:checked { background: var(--window-bg-color); color: var(--window-fg-color); border-bottom: 2px solid var(--accent-color); }
.compact-tab-close:hover { background: color-mix(in srgb, var(--window-fg-color) 12%, transparent); }
window.preferences, preferencespage { background: var(--window-bg-color); color: var(--window-fg-color); }
preferencesgroup { color: var(--window-fg-color); }
.preferences row { background: var(--card-bg-color); color: var(--card-fg-color); border-color: var(--border-color); }
.preferences row:hover { background: color-mix(in srgb, var(--card-fg-color) 8%, var(--card-bg-color)); }
.navigation-rail { background: var(--sidebar-bg-color); border-right: 1px solid var(--sidebar-border-color); }
.sidebar-panel, .sidebar-stack { background: var(--sidebar-bg-color); color: var(--sidebar-fg-color); }
.sidebar-title { color: color-mix(in srgb, var(--sidebar-fg-color) 78%, transparent); font-size: 14px; font-weight: 500; }
.sidebar-nav-button { color: color-mix(in srgb, var(--sidebar-fg-color) 72%, transparent); }
.sidebar-nav-button:hover, .table-nav-item:hover { background: color-mix(in srgb, var(--sidebar-fg-color) 8%, var(--sidebar-bg-color)); color: var(--sidebar-fg-color); }
button:hover image { color: var(--accent-color); }
.sidebar-nav-active { background: transparent; color: var(--accent-color); }
.schema-tree-row:hover { background: color-mix(in srgb, var(--sidebar-fg-color) 8%, var(--sidebar-bg-color)); color: var(--sidebar-fg-color); }
.schema-tree-row-active, .schema-tree-row-active:hover { background: transparent; color: var(--accent-color); }
.schema-tree-row-active label { color: var(--accent-color); }
.table-nav-item-selected, .table-nav-item-selected:hover { background: transparent; color: var(--accent-color); }
.table-nav-item-selected label { color: var(--accent-color); opacity: 1; }
.table-nav-item-selected:hover { background: color-mix(in srgb, var(--sidebar-fg-color) 10%, var(--sidebar-bg-color)); }
.connection-card { background: var(--card-bg-color); color: var(--card-fg-color); border: 1px solid var(--border-color); }
.connection-card-active.connection-color-blue { border-color: #4f8fe8; }
.connection-card-active.connection-color-teal { border-color: #42b8b0; }
.connection-card-active.connection-color-green { border-color: #55b879; }
.connection-card-active.connection-color-yellow { border-color: #e4b84f; }
.connection-card-active.connection-color-orange { border-color: #e58b4f; }
.connection-card-active.connection-color-red { border-color: #df626b; }
.connection-card-active.connection-color-purple { border-color: #a57be8; }
.connection-card .heading { color: var(--card-fg-color); font-size: 14px; font-weight: 400; }
.connection-card .dim-label, .connection-details, .dim-label { color: color-mix(in srgb, var(--window-fg-color) 72%, transparent); }
.connection-card .dim-label { font-size: 10px; }
.connection-card .environment-tag { font-size: 14px; font-weight: 400; }
.active-connection-header { background: var(--card-bg-color); color: var(--card-fg-color); border: 1px solid var(--border-color); }
.active-connection-header.connection-color-blue { border-left-color: #4f8fe8; }
.active-connection-header.connection-color-teal { border-left-color: #42b8b0; }
.active-connection-header.connection-color-green { border-left-color: #55b879; }
.active-connection-header.connection-color-yellow { border-left-color: #e4b84f; }
.active-connection-header.connection-color-orange { border-left-color: #e58b4f; }
.active-connection-header.connection-color-red { border-left-color: #df626b; }
.active-connection-header.connection-color-purple { border-left-color: #a57be8; }
.table-nav-item { color: var(--sidebar-fg-color); }
.table-filter-panel { background: var(--view-bg-color); color: var(--view-fg-color); border-bottom: 1px solid var(--border-color); }
.table-filter-row entry, .table-filter-row combobox, entry, combobox { background: var(--view-bg-color); color: var(--view-fg-color); border-color: var(--border-color); }
entry:focus, combobox:focus { border-color: var(--accent-color); }
button.grid-header, .grid-header { background: color-mix(in srgb, var(--view-fg-color) 12%, var(--view-bg-color)); color: var(--view-fg-color); border-color: var(--border-color); min-width: 0; padding: 0; }
button.grid-header:hover { background: color-mix(in srgb, var(--view-fg-color) 18%, var(--view-bg-color)); }
.grid-sort-button { min-width: 24px; min-height: 24px; padding: 2px 4px; margin: 0; border-radius: 4px; }
.grid-header-label { min-height: 0; padding: 3px 6px; margin: 0; }
.grid-column-resize-handle { min-width: 6px; background: transparent; }
.grid-column-resize-handle:hover { background: color-mix(in srgb, var(--accent-color) 22%, transparent); }
.grid-row-header { min-width: 38px; min-height: 28px; padding: 2px 5px; border-radius: 0; color: color-mix(in srgb, var(--view-fg-color) 66%, transparent); background: color-mix(in srgb, var(--view-fg-color) 7%, var(--view-bg-color)); font-weight: 400; }
.grid-row-header-selected { color: var(--accent-color); background: color-mix(in srgb, var(--accent-color) 16%, var(--view-bg-color)); }
.grid-cell { background: var(--view-bg-color); color: var(--view-fg-color); border-color: var(--border-color); min-width: 48px; padding: 4px 8px; border: 1px solid transparent; }
.grid-cell-inline-editor, .grid-cell-inline-editor:focus { background: var(--view-bg-color); color: var(--view-fg-color); border: 1px solid var(--border-color); border-radius: 0; box-shadow: inset 0 0 0 2px var(--accent-color); padding: 4px 8px; margin: 0; min-width: 0; min-height: 0; }
.grid-cell.alternate { background: color-mix(in srgb, var(--view-fg-color) 4.5%, var(--view-bg-color)); }
.grid-cell.row-hover, .grid-cell.alternate.row-hover { border-top-color: var(--accent-color); border-bottom-color: var(--accent-color); }
.grid-cell.grid-row-first.row-hover { border-left-color: var(--accent-color); }
.grid-cell.grid-row-last.row-hover { border-right-color: var(--accent-color); }
.grid-cell.grid-cell-selected, .grid-cell.row-hover.grid-cell-selected, .grid-cell.alternate.row-hover.grid-cell-selected { box-shadow: inset 0 0 0 2px var(--accent-color); }
.grid-cell.grid-cell-range { background: color-mix(in srgb, var(--accent-color) 12%, var(--view-bg-color)); }
.grid-cell.grid-cell-copied { outline: 1px dashed var(--accent-color); outline-offset: -2px; }
.grid-cell.grid-cell-edited { color: var(--warning-color); background: color-mix(in srgb, var(--warning-color) 16%, var(--view-bg-color)); border-color: var(--warning-color); }
.grid-cell.grid-cell-edited.grid-cell-selected { box-shadow: inset 0 0 0 2px var(--warning-color); }
.grid-cell.null-cell { color: color-mix(in srgb, var(--view-fg-color) 72%, transparent); }
.grid-cell.truncated-cell, .status-pending { color: var(--warning-color); }
.grid-cell.decode-error, .status-error { color: var(--error-color); }
.tag-prod { background: color-mix(in srgb, var(--error-bg-color) 14%, transparent); color: var(--error-color); }
.tag-stage { background: color-mix(in srgb, var(--warning-bg-color) 14%, transparent); color: var(--warning-color); }
.tag-local { background: color-mix(in srgb, var(--success-bg-color) 14%, transparent); color: var(--success-color); }
.status-success { color: var(--success-color); }
.tag-active { background: var(--accent-bg-color); color: var(--accent-fg-color); }
.table-context-popover > contents, popover > contents { background: var(--popover-bg-color); color: var(--popover-fg-color); border: 1px solid var(--border-color); }
.table-context-popover.connection-color-blue > contents { border-color: #4f8fe8; }
.table-context-popover.connection-color-teal > contents { border-color: #42b8b0; }
.table-context-popover.connection-color-green > contents { border-color: #55b879; }
.table-context-popover.connection-color-yellow > contents { border-color: #e4b84f; }
.table-context-popover.connection-color-orange > contents { border-color: #e58b4f; }
.table-context-popover.connection-color-red > contents { border-color: #df626b; }
.table-context-popover.connection-color-purple > contents { border-color: #a57be8; }
.table-context-action:hover { background: color-mix(in srgb, var(--popover-fg-color) 10%, var(--popover-bg-color)); color: var(--popover-fg-color); }
.table-context-action image { color: var(--popover-fg-color); opacity: 1; }
.color-option-selected { outline-color: var(--accent-color); }
button.suggested-action { background: var(--accent-bg-color); color: var(--accent-fg-color); }
button.suggested-action:hover { background: var(--accent-bg-color); color: var(--accent-fg-color); }
button:focus { outline-color: var(--accent-color); }
frame { border-color: var(--border-color); }

/* Compact data/query tabs; the surrounding header stays transparent. */
notebook.workspace-tabs > header.top { background: transparent; border: 0; box-shadow: none; padding: 0; }
notebook.workspace-tabs > header.top > tabs { background: transparent; border: 0; padding: 0; min-height: 28px; }
notebook.workspace-tabs > header.top > tabs > tab { background: var(--card-bg-color); padding: 2px 8px; margin: 0 2px 0 0; border: 0; border-radius: 5px 5px 0 0; min-height: 22px; box-shadow: none; }
notebook.workspace-tabs > header.top > tabs > tab:checked { background: var(--view-bg-color); color: var(--window-fg-color); border: 0; box-shadow: none; font-weight: 600; }
notebook.workspace-tabs > header.top > button.sql-tab-add { background: transparent; border: 1px solid var(--border-color); border-radius: 999px; padding: 2px; margin: 0 0 0 1px; min-width: 24px; min-height: 24px; box-shadow: none; }
notebook.workspace-tabs > header.top > button.sql-tab-add:hover { background: var(--card-bg-color); }
"#;

const CATPPUCCIN_MOCHA_CSS: &str = r#"
@define-color accent_color #94e2d5;
@define-color accent_bg_color #94e2d5;
@define-color accent_fg_color #1e1e2e;
window { background: #1e1e2e; color: #cdd6f4; font-family: "Roboto Flex", "Noto Sans", sans-serif; }
headerbar { background: #181825; color: #cdd6f4; border-bottom: 0; box-shadow: none; }
.sql-editor { font-family: monospace; font-size: 12px; }
.sql-editor-region, .sql-results-region { background: #11111b; }
.sql-toolbar { background: #1e1e2e; border-top: 1px solid #45475a; padding: 6px 8px; }
.query-risk-indicator { border-radius: 999px; padding: 2px 8px; }
.query-risk-indicator.status-pending { background: #453b32; color: #f9e2af; }
.query-risk-indicator.status-success { background: #313244; }
.query-error-message { color: #f38ba8; background: #313244; border-radius: 6px; }
.table-edit-toolbar { background: #1e1e2e; border-top: 1px solid #45475a; padding: 6px 8px; }
.grid-cell-edited { color: #f9e2af; background: color-mix(in srgb, #f9e2af 16%, #11111b); border-color: #f9e2af; }
button.suggested-action.table-save-ready, button.suggested-action.table-save-ready:hover { background: #a6e3a1; color: #1e1e2e; }
.grid-column-selected { background: #273a43; }
.sql-tab-add { min-width: 28px; min-height: 28px; padding: 4px; }
notebook > header.top > tabs { background: #11111b; border-bottom: 1px solid #45475a; padding: 0; }
notebook > header.top > tabs > tab { background: #11111b; color: #9399b2; padding: 3px 8px; margin: 0; border-radius: 0; border-right: 0; border-bottom: 2px solid transparent; }
notebook > header.top > tabs > tab:hover { background: #181825; color: #cdd6f4; }
notebook > header.top > tabs > tab:checked { background: #1e1e2e; color: #cdd6f4; border-bottom: 2px solid #94e2d5; }
.add-connection-button:hover, .connected-action:hover { background: #45475a; }
.compact-tab-close:hover { background: #313244; }
window.preferences, preferencespage { background: #1e1e2e; color: #cdd6f4; }
preferencesgroup { color: #cdd6f4; }
.preferences row { background: #181825; color: #cdd6f4; border-color: #45475a; }
.preferences row:hover { background: #313244; }
.navigation-rail { background: #11111b; border-right: 1px solid #45475a; }
.sidebar-panel, .sidebar-stack { background: #181825; color: #cdd6f4; }
.sidebar-title { color: #9399b2; font-size: 14px; font-weight: 500; }
.sidebar-nav-button { color: #9399b2; }
.sidebar-nav-button:hover, .table-nav-item:hover { background: #313244; color: #cdd6f4; }
button:hover image { color: #94e2d5; }
.sidebar-nav-active { background: transparent; color: #94e2d5; }
.schema-tree-row:hover { background: #313244; color: #cdd6f4; }
.schema-tree-row-active, .schema-tree-row-active:hover { background: transparent; color: #94e2d5; }
.schema-tree-row-active label { color: #94e2d5; }
.table-nav-item-selected, .table-nav-item-selected:hover { background: transparent; color: #94e2d5; }
.table-nav-item-selected label { color: #94e2d5; opacity: 1; }
.table-nav-item-selected:hover { background: #45475a; }
.connection-card { background: #11111b; color: #cdd6f4; border: 1px solid #45475a; }
.connection-card-active.connection-color-blue { border-color: #74c7ec; }
.connection-card-active.connection-color-teal { border-color: #94e2d5; }
.connection-card-active.connection-color-green { border-color: #a6e3a1; }
.connection-card-active.connection-color-yellow { border-color: #f9e2af; }
.connection-card-active.connection-color-orange { border-color: #fab387; }
.connection-card-active.connection-color-red { border-color: #f38ba8; }
.connection-card-active.connection-color-purple { border-color: #cba6f7; }
.connection-card .heading { color: #cdd6f4; font-size: 14px; font-weight: 400; }
.connection-card .dim-label { font-size: 10px; }
.connection-card .environment-tag { font-size: 14px; font-weight: 400; }
.active-connection-header { background: #313244; color: #cdd6f4; border: 1px solid #45475a; }
.active-connection-header.connection-color-blue { border-left-color: #89b4fa; }
.active-connection-header.connection-color-teal { border-left-color: #94e2d5; }
.active-connection-header.connection-color-green { border-left-color: #a6e3a1; }
.active-connection-header.connection-color-yellow { border-left-color: #f9e2af; }
.active-connection-header.connection-color-orange { border-left-color: #fab387; }
.active-connection-header.connection-color-red { border-left-color: #f38ba8; }
.active-connection-header.connection-color-purple { border-left-color: #cba6f7; }
.connection-details, .dim-label { color: #9399b2; }
.table-nav-item { color: #cdd6f4; }
.table-filter-panel { background: #181825; color: #cdd6f4; border-bottom: 1px solid #45475a; }
.table-filter-row entry, .table-filter-row combobox, entry, combobox { background: #11111b; color: #cdd6f4; border-color: #585b70; }
entry:focus, combobox:focus { border-color: #94e2d5; }
button.grid-header, .grid-header { background: #313244; color: #cdd6f4; border-color: #45475a; min-width: 0; padding: 0; }
button.grid-header:hover { background: #45475a; }
.grid-sort-button { min-width: 24px; min-height: 24px; padding: 2px 4px; margin: 0; border-radius: 4px; }
.grid-header-label { min-height: 0; padding: 3px 6px; margin: 0; }
.grid-column-resize-handle { min-width: 6px; background: transparent; }
.grid-column-resize-handle:hover { background: color-mix(in srgb, #94e2d5 22%, transparent); }
.grid-row-header { min-width: 38px; min-height: 28px; padding: 2px 5px; border-radius: 0; color: #9399b2; background: #181825; font-weight: 400; }
.grid-row-header-selected { color: #94e2d5; background: #263640; }
.grid-cell { background: #11111b; color: #cdd6f4; border-color: #45475a; min-width: 48px; padding: 4px 8px; border: 1px solid transparent; }
.grid-cell-inline-editor, .grid-cell-inline-editor:focus { background: #11111b; color: #cdd6f4; border: 1px solid #45475a; border-radius: 0; box-shadow: inset 0 0 0 2px #89b4fa; padding: 4px 8px; }
.grid-cell.alternate { background: #181825; }
.grid-cell.row-hover, .grid-cell.alternate.row-hover { border-top-color: #94e2d5; border-bottom-color: #94e2d5; }
.grid-cell.grid-row-first.row-hover { border-left-color: #94e2d5; }
.grid-cell.grid-row-last.row-hover { border-right-color: #94e2d5; }
.grid-cell.grid-cell-selected { box-shadow: inset 0 0 0 2px #89b4fa; }
.grid-cell.row-hover.grid-cell-selected, .grid-cell.alternate.row-hover.grid-cell-selected { box-shadow: inset 0 0 0 2px #89b4fa; }
.grid-cell.grid-cell-range { background: color-mix(in srgb, #89b4fa 12%, var(--view-bg-color)); }
.grid-cell.grid-cell-copied { outline: 1px dashed #89b4fa; outline-offset: -2px; }
.grid-cell.grid-cell-edited { color: #f9e2af; background: color-mix(in srgb, #f9e2af 16%, #11111b); border-color: #f9e2af; }
.grid-cell.grid-cell-edited.grid-cell-selected { box-shadow: inset 0 0 0 2px #f9e2af; }
.grid-cell.null-cell { color: #9399b2; }
.grid-cell.truncated-cell, .status-pending { color: #f9e2af; }
.grid-cell.decode-error, .status-error { color: #f38ba8; }
.tag-prod { background: #313244; color: #f38ba8; }
.tag-stage { background: #313244; color: #f9e2af; }
.tag-local { background: #313244; color: #a6e3a1; }
.status-success { color: #a6e3a1; }
.tag-active { background: #94e2d5; color: #1e1e2e; }
.table-context-popover > contents, popover > contents { background: #313244; color: #cdd6f4; border: 1px solid #585b70; }
.table-context-popover.connection-color-blue > contents { border-color: #74c7ec; }
.table-context-popover.connection-color-teal > contents { border-color: #94e2d5; }
.table-context-popover.connection-color-green > contents { border-color: #a6e3a1; }
.table-context-popover.connection-color-yellow > contents { border-color: #f9e2af; }
.table-context-popover.connection-color-orange > contents { border-color: #fab387; }
.table-context-popover.connection-color-red > contents { border-color: #f38ba8; }
.table-context-popover.connection-color-purple > contents { border-color: #cba6f7; }
.table-context-action:hover { background: #45475a; color: #cdd6f4; }
.table-context-action image { color: #cdd6f4; opacity: 1; }
.color-option-selected { outline-color: #94e2d5; }
button.suggested-action { background: #94e2d5; color: #1e1e2e; }
button.suggested-action:hover { background: #94e2d5; color: #1e1e2e; }
button:focus { outline-color: #94e2d5; }
frame { border-color: #45475a; }

/* Compact data/query tabs; the surrounding header stays transparent. */
notebook.workspace-tabs > header.top { background: transparent; border: 0; box-shadow: none; padding: 0; }
notebook.workspace-tabs > header.top > tabs { background: transparent; border: 0; padding: 0; min-height: 28px; }
notebook.workspace-tabs > header.top > tabs > tab { background: #242431; padding: 2px 8px; margin: 0 2px 0 0; border: 0; border-radius: 5px 5px 0 0; min-height: 22px; box-shadow: none; }
notebook.workspace-tabs > header.top > tabs > tab:checked { background: #11111b; color: #cdd6f4; border: 0; box-shadow: none; font-weight: 600; }
notebook.workspace-tabs > header.top > button.sql-tab-add { background: transparent; border: 1px solid #45475a; border-radius: 999px; padding: 2px; margin: 0 0 0 1px; min-width: 24px; min-height: 24px; box-shadow: none; }
notebook.workspace-tabs > header.top > button.sql-tab-add:hover { background: #242431; }
"#;

const DARK_CSS: &str = r#"
@define-color accent_color #42b8b0;
@define-color accent_bg_color #42b8b0;
@define-color accent_fg_color #111720;
window { background: #111720; color: #f1f5f9; font-family: "Roboto Flex", "Noto Sans", sans-serif; }
headerbar { background: #1b2632; color: #f1f5f9; border-bottom: 0; box-shadow: none; }
.sql-editor { font-family: monospace; font-size: 12px; }
.sql-editor-region, .sql-results-region { background: #111923; }
.sql-toolbar { background: #1b2632; border-top: 1px solid #3b4b5e; padding: 6px 8px; }
.query-risk-indicator { border-radius: 999px; padding: 2px 8px; }
.query-risk-indicator.status-pending { background: #3a2d13; color: #f0c45d; }
.query-risk-indicator.status-success { background: #12261e; }
.query-error-message { color: #ff7b72; background: #3f1d23; border-radius: 6px; }
.table-edit-toolbar { background: #1b2632; border-top: 1px solid #3b4b5e; padding: 6px 8px; }
.grid-cell-edited { color: #e3b341; background: #3a2d13; border-color: #e3b341; }
button.suggested-action.table-save-ready, button.suggested-action.table-save-ready:hover { background: #3fb950; color: #0d141c; }
.grid-column-selected { background: #1b3539; }
.sql-tab-add { min-width: 28px; min-height: 28px; padding: 4px; }
notebook > header.top > tabs { background: #111720; border-bottom: 1px solid #3a4a5a; padding: 0; }
notebook > header.top > tabs > tab { background: #111720; color: #b9c5d1; padding: 3px 8px; margin: 0; border-radius: 0; border-right: 0; border-bottom: 2px solid transparent; }
notebook > header.top > tabs > tab:hover { background: #1b2733; color: #f1f5f9; }
notebook > header.top > tabs > tab:checked { background: #1b2733; color: #f1f5f9; border-bottom: 2px solid #42b8b0; }
.add-connection-button:hover, .connected-action:hover { background: #2a3948; }
.compact-tab-close:hover { background: #2a3948; }
window.preferences, preferencespage { background: #111720; color: #f1f5f9; }
preferencesgroup { color: #f1f5f9; }
.preferences row { background: #1b2733; color: #f1f5f9; border-color: #3a4a5a; }
.preferences row:hover { background: #2a3948; }
.navigation-rail { background: #1a2531; border-right: 1px solid #3a4a5a; }
.sidebar-panel, .sidebar-stack { background: #1b2733; color: #f1f5f9; }
.sidebar-title, .dim-label, .connection-details { color: #b9c5d1; }
.sidebar-nav-button { color: #b9c5d1; }
.sidebar-nav-button:hover, .table-nav-item:hover { background: #2a3948; color: #f1f5f9; }
button:hover image { color: #94e2d5; }
.sidebar-nav-active { background: transparent; color: #42b8b0; }
.schema-tree-row:hover { background: #2a3948; color: #f1f5f9; }
.schema-tree-row-active, .schema-tree-row-active:hover { background: transparent; color: #42b8b0; }
.schema-tree-row-active label { color: #42b8b0; }
.table-nav-item-selected, .table-nav-item-selected:hover { background: transparent; color: #42b8b0; }
.table-nav-item-selected label { color: #42b8b0; opacity: 1; }
.table-nav-item-selected:hover { background: #35495d; }
.connection-card { background: #202c38; color: #f1f5f9; border: 1px solid #3b4b5a; }
.connection-card-active.connection-color-blue { border-color: #4f8fe8; }
.connection-card-active.connection-color-teal { border-color: #42b8b0; }
.connection-card-active.connection-color-green { border-color: #55b879; }
.connection-card-active.connection-color-yellow { border-color: #e4b84f; }
.connection-card-active.connection-color-orange { border-color: #e58b4f; }
.connection-card-active.connection-color-red { border-color: #df626b; }
.connection-card-active.connection-color-purple { border-color: #a57be8; }
.active-connection-header { background: #263544; color: #f7fafc; border: 1px solid #46596a; }
.active-connection-header.connection-color-blue { border-left-color: #5b9ce6; }
.active-connection-header.connection-color-teal { border-left-color: #4fc4bb; }
.active-connection-header.connection-color-green { border-left-color: #5dcc86; }
.active-connection-header.connection-color-yellow { border-left-color: #f0c45d; }
.active-connection-header.connection-color-orange { border-left-color: #f09a5b; }
.active-connection-header.connection-color-red { border-left-color: #ed737d; }
.active-connection-header.connection-color-purple { border-left-color: #b293f2; }
.table-nav-item { color: #f1f5f9; }
.table-filter-panel { background: #1b2733; border-bottom: 1px solid #3a4a5a; }
.table-filter-row entry, .table-filter-row combobox, entry, combobox { background: #131c26; color: #f1f5f9; border-color: #3a4a5a; }
entry:focus, combobox:focus { border-color: #42b8b0; }
button.grid-header, .grid-header { background: #2a3948; color: #f1f5f9; border-color: #46596a; min-width: 0; padding: 0; }
button.grid-header:hover { background: #35495d; }
.grid-sort-button { min-width: 24px; min-height: 24px; padding: 2px 4px; margin: 0; border-radius: 4px; }
.grid-header-label { min-height: 0; padding: 3px 6px; margin: 0; }
.grid-column-resize-handle { min-width: 6px; background: transparent; }
.grid-column-resize-handle:hover { background: color-mix(in srgb, #42b8b0 22%, transparent); }
.grid-row-header { min-width: 38px; min-height: 28px; padding: 2px 5px; border-radius: 0; color: #b9c5d1; background: #151f2b; font-weight: 400; }
.grid-row-header-selected { color: #42b8b0; background: #20383c; }
.grid-cell { background: #0d141c; color: #f1f5f9; border-color: #3a4a5a; min-width: 48px; padding: 4px 8px; border: 1px solid transparent; }
.grid-cell-inline-editor, .grid-cell-inline-editor:focus { background: #0d141c; color: #f1f5f9; border: 1px solid #3a4a5a; border-radius: 0; box-shadow: inset 0 0 0 2px #5b9ce6; padding: 4px 8px; }
.grid-cell.alternate { background: #151f2b; }
.grid-cell.row-hover, .grid-cell.alternate.row-hover { border-top-color: #42b8b0; border-bottom-color: #42b8b0; }
.grid-cell.grid-row-first.row-hover { border-left-color: #42b8b0; }
.grid-cell.grid-row-last.row-hover { border-right-color: #42b8b0; }
.grid-cell.grid-cell-selected { box-shadow: inset 0 0 0 2px #5b9ce6; }
.grid-cell.row-hover.grid-cell-selected, .grid-cell.alternate.row-hover.grid-cell-selected { box-shadow: inset 0 0 0 2px #5b9ce6; }
.grid-cell.grid-cell-range { background: color-mix(in srgb, #5b9ce6 12%, var(--view-bg-color)); }
.grid-cell.grid-cell-copied { outline: 1px dashed #5b9ce6; outline-offset: -2px; }
.grid-cell.grid-cell-edited { color: #e3b341; background: #3a2d13; border-color: #e3b341; }
.grid-cell.grid-cell-edited.grid-cell-selected { box-shadow: inset 0 0 0 2px #e3b341; }
.grid-cell.null-cell, .dim-label { color: #b9c5d1; }
.grid-cell.truncated-cell, .status-pending { color: #e3b341; }
.grid-cell.decode-error, .status-error { color: #ff7b72; }
.tag-prod { background: #3f1d23; color: #ff7b72; }
.tag-stage { background: #3a2d13; color: #e3b341; }
.tag-local { background: #12261e; color: #3fb950; }
.status-success { color: #3fb950; }
.tag-active { background: #42b8b0; color: #111720; }
.table-context-popover > contents, popover > contents { background: #202c38; color: #f1f5f9; border: 1px solid #42b8b0; }
.table-context-popover.connection-color-blue > contents { border-color: #4f8fe8; }
.table-context-popover.connection-color-teal > contents { border-color: #42b8b0; }
.table-context-popover.connection-color-green > contents { border-color: #55b879; }
.table-context-popover.connection-color-yellow > contents { border-color: #e4b84f; }
.table-context-popover.connection-color-orange > contents { border-color: #e58b4f; }
.table-context-popover.connection-color-red > contents { border-color: #df626b; }
.table-context-popover.connection-color-purple > contents { border-color: #a57be8; }
.table-context-action:hover { background: #35495d; }
.table-context-action image { color: #f1f5f9; opacity: 1; }
.color-option-selected { outline-color: #42b8b0; }
button.suggested-action { background: #42b8b0; color: #111720; }
button.suggested-action:hover { background: #4fc4bb; color: #111720; }
button:focus { outline-color: #42b8b0; }
frame { border-color: #3a4a5a; }

/* Compact data/query tabs; the surrounding header stays transparent. */
notebook.workspace-tabs > header.top { background: transparent; border: 0; box-shadow: none; padding: 0; }
notebook.workspace-tabs > header.top > tabs { background: transparent; border: 0; padding: 0; min-height: 28px; }
notebook.workspace-tabs > header.top > tabs > tab { background: #202b38; padding: 2px 8px; margin: 0 2px 0 0; border: 0; border-radius: 5px 5px 0 0; min-height: 22px; box-shadow: none; }
notebook.workspace-tabs > header.top > tabs > tab:checked { background: #101820; color: #f1f5f9; border: 0; box-shadow: none; font-weight: 600; }
notebook.workspace-tabs > header.top > button.sql-tab-add { background: transparent; border: 1px solid #3a4a5a; border-radius: 999px; padding: 2px; margin: 0 0 0 1px; min-width: 24px; min-height: 24px; box-shadow: none; }
notebook.workspace-tabs > header.top > button.sql-tab-add:hover { background: #202b38; }
"#;

const SQL_CONTROLS_CSS: &str = r#"
/* Completion refilters before GtkSourceView's deferred hide tick. Keep a valid
   surface size even while its last proposal/content is disappearing. */
.sql-editor GtkSourceAssistant.completion { min-width: 220px; min-height: 24px; }
.sql-syntax-diagnostic { color: @dbdeck_error; margin: 2px 8px; font-size: 11px; }
.sql-toolbar { padding: 4px 8px; }
.sql-toolbar button.sql-run-button, .sql-toolbar menubutton.sql-actions-button > button { min-height: 22px; min-width: 20px; padding: 2px 8px; font-size: 12px; }
.sql-actions-popover button { padding: 6px 10px; min-height: 22px; font-weight: 400; }
.table-filter-panel { padding: 4px 0; border-radius: 0; }
.table-filter-panel button, .table-filter-panel entry, .table-filter-panel combobox button { min-height: 22px; padding: 1px 6px; font-size: 12px; border-radius: 4px; }
.table-filter-panel .sql-condition-entry { font-family: monospace; }
.table-filter-panel entry { border: 1px solid @dbdeck_border; box-shadow: none; }
.table-filter-panel entry:focus-within { border-color: @accent_bg_color; box-shadow: none; outline-style: none; }
.table-edit-toolbar { padding: 0; border: 0; background: transparent; }
.table-footer { padding: 4px 6px; border-top: 1px solid alpha(currentColor, 0.12); }
.table-footer button { min-height: 22px; padding: 1px 8px; font-size: 12px; border-radius: 4px; }
.table-footer .table-sql-mode { background: alpha(@accent_bg_color, 0.18); color: @accent_color; border: 1px solid alpha(@accent_bg_color, 0.6); }
.table-footer .table-sql-mode:checked { background: @accent_bg_color; color: @accent_fg_color; }
.table-edit-toolbar button { min-height: 22px; padding: 1px 8px; font-size: 12px; border-radius: 4px; }
.sql-shortcut-hints { opacity: 0.6; margin: 16px; font-size: 12px; }
"#;

const SETTINGS_CSS: &str = r#"
.dbdeck-settings headerbar { min-height: 34px; padding: 3px 6px; }
.dbdeck-settings .settings-heading { font-size: 15px; font-weight: 600; }
.dbdeck-settings .settings-row-title { font-size: 13px; font-weight: 600; }
.dbdeck-settings .settings-description { font-size: 12px; opacity: 0.8; }
.dbdeck-settings .settings-row { padding: 10px 12px; border: 1px solid alpha(currentColor, 0.15); border-radius: 6px; background: alpha(currentColor, 0.025); }
.dbdeck-settings button { min-height: 24px; padding: 2px 8px; border-radius: 5px; font-size: 12px; }
.dbdeck-settings entry, .dbdeck-settings passwordentry { min-height: 24px; padding: 2px 8px; border-radius: 5px; border: 1px solid alpha(currentColor, 0.4); }
.dbdeck-settings entry:focus-within, .dbdeck-settings passwordentry:focus-within { border-color: @accent_bg_color; outline-style: none; box-shadow: none; }
.dbdeck-settings combobox { padding: 0; min-height: 0; border: 0; }
.dbdeck-settings combobox button { min-height: 24px; border: 1px solid alpha(currentColor, 0.3); }
.dbdeck-settings .settings-tabs button { min-height: 24px; padding: 3px 12px; border: 0; border-radius: 5px; box-shadow: none; }
.dbdeck-settings .settings-tabs button:checked { background: alpha(@accent_bg_color, 0.16); color: @accent_color; }
.dbdeck-settings .settings-preview { padding: 14px; border: 1px solid alpha(currentColor, 0.2); border-radius: 6px; }
.dbdeck-settings .settings-preview.tokyo-night { background: #1a1b26; color: #c0caf5; border-color: #414868; }
.dbdeck-settings .settings-preview.mocha { background: #11111b; color: #cdd6f4; }
.dbdeck-settings .settings-preview.graphite { background: #111923; color: #d7e1eb; }
.dbdeck-settings .settings-preview.gruvbox { background: #282828; color: #ebdbb2; border-color: #665c54; }
.dbdeck-settings .settings-preview.nord { background: #2e3440; color: #d8dee9; border-color: #4c566a; }
.dbdeck-settings .settings-preview.rose-pine { background: #191724; color: #e0def4; border-color: #524f67; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noctalia::{Rgb, contrast};

    #[test]
    fn theme_ids_and_preferences_roundtrip() {
        for mode in [
            ThemeMode::System,
            ThemeMode::TokyoNight,
            ThemeMode::Dark,
            ThemeMode::CatppuccinMocha,
            ThemeMode::GruvboxDark,
            ThemeMode::Nord,
            ThemeMode::RosePine,
            ThemeMode::Noctalia,
        ] {
            assert_eq!(ThemeMode::from_id(Some(mode.id())), mode);
            let json = serde_json::to_string(&ThemePreferences { mode }).unwrap();
            assert_eq!(
                serde_json::from_str::<ThemePreferences>(&json)
                    .unwrap()
                    .mode,
                mode
            );
        }
        assert_eq!(
            ThemeMode::from_id(Some("light")),
            ThemeMode::CatppuccinMocha
        );
        assert_eq!(ThemeMode::from_id(None), ThemeMode::CatppuccinMocha);
        assert_eq!(
            ThemeMode::from_id(Some("unknown")),
            ThemeMode::CatppuccinMocha
        );
    }

    #[test]
    fn stored_ids_from_older_versions_keep_their_meaning() {
        for (id, expected) in [
            ("dark", ThemeMode::Dark),
            ("tokyo_night", ThemeMode::TokyoNight),
            ("light", ThemeMode::CatppuccinMocha),
            ("system", ThemeMode::System),
            ("noctalia", ThemeMode::Noctalia),
        ] {
            assert_eq!(ThemeMode::from_id(Some(id)), expected, "from_id {id}");
            let json = format!(r#"{{"mode":"{id}"}}"#);
            let prefs: ThemePreferences = serde_json::from_str(&json).unwrap();
            assert_eq!(prefs.mode, expected, "serde {id}");
        }
    }

    fn rgb(value: &str) -> Rgb {
        Rgb::parse(value).unwrap_or_else(|| panic!("invalid color {value}"))
    }

    fn contrast_of(a: &str, b: &str) -> f64 {
        contrast(rgb(a), rgb(b))
    }

    /// Текст - 4.5, элементы интерфейса - 3.0. Пары из `layout.css`: вкладки на фоне
    /// панели, hover как `alpha(fg, 0.08)` поверх панели, активная вкладка на selection
    /// с акцентной полосой, бейджи на панели.
    fn assert_palette_contrast(name: &str, p: &Palette) {
        let hover = rgb(&p.panel).mix(rgb(&p.fg), 0.08);
        // Subtitle opacity is part of the rendered contrast, not just the palette.
        let css = include_str!("../assets/themes/layout.css");
        let subtitle = css
            .split("label.tab-strip-subtitle {")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        let opacity: f64 = subtitle
            .split("opacity:")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let rendered_subtitle = rgb(&p.panel).mix(rgb(&p.muted), opacity);
        assert!(
            contrast(rendered_subtitle, rgb(&p.panel)) >= 4.5,
            "{name}: rendered tab subtitle"
        );
        for (pair, ratio, min) in [
            ("fg/bg", contrast_of(&p.fg, &p.bg), 4.5),
            ("fg/panel", contrast_of(&p.fg, &p.panel), 4.5),
            ("fg/selection", contrast_of(&p.fg, &p.selection), 4.5),
            ("fg/hover", contrast(rgb(&p.fg), hover), 4.5),
            ("muted/bg", contrast_of(&p.muted, &p.bg), 4.5),
            ("muted/panel", contrast_of(&p.muted, &p.panel), 4.5),
            ("accent/bg", contrast_of(&p.accent, &p.bg), 3.0),
            ("accent/panel", contrast_of(&p.accent, &p.panel), 3.0),
            (
                "accent/selection",
                contrast_of(&p.accent, &p.selection),
                3.0,
            ),
            ("error/bg", contrast_of(&p.error, &p.bg), 3.0),
            ("warning/bg", contrast_of(&p.warning, &p.bg), 3.0),
            ("success/bg", contrast_of(&p.success, &p.bg), 3.0),
        ] {
            assert!(ratio >= min, "{name} {pair}: {ratio:.2} < {min}");
        }
    }

    #[test]
    fn dark_palettes_meet_wcag_aa_contrast() {
        for (name, mode) in [
            ("dark", ThemeMode::Dark),
            ("tokyo_night", ThemeMode::TokyoNight),
            ("catppuccin_mocha", ThemeMode::CatppuccinMocha),
            ("gruvbox_dark", ThemeMode::GruvboxDark),
            ("nord", ThemeMode::Nord),
            ("rose_pine", ThemeMode::RosePine),
        ] {
            assert_palette_contrast(name, &palette(mode));
        }
    }

    #[test]
    fn preset_css_and_schemes_use_only_their_own_palette() {
        let tokyo_only = ["#1a1b26", "#7aa2f7", "#4fc4bb"];
        for (mode, skin) in [
            (ThemeMode::GruvboxDark, GRUVBOX_DARK),
            (ThemeMode::Nord, NORD),
            (ThemeMode::RosePine, ROSE_PINE),
        ] {
            let css = skin.css("test");
            let xml = skin.scheme_xml("dbdeck-test");
            let p = palette(mode);
            assert!(css.contains(&format!("@define-color accent_color {};", p.accent)));
            assert!(xml.contains(&format!("background=\"{}\"", p.bg)));
            for old in tokyo_only {
                assert!(!css.contains(old) && !xml.contains(old), "{mode:?}: {old}");
            }
        }
    }

    #[test]
    fn noctalia_fallback_palette_meets_wcag_aa_contrast() {
        assert_palette_contrast(
            "noctalia_fallback",
            &crate::noctalia::Theme::fallback().palette(),
        );
    }

    #[test]
    fn noctalia_example_palette_meets_wcag_aa_contrast() {
        let colors = crate::noctalia::parse_colors(
            r##"{"mPrimary":"#cba6f7","mSecondary":"#a6c8ff","mTertiary":"#f5c2e7","mError":"#f38ba8","mSurface":"#1c1b22","mOnSurface":"#e6e1ea","mSurfaceVariant":"#2a2831","mOnSurfaceVariant":"#cac4d0","mOutline":"#49454f","mHover":"#3a3745"}"##,
        )
        .unwrap();
        let theme = crate::noctalia::Theme::from_colors(&colors);
        assert_palette_contrast("noctalia_example", &theme.palette());
    }
}

const THEMED_ICONS_CSS: &str = r#"
notebook.workspace-tabs > header.top > tabs > tab:checked { box-shadow: inset 0 0 0 1px @accent_color; }
button:hover image { color: @accent_color; }
button.suggested-action:hover image { color: inherit; }
image.penpot-icon.icon-muted { color: @dbdeck_fg; }
image.penpot-icon.icon-accent { color: @accent_color; }
image.penpot-icon.icon-disabled { color: @dbdeck_muted; opacity: 0.45; }
image.penpot-icon.icon-success,
.connected-action image.icon-success,
.connected-action:hover image.icon-success { color: @dbdeck_success; }
image.penpot-icon.icon-danger { color: @dbdeck_error; }
"#;

const SELECT_CSS: &str = r#"
combobox, dropdown { background: transparent; padding: 0; border: 0; box-shadow: none; }
combobox button.combo, dropdown > button { background: @dbdeck_panel; background-image: none; color: @dbdeck_fg; border: 1px solid @dbdeck_border; box-shadow: none; }
combobox button.combo:hover, dropdown > button:hover { background: mix(@dbdeck_panel, @dbdeck_fg, 0.06); }
combobox button.combo:focus-visible, dropdown > button:focus-visible { border-color: @accent_color; outline: none; box-shadow: none; }
"#;

const CATALOG_TREE_CSS: &str = r#"
.app-header-tabs > header > tabs > tab:checked {
    background: alpha(@accent_bg_color, 0.16);
    box-shadow: inset 0 0 0 1px alpha(@accent_bg_color, 0.65);
}
.connection-card-active { background: alpha(@accent_bg_color, 0.08); }
.database-tree-row { border: 1px solid transparent; border-radius: 4px; }
.database-tree-row-active, .database-tree-row-active:hover {
    background: alpha(@accent_bg_color, 0.16);
    border-color: alpha(@accent_bg_color, 0.65);
    color: @accent_color;
}
.database-tree-row-active label, .database-tree-row-active image { color: @accent_color; }
.tree-row-line { border-radius: 4px; }
.tree-row-action { min-width: 24px; min-height: 22px; padding: 0 4px; margin-left: 2px; border-radius: 4px; opacity: 0.45; transition: opacity 120ms ease-out; }
.tree-row-line:hover .tree-row-action, .tree-row-action:focus-visible { opacity: 0.85; }
.tree-row-action:hover { opacity: 1; background: alpha(@accent_bg_color, 0.16); }
.active-connection-header {
    background: alpha(@accent_bg_color, 0.10);
    border: 1px solid alpha(@accent_bg_color, 0.55);
    border-left-width: 3px;
}
"#;
