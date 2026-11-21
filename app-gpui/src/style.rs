//! Colours, fonts and small shared pieces of the views. The palettes follow the
//! Svelte app's light and dark themes, so the two front ends are easy to compare.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui::{
    App, Div, ElementId, Font, FontFeatures, Global, Hsla, SharedString, Stateful, WindowAppearance, div, font,
    prelude::*, px, rgb,
};
use parallax_core::compare::Status;
use parallax_core::highlight::ThemeKind;

static ZOOM: AtomicU32 = AtomicU32::new(100);

/// The text size of file contents and rows, in percent (Ctrl+= / Ctrl+- / Ctrl+0).
pub fn zoom() -> u32 {
    ZOOM.load(Ordering::Relaxed)
}

/// Sets the text size, kept between 60 % and 200 %.
pub fn set_zoom(percent: u32) {
    ZOOM.store(percent.clamp(60, 200), Ordering::Relaxed);
}

fn zoomed(size: f32) -> f32 {
    (size * zoom() as f32 / 100.0).round()
}

/// Height of a folder row.
pub fn row() -> f32 {
    zoomed(22.0)
}

/// Height of a text or merge row.
pub fn text_row() -> f32 {
    zoomed(20.0)
}

/// Size of file contents and folder rows.
pub fn code_size() -> f32 {
    zoomed(13.0)
}

static DARK: AtomicBool = AtomicBool::new(false);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

/// The palette that matches the system's appearance.
pub fn follow(appearance: WindowAppearance) {
    set_dark(matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark));
}

/// The syntax colours for the current palette.
pub fn syntax_theme() -> ThemeKind {
    if is_dark() { ThemeKind::Dark } else { ThemeKind::Light }
}

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn pick(light: u32, dark: u32) -> Hsla {
    c(if is_dark() { dark } else { light })
}

pub fn bg() -> Hsla {
    pick(0xffffff, 0x1b1f27)
}
pub fn chrome() -> Hsla {
    pick(0xf3f4f7, 0x232833)
}
pub fn line() -> Hsla {
    pick(0xdfe3ea, 0x343b48)
}
pub fn fg() -> Hsla {
    pick(0x1b2230, 0xdde3ec)
}
pub fn muted() -> Hsla {
    pick(0x6b7383, 0x9aa3b4)
}
pub fn faint() -> Hsla {
    pick(0x9aa3b2, 0x667084)
}
pub fn accent() -> Hsla {
    pick(0x2f5fd0, 0x7ea2ff)
}
pub fn accent_bg() -> Hsla {
    pick(0xdde7fb, 0x2a3a5c)
}
pub fn hover() -> Hsla {
    pick(0xf1f4fa, 0x2a303c)
}
pub fn overlay() -> Hsla {
    pick(0xffffff, 0x262c37)
}

/// Row colours of the text view: line background and changed characters.
pub fn changed() -> (Hsla, Hsla) {
    (pick(0xfff3d6, 0x3a3420), pick(0xf6c978, 0x7a6224))
}
pub fn removed() -> (Hsla, Hsla) {
    (pick(0xfde4e1, 0x40262a), pick(0xf4aaa3, 0x8a3c40))
}
pub fn added() -> (Hsla, Hsla) {
    (pick(0xe2f4e4, 0x1f3a2a), pick(0x9fd7a8, 0x2f6e48))
}
pub fn minor() -> (Hsla, Hsla) {
    (pick(0xeef3fb, 0x232d3d), pick(0xd6e2f7, 0x34476a))
}
pub fn search_hit() -> Hsla {
    pick(0xffe066, 0x8a7414)
}
pub fn selection() -> Hsla {
    pick(0xcfdcf7, 0x30436a)
}

pub fn status_color(status: Status) -> Hsla {
    match status {
        Status::Same => fg(),
        Status::Similar => pick(0x9a6a00, 0xe0b24a),
        Status::Different => pick(0xc0392b, 0xff8a78),
        Status::LeftOnly => pick(0x7b3fbf, 0xc49bff),
        Status::RightOnly => pick(0x0f7c8c, 0x55c8d6),
        Status::Error => pick(0xb00020, 0xff6b81),
    }
}

