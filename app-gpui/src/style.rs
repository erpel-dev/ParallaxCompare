//! Colours, fonts and small shared pieces of the views. The palette follows the
//! Svelte app's light theme, so the two front ends are easy to compare.

use gpui::{
    App, Div, ElementId, Font, FontFeatures, Global, Hsla, SharedString, Stateful, div, font, prelude::*, px, rgb,
};
use parallax_core::compare::Status;

pub const ROW: f32 = 22.0;
pub const TEXT_ROW: f32 = 20.0;

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn bg() -> Hsla {
    c(0xffffff)
}
pub fn chrome() -> Hsla {
    c(0xf3f4f7)
}
pub fn line() -> Hsla {
    c(0xdfe3ea)
}
pub fn fg() -> Hsla {
    c(0x1b2230)
}
pub fn muted() -> Hsla {
    c(0x6b7383)
}
pub fn faint() -> Hsla {
    c(0x9aa3b2)
}
pub fn accent() -> Hsla {
    c(0x2f5fd0)
}
pub fn accent_bg() -> Hsla {
    c(0xdde7fb)
}
pub fn hover() -> Hsla {
    c(0xf1f4fa)
}

pub fn status_color(status: Status) -> Hsla {
    match status {
        Status::Same => fg(),
        Status::Similar => c(0x9a6a00),
        Status::Different => c(0xc0392b),
        Status::LeftOnly => c(0x7b3fbf),
        Status::RightOnly => c(0x0f7c8c),
        Status::Error => c(0xb00020),
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
        .child(div().px_1().rounded(px(3.)).bg(chrome()).text_color(muted()).child(group_digits(count)));
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
