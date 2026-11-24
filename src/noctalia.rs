//! Тема Noctalia: цвета берутся из `~/.config/noctalia/colors.json`.
//!
//! Любое значение цвета проходит через строгий тип [`Rgb`], поэтому в CSS и
//! схему подсветки попадают только строки вида `#rrggbb`.

use std::{
    cell::RefCell,
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
    time::Duration,
};

use relm4::gtk::{gio, glib, prelude::*};

use crate::theme::Palette;

const COLORS_FILE: &str = "colors.json";
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const RELOAD_DELAY: Duration = Duration::from_millis(250);
const TEXT_CONTRAST: f64 = 4.5;
const ACCENT_CONTRAST: f64 = 3.0;

/// Последняя успешно разобранная тема; при ошибке чтения остаётся прежней.
static CACHE: Mutex<Option<Theme>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    const BLACK: Self = Self::new(0, 0, 0);
    const WHITE: Self = Self::new(255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Цвет из литерала `0xRRGGBB`.
    pub const fn hex(value: u32) -> Self {
        Self::new((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// Принимает только `#RRGGBB` и `#RRGGBBAA`; альфа-канал отбрасывается.
    pub fn parse(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('#')?;
        if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Self::new(channel(0)?, channel(2)?, channel(4)?))
    }

    /// Относительная яркость по WCAG 2.x.
    pub fn luminance(self) -> f64 {
        let linear = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    pub fn mix(self, other: Self, amount: f64) -> Self {
        let blend = |a: u8, b: u8| {
            (f64::from(a) + (f64::from(b) - f64::from(a)) * amount)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self::new(
            blend(self.r, other.r),
            blend(self.g, other.g),
            blend(self.b, other.b),
        )
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Коэффициент контраста WCAG 2.x, от 1.0 до 21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Сдвигает `color` к белому или чёрному (что контрастнее фону), пока контраст не достигнет `min`.
pub fn ensure_contrast(color: Rgb, bg: Rgb, min: f64) -> Rgb {
    if contrast(color, bg) >= min {
        return color;
    }
    let end = if contrast(Rgb::WHITE, bg) >= contrast(Rgb::BLACK, bg) {
        Rgb::WHITE
    } else {
        Rgb::BLACK
    };
    (1..=50)
        .map(|step| color.mix(end, f64::from(step) / 50.0))
        .find(|candidate| contrast(*candidate, bg) >= min)
        .unwrap_or(end)
}

#[derive(Clone, Debug, Default)]
pub struct NoctaliaColors(HashMap<String, Rgb>);

impl NoctaliaColors {
    fn get(&self, key: &str) -> Option<Rgb> {
        self.0.get(key).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `None` - не JSON-объект. Значения не в формате `#RRGGBB[AA]` пропускаются.
pub fn parse_colors(json: &str) -> Option<NoctaliaColors> {
    let serde_json::Value::Object(map) = serde_json::from_str(json).ok()? else {
        return None;
    };
    Some(NoctaliaColors(
        map.iter()
            .filter_map(|(key, value)| Some((key.clone(), Rgb::parse(value.as_str()?)?)))
            .collect(),
    ))
}

/// `None` - файла нет, он слишком большой или не разбирается.
pub fn load_from(path: &Path) -> Option<NoctaliaColors> {
    if fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    parse_colors(&fs::read_to_string(path).ok()?)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub panel: Rgb,
    pub alternate: Rgb,
    pub border: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub selection: Rgb,
    pub hover: Rgb,
    pub secondary: Rgb,
    pub tertiary: Rgb,
}

impl Theme {
    /// Tokyo Night: используется, пока Noctalia недоступна, и для недостающих ключей.
    pub const fn fallback() -> Self {
        Self {
            bg: Rgb::new(0x1a, 0x1b, 0x26),
            fg: Rgb::new(0xc0, 0xca, 0xf5),
            panel: Rgb::new(0x1f, 0x23, 0x35),
            alternate: Rgb::new(0x16, 0x16, 0x1e),
            border: Rgb::new(0x41, 0x48, 0x68),
            muted: Rgb::new(0x9a, 0xa5, 0xce),
            accent: Rgb::new(0x7a, 0xa2, 0xf7),
            success: Rgb::new(0x9e, 0xce, 0x6a),
            warning: Rgb::new(0xe0, 0xaf, 0x68),
            error: Rgb::new(0xf7, 0x76, 0x8e),
            selection: Rgb::new(0x29, 0x2e, 0x42),
            hover: Rgb::new(0x24, 0x28, 0x3b),
            secondary: Rgb::new(0xbb, 0x9a, 0xf7),
            tertiary: Rgb::new(0x7d, 0xcf, 0xff),
        }
    }

    pub fn from_colors(colors: &NoctaliaColors) -> Self {
        let fb = Self::fallback();
        if colors.is_empty() {
            return fb;
        }
        let bg = colors.get("mSurface").unwrap_or(fb.bg);
        let panel = colors.get("mSurfaceVariant").unwrap_or(fb.panel);
        let readable = |color: Rgb| {
            ensure_contrast(
                ensure_contrast(color, bg, TEXT_CONTRAST),
                panel,
                TEXT_CONTRAST,
            )
        };
        let fg = readable(colors.get("mOnSurface").unwrap_or(fb.fg));
        let muted = readable(colors.get("mOnSurfaceVariant").unwrap_or(fb.muted));
        let accent = colors.get("mPrimary").unwrap_or(fb.accent);
        let alternate = match colors.get("mSurface") {
            Some(surface) if surface.luminance() < 0.4 => surface.mix(Rgb::BLACK, 0.2),
            Some(surface) => surface.mix(Rgb::BLACK, 0.04),
            None => fb.alternate,
        };
        let hover = colors
            .get("mHover")
            .filter(|hover| contrast(fg, *hover) >= TEXT_CONTRAST)
            .unwrap_or_else(|| panel.mix(fg, 0.08));
        Self {
            bg,
            fg,
            panel,
            alternate,
            border: colors.get("mOutline").unwrap_or(fb.border),
            muted,
            accent,
            success: ensure_contrast(fb.success, bg, ACCENT_CONTRAST),
            warning: ensure_contrast(fb.warning, bg, ACCENT_CONTRAST),
            error: colors.get("mError").unwrap_or(fb.error),
            selection: panel.mix(accent, 0.2),
            hover,
            secondary: colors.get("mSecondary").unwrap_or(fb.secondary),
            tertiary: colors.get("mTertiary").unwrap_or(fb.tertiary),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.bg.luminance() < 0.4
    }

    pub fn palette(&self) -> Palette {
        Palette {
            bg: self.bg.to_string(),
            fg: self.fg.to_string(),
            panel: self.panel.to_string(),
            alternate: self.alternate.to_string(),
            border: self.border.to_string(),
            muted: self.muted.to_string(),
            accent: self.accent.to_string(),
            success: self.success.to_string(),
            warning: self.warning.to_string(),
            error: self.error.to_string(),
            selection: self.selection.to_string(),
        }
    }

    /// Роли, которых нет в `colors.json`, выводятся из основных цветов.
    pub fn skin(&self) -> Skin {
        let tint = |color: Rgb| self.bg.mix(color, 0.14);
        Skin {
            theme: *self,
            number: self.warning,
            function: self.accent,
            type_color: self.accent.mix(self.fg, 0.3),
            highlight: self.accent.mix(self.fg, 0.25),
            strong_hover: self.selection,
            warning_tint: tint(self.warning),
            success_tint: tint(self.success),
            error_tint: tint(self.error),
        }
    }

    pub fn css(&self) -> String {
        self.skin()
            .css("Noctalia palette generated from ~/.config/noctalia/colors.json")
    }

    pub fn scheme_xml(&self) -> String {
        self.skin().scheme_xml("dbdeck-noctalia")
    }
}

/// Строки шаблона, где Tokyo Night использует `selection`, а Nord - более светлый тон.
const STRONG_HOVER_RULES: [&str; 2] = ["button.grid-header:hover", ".table-context-action:hover"];

/// Полный набор ролей темы: [`Theme`] плюс цвета, которые Noctalia выводит сама,
/// а встроенные темы (Gruvbox, Nord, Rose Pine) задают явно.
#[derive(Clone, Copy, Debug)]
pub struct Skin {
    pub theme: Theme,
    pub number: Rgb,
    pub function: Rgb,
    pub type_color: Rgb,
    /// Второй акцент: ховер основной кнопки и цвет бирюзового соединения.
    pub highlight: Rgb,
    /// Фон hover у заголовка колонки и пункта контекстного меню.
    pub strong_hover: Rgb,
    pub warning_tint: Rgb,
    pub success_tint: Rgb,
    pub error_tint: Rgb,
}

impl Skin {
    /// CSS строится из шаблона Tokyo Night: каждый его цвет заменяется ролью палитры.
    pub fn css(&self, title: &str) -> String {
        let t = &self.theme;
        let template = include_str!("../assets/themes/tokyo-night.css");
        let body = template.split_once('\n').map_or(template, |(_, rest)| rest);
        let roles = [
            ("#1a1b26", t.bg),
            ("#c0caf5", t.fg),
            ("#16161e", t.alternate),
            ("#1f2335", t.panel),
            ("#414868", t.border),
            ("#9aa5ce", t.muted),
            ("#7aa2f7", t.accent),
            ("#9ece6a", t.success),
            ("#e0af68", t.warning),
            ("#f7768e", t.error),
            ("#292e42", t.selection),
            ("#24283b", t.hover),
            ("#352c29", self.warning_tint),
            ("#23312f", self.success_tint),
            ("#3b2535", self.error_tint),
            ("#4fc4bb", self.highlight),
        ];
        let strong_roles = roles.map(|(token, color)| {
            let color = if token == "#292e42" {
                self.strong_hover
            } else {
                color
            };
            (token, color)
        });
        let colored: String = body
            .split_inclusive('\n')
            .map(|line| {
                let line_roles = if STRONG_HOVER_RULES.iter().any(|rule| line.starts_with(rule)) {
                    &strong_roles
                } else {
                    &roles
                };
                recolor(line, line_roles)
            })
            .collect();
        format!("/* {title} */\n{colored}")
    }

    pub fn scheme_xml(&self, id: &str) -> String {
        let t = &self.theme;
        let parent = if t.is_dark() {
            "Adwaita-dark"
        } else {
            "Adwaita"
        };
        format!(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<style-scheme id="{id}" name="{id}" version="1.0" parent-scheme="{parent}">
  <style name="text" foreground="{fg}" background="{bg}"/>
  <style name="selection" foreground="{fg}" background="{sel}"/>
  <style name="selection-unfocused" foreground="{fg}" background="{sel}"/>
  <style name="cursor" foreground="{fg}"/>
  <style name="current-line" background="{panel}"/>
  <style name="line-numbers" foreground="{muted}" background="{alt}"/>
  <style name="current-line-number" foreground="{fg}" background="{panel}"/>
  <style name="def:keyword" foreground="{secondary}" bold="true"/>
  <style name="def:statement" foreground="{secondary}"/>
  <style name="def:string" foreground="{tertiary}"/>
  <style name="def:comment" foreground="{muted}" italic="true"/>
  <style name="def:decimal" foreground="{number}"/>
  <style name="def:floating-point" foreground="{number}"/>
  <style name="def:boolean" foreground="{number}"/>
  <style name="def:function" foreground="{function}"/>
  <style name="def:type" foreground="{type_color}"/>
  <style name="def:preprocessor" foreground="{secondary}"/>
  <style name="bracket-match" foreground="{fg}" background="{sel}"/>
</style-scheme>
"##,
            fg = t.fg,
            bg = t.bg,
            sel = t.selection,
            panel = t.panel,
            muted = t.muted,
            alt = t.alternate,
            secondary = t.secondary,
            tertiary = t.tertiary,
            number = self.number,
            function = self.function,
            type_color = self.type_color,
        )
    }

    /// Цвета для превью темы: текст, ключевое слово, строка, число, функция, тип.
    pub fn preview_colors(&self) -> [String; 6] {
        let t = &self.theme;
        [
            t.fg,
            t.secondary,
            t.tertiary,
            self.number,
            self.function,
            self.type_color,
        ]
        .map(|color| color.to_string())
    }
}

/// Один проход по шаблону, чтобы подставленный цвет не заменялся повторно.
fn recolor(template: &str, roles: &[(&str, Rgb)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find('#') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let replacement = tail
            .get(..7)
            .and_then(|token| roles.iter().find(|(name, _)| *name == token));
        match replacement {
            Some((_, color)) => {
                out.push_str(&color.to_string());
                rest = &tail[7..];
            }
            None => {
                out.push('#');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn config_dir() -> Option<PathBuf> {
    Some(directories::BaseDirs::new()?.config_dir().join("noctalia"))
}

fn colors_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(COLORS_FILE))
}

/// Текущая тема Noctalia: кеш, а при первом обращении - файл или Tokyo Night.
pub fn current() -> Theme {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache.get_or_insert_with(|| {
        colors_path()
            .and_then(|path| load_from(&path))
            .map_or_else(Theme::fallback, |colors| Theme::from_colors(&colors))
    })
}

/// Перечитывает файл; `true`, если тема изменилась. При ошибке остаётся последняя валидная.
fn reload_from(path: &Path) -> bool {
    let Some(colors) = load_from(path) else {
        return false;
    };
    let next = Theme::from_colors(&colors);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let changed = cache.as_ref() != Some(&next);
    *cache = Some(next);
    changed
}

fn is_colors_file(file: &gio::File) -> bool {
    file.basename()
        .is_some_and(|name| name.to_str() == Some(COLORS_FILE))
}

/// Следит за каталогом (редакторы и Noctalia заменяют файл через rename), `on_change`
/// вызывается в главном потоке после паузы и только если цвета действительно изменились.
/// Монитор нужно держать живым.
pub fn watch(on_change: impl Fn() + 'static) -> Option<gio::FileMonitor> {
    watch_dir(&config_dir()?, on_change)
}

fn watch_dir(dir: &Path, on_change: impl Fn() + 'static) -> Option<gio::FileMonitor> {
    let colors_file = dir.join(COLORS_FILE);
    let monitor = gio::File::for_path(dir)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        .map_err(|error| eprintln!("Не удалось следить за темой Noctalia: {error}"))
        .ok()?;
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let on_change = Rc::new(on_change);
    monitor.connect_changed(move |_, file, other, event| {
        use gio::FileMonitorEvent as Event;
        if !matches!(
            event,
            Event::Changed
                | Event::ChangesDoneHint
                | Event::Created
                | Event::Renamed
                | Event::MovedIn
        ) {
            return;
        }
        if !is_colors_file(file) && !other.is_some_and(is_colors_file) {
            return;
        }
        if let Some(previous) = pending.borrow_mut().take() {
            previous.remove();
        }
        let slot = Rc::clone(&pending);
        let on_change = Rc::clone(&on_change);
        let colors_file = colors_file.clone();
        let id = glib::timeout_add_local_once(RELOAD_DELAY, move || {
            slot.borrow_mut().take();
            if reload_from(&colors_file) {
                on_change();
            }
        });
        *pending.borrow_mut() = Some(id);
    });
    Some(monitor)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        process,
        sync::atomic::{AtomicU32, Ordering},
        time::Instant,
    };

    use super::*;

    const EXAMPLE: &str = r##"{
        "mPrimary": "#cba6f7", "mOnPrimary": "#1c1b22",
        "mSecondary": "#a6c8ff", "mOnSecondary": "#1c1b22",
        "mTertiary": "#f5c2e7", "mOnTertiary": "#1c1b22",
        "mError": "#f38ba8", "mOnError": "#1c1b22",
        "mSurface": "#1c1b22", "mOnSurface": "#e6e1ea",
        "mSurfaceVariant": "#2a2831", "mOnSurfaceVariant": "#cac4d0",
        "mOutline": "#49454f", "mShadow": "#000000aa",
        "mHover": "#3a3745", "mOnHover": "#e6e1ea"
    }"##;

    fn temp_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dbdeck-noctalia-test-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn theme_from(json: &str) -> Theme {
        Theme::from_colors(&parse_colors(json).unwrap())
    }

    fn assert_wcag(theme: &Theme) {
        for (name, a, b, min) in [
            ("fg/bg", theme.fg, theme.bg, 4.5),
            ("fg/panel", theme.fg, theme.panel, 4.5),
            ("muted/bg", theme.muted, theme.bg, 4.5),
            ("accent/bg", theme.accent, theme.bg, 3.0),
            ("error/bg", theme.error, theme.bg, 3.0),
            ("warning/bg", theme.warning, theme.bg, 3.0),
            ("success/bg", theme.success, theme.bg, 3.0),
        ] {
            let ratio = contrast(a, b);
            assert!(ratio >= min, "{name}: {ratio:.2} < {min}");
        }
    }

    #[test]
    fn rgb_accepts_only_hex_colors() {
        assert_eq!(Rgb::parse("#1a2B3c"), Some(Rgb::new(0x1a, 0x2b, 0x3c)));
        assert_eq!(Rgb::parse("#1a2b3cff"), Some(Rgb::new(0x1a, 0x2b, 0x3c)));
        for bad in [
            "",
            "#",
            "#fff",
            "1a2b3c",
            "#1a2b3",
            "#1a2b3c4",
            "#gggggg",
            "red; } * {",
            "#é2b3c",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(Rgb::BLACK, Rgb::WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast(Rgb::WHITE, Rgb::WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn example_colors_json_is_mapped_and_readable() {
        let theme = theme_from(EXAMPLE);
        assert_eq!(theme.accent, Rgb::new(0xcb, 0xa6, 0xf7));
        assert_eq!(theme.bg, Rgb::new(0x1c, 0x1b, 0x22));
        assert_eq!(theme.hover, Rgb::new(0x3a, 0x37, 0x45));
        assert_wcag(&theme);
    }

    #[test]
    fn fallback_theme_is_readable() {
        assert_wcag(&Theme::fallback());
    }

    #[test]
    fn incomplete_file_takes_missing_keys_from_tokyo_night() {
        let theme = theme_from(r##"{"mPrimary": "#ff0000"}"##);
        let fallback = Theme::fallback();
        assert_eq!(theme.accent, Rgb::new(255, 0, 0));
        assert_eq!(theme.bg, fallback.bg);
        assert_eq!(theme.error, fallback.error);
    }

    #[test]
    fn empty_object_gives_exact_fallback() {
        assert_eq!(theme_from("{}"), Theme::fallback());
    }

    #[test]
    fn low_contrast_text_is_lightened_but_background_and_accent_stay() {
        let theme = theme_from(
            r##"{"mSurface": "#222222", "mOnSurface": "#333333",
                 "mOnSurfaceVariant": "#2a2a2a", "mPrimary": "#242424"}"##,
        );
        assert_eq!(theme.bg, Rgb::new(0x22, 0x22, 0x22));
        assert_eq!(theme.accent, Rgb::new(0x24, 0x24, 0x24));
        assert!(contrast(theme.fg, theme.bg) >= 4.5);
        assert!(contrast(theme.muted, theme.bg) >= 4.5);
        assert!(theme.fg.luminance() > Rgb::new(0x33, 0x33, 0x33).luminance());
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(parse_colors("{not json").is_none());
        assert!(parse_colors("[1, 2]").is_none());
    }

    #[test]
    fn non_hex_value_is_ignored_and_never_reaches_css() {
        let theme =
            theme_from(r##"{"mPrimary": "red; } * { display: none", "mSurface": "#101010"}"##);
        assert_eq!(theme.accent, Theme::fallback().accent);
        let css = theme.css();
        assert!(!css.contains("display: none"));
        assert!(!theme.scheme_xml().contains("display"));
    }

    #[test]
    fn load_from_reads_valid_file_and_rejects_missing_or_broken() {
        let dir = temp_dir();
        let good = dir.join("good.json");
        let broken = dir.join("broken.json");
        fs::write(&good, EXAMPLE).unwrap();
        fs::write(&broken, "{").unwrap();
        assert!(load_from(&good).is_some_and(|colors| !colors.is_empty()));
        assert!(load_from(&broken).is_none());
        assert!(load_from(&dir.join("missing.json")).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn generated_css_keeps_tokyo_selectors_and_uses_only_palette_colors() {
        let theme = theme_from(EXAMPLE);
        let css = theme.css();
        assert!(css.contains(".grid-cell.grid-cell-selected"));
        assert!(css.contains("@define-color accent_color #cba6f7;"));
        for old in ["#1a1b26", "#c0caf5", "#7aa2f7"] {
            assert!(!css.contains(old), "{old} left in generated CSS");
        }
        assert!(theme.scheme_xml().contains("dbdeck-noctalia"));
    }

    /// Крутит главный цикл GLib, пока условие не выполнится; ожидание ограничено.
    fn pump_until(context: &glib::MainContext, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            if !context.iteration(false) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        true
    }

    #[test]
    fn watch_reports_in_place_rewrite_and_rename_over_the_file() {
        let dir = temp_dir();
        let file = dir.join(COLORS_FILE);
        fs::write(&file, r##"{"mPrimary": "#111111"}"##).unwrap();
        let calls = Rc::new(Cell::new(0u32));
        let counter = Rc::clone(&calls);
        let context = glib::MainContext::default();
        let _owner = context.acquire().unwrap();
        let _monitor = watch_dir(&dir, move || counter.set(counter.get() + 1)).unwrap();

        fs::write(&file, r##"{"mPrimary": "#ff0000"}"##).unwrap();
        assert!(
            pump_until(&context, || calls.get() == 1),
            "перезапись файла не замечена"
        );

        let staged = dir.join("colors.json.tmp");
        fs::write(&staged, r##"{"mPrimary": "#00ff00"}"##).unwrap();
        fs::rename(&staged, &file).unwrap();
        assert!(
            pump_until(&context, || calls.get() == 2),
            "замена файла через rename не замечена"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