pub fn status_glyph(status: Status) -> &'static str {
    match status {
        Status::Same => "=",
        Status::Similar => "≈",
        Status::Different => "≠",
        Status::LeftOnly => "◀",
        Status::RightOnly => "▶",
        Status::Error => "!",
    }
}

/// The fonts found on this system: one for the interface, one monospaced.
pub struct Fonts {
    pub ui: SharedString,
    pub mono: SharedString,
}

impl Global for Fonts {}

pub fn init(cx: &mut App) {
    let names = cx.text_system().all_font_names();
    let pick = |wanted: &[&str], fallback: &str| -> SharedString {
        wanted
            .iter()
            .find(|w| names.iter().any(|n| n == *w))
            .map_or_else(|| fallback.to_string().into(), |w| w.to_string().into())
    };
    let ui = pick(&["Segoe UI", "Inter", "Cantarell", "Noto Sans", "Ubuntu", "DejaVu Sans"], ".SystemUIFont");
    let mono = pick(
        &["Cascadia Mono", "Consolas", "JetBrains Mono", "DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono"],
        "monospace",
    );
    cx.set_global(Fonts { ui, mono });
}

pub fn fonts(cx: &App) -> &Fonts {
    cx.global::<Fonts>()
}

/// The monospaced font for file contents, without ligatures: a compare view
/// shows `===` or `->` as the characters they are.
pub fn mono(cx: &App) -> Font {
    Font { features: FontFeatures::disable_ligatures(), ..font(fonts(cx).mono.clone()) }
}

/// A toolbar button; `enabled` greys it out and the caller skips its click handler.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    let base = div()
        .id(id)
        .px_2()
        .py(px(3.))
        .rounded(px(5.))
        .border_1()
        .border_color(line())
        .bg(bg())
        .text_size(px(13.))
        .child(label.into());
    if enabled {
        base.text_color(fg()).cursor_pointer().hover(|s| s.bg(hover()).border_color(faint()))
    } else {
        base.text_color(faint())
    }
}

/// A filter chip with a count, as in the Svelte app's filter bar.
pub fn chip(id: impl Into<ElementId>, label: &str, count: u64, dot: Option<Hsla>, active: bool) -> Stateful<Div> {
    chip_with(id, label, Some(count), dot, active)
}

/// An on/off chip without a count.
pub fn toggle(id: impl Into<ElementId>, label: &str, dot: Option<Hsla>, active: bool) -> Stateful<Div> {
    chip_with(id, label, None, dot, active)
}

fn chip_with(
    id: impl Into<ElementId>,
    label: &str,
    count: Option<u64>,
    dot: Option<Hsla>,
    active: bool,
) -> Stateful<Div> {
    let chip = div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py(px(2.))
        .rounded(px(5.))
        .border_1()
        .text_size(px(12.5))
        .cursor_pointer()
        .when_some(dot, |d, color| d.child(div().size(px(7.)).rounded_full().bg(color)))
        .child(label.to_string())
        .when_some(count, |d, count| {
            d.child(div().px_1().rounded(px(3.)).bg(chrome()).text_color(muted()).child(group_digits(count)))
        });
    if active {
        chip.border_color(accent()).bg(accent_bg()).text_color(fg())
    } else {
        chip.border_color(line()).bg(bg()).text_color(fg()).hover(|s| s.bg(hover()))
    }
}

pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `YYYY-MM-DD HH:MM` (UTC) for seconds since the Unix epoch.
pub fn date_time(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_sizes_and_dates() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(1_000_000), "1,000,000");
        assert_eq!(group_digits(39_548), "39,548");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(date_time(0), "1970-01-01 00:00");
        assert_eq!(date_time(1_795_235_000), "2026-11-21 04:23");
        assert_eq!(date_time(951_782_400), "2000-02-29 00:00");
    }
}
