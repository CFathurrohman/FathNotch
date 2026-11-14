//! Color schemes and accents (Settings → Appearance). Solid colors only.

use crate::gfx::{color, with_alpha};
use crate::services::settings;
use crate::ui::Theme;

pub struct Scheme {
    pub id: &'static str,
    pub name: &'static str,
    pub pill: u32,
    pub surface: u32,
    pub surface2: u32,
    pub hover: u32,
    pub text: u32,
    pub subtext: u32,
    pub light: bool,
    pub accent: &'static str,
}

const fn s(
    id: &'static str,
    name: &'static str,
    pill: u32,
    surface: u32,
    surface2: u32,
    hover: u32,
    text: u32,
    subtext: u32,
    light: bool,
    accent: &'static str,
) -> Scheme {
    Scheme { id, name, pill, surface, surface2, hover, text, subtext, light, accent }
}

pub const SCHEMES: &[Scheme] = &[
    s("midnight", "Midnight", 0x000000, 0x111318, 0x1A1D24, 0x262A33, 0xF4F6FA, 0x8B91A1, false, "snow"),
    s("graphite", "Graphite", 0x1B1B1D, 0x242427, 0x2D2D31, 0x38383D, 0xF2F2F4, 0x94949A, false, "blue"),
    s("slate", "Slate", 0x161A20, 0x1F242C, 0x283039, 0x333C47, 0xEEF1F5, 0x8F99A6, false, "sky"),
    s("nord", "Nord", 0x1A1F29, 0x232A36, 0x2C3442, 0x38414F, 0xECEFF4, 0x8E99AB, false, "teal"),
    s("ocean", "Ocean", 0x07141F, 0x0E1E2B, 0x152837, 0x1F3445, 0xE6F1F8, 0x86A0B3, false, "sky"),
    s("forest", "Forest", 0x0F1712, 0x17211B, 0x1F2B23, 0x29372E, 0xE9F2EC, 0x8BA295, false, "mint"),
    s("plum", "Plum", 0x170F1A, 0x211725, 0x2B1F30, 0x372A3D, 0xF4ECF6, 0xA592AB, false, "lavender"),
    s("mocha", "Mocha", 0x1C1714, 0x26201C, 0x302823, 0x3C332C, 0xF4EDE6, 0xA39589, false, "amber"),
    s("rosewood", "Rosewood", 0x1C1012, 0x27181B, 0x322024, 0x3E2A2F, 0xF7ECEE, 0xAE9196, false, "rose"),
    s("aurora", "Aurora", 0x04131A, 0x0B1E24, 0x12292F, 0x1B353B, 0xE8F6F5, 0x86A9A8, false, "mint"),
    s("nebula", "Nebula", 0x0B0718, 0x161026, 0x201833, 0x2B2241, 0xF1ECFA, 0x9C92B3, false, "lavender"),
    s("sunset", "Sunset", 0x130A12, 0x20121A, 0x2B1922, 0x38222C, 0xFAEEEB, 0xB0928F, false, "rose"),
    s("abyss", "Abyss", 0x02060F, 0x0A1220, 0x111B2C, 0x1A2639, 0xE9EEF7, 0x8995A8, false, "blue"),
    s("daylight", "Daylight", 0xFFFFFF, 0xF3F3F5, 0xE9E9EC, 0xDCDCE1, 0x1C1C1E, 0x6E6E75, true, "blue"),
    s("paper", "Paper", 0xFBF8F3, 0xF2EDE4, 0xE8E1D5, 0xDDD4C5, 0x24201A, 0x776E61, true, "orange"),
    s("frost", "Frost", 0xF6F9FC, 0xEAF0F6, 0xDFE7F0, 0xD2DCE8, 0x18212B, 0x66727F, true, "indigo"),
    s("dawn", "Dawn", 0xFFF7F0, 0xF5EDE8, 0xECE2DD, 0xE0D4CF, 0x241C1A, 0x7A6B66, true, "pink"),
];

pub const ACCENTS: &[(&str, &str, u32)] = &[
    ("orange", "Orange", 0xFF8A3D),
    ("amber", "Amber", 0xFFC233),
    ("lime", "Lime", 0xA3E635),
    ("green", "Green", 0x32D74B),
    ("mint", "Mint", 0x34D3A0),
    ("teal", "Teal", 0x3CD3E0),
    ("sky", "Sky", 0x38BDF8),
    ("blue", "Blue", 0x0A84FF),
    ("indigo", "Indigo", 0x6E6CF0),
    ("lavender", "Lavender", 0xA78BFA),
    ("purple", "Purple", 0xBF5AF2),
    ("pink", "Pink", 0xFF4F79),
    ("rose", "Rose", 0xFB7185),
    ("red", "Red", 0xFF453A),
    ("snow", "Snow", 0xF2F3F5),
    ("mono", "Mono", 0x9A9AA2),
];

pub fn scheme(id: &str) -> &'static Scheme {
    SCHEMES.iter().find(|s| s.id == id).unwrap_or(&SCHEMES[0])
}

pub fn accent(id: &str) -> u32 {
    ACCENTS.iter().find(|a| a.0 == id).map_or(0xF2F3F5, |a| a.2)
}

/// Builds the theme from Settings → Appearance. The open panel and the pill share it.
pub fn build(scheme_id: &str, accent_id: &str, glass: bool, glass_strength: f64) -> Theme {
    let sc = scheme(scheme_id);
    let acc = if accent_id.is_empty() || accent_id == "auto" { accent(sc.accent) } else { accent(accent_id) };
    // Translucent: panes become see-through, the pill stays mostly solid.
    let pane = if glass { (0.55 + 0.35 * (1.0 - glass_strength.clamp(0.0, 1.0))) as f32 } else { 1.0 };
    let pill = if glass { (0.80 + 0.15 * (1.0 - glass_strength.clamp(0.0, 1.0))) as f32 } else { 1.0 };
    let text = color(sc.text, 1.0);
    let t = Theme {
        pill: color(sc.pill, pill),
        surface: color(sc.surface, pane),
        surface2: color(sc.surface2, pane),
        hover: with_alpha(color(sc.text, 1.0), if sc.light { 0.07 } else { 0.08 }),
        stroke: with_alpha(text, if sc.light { 0.12 } else { 0.08 }),
        track: with_alpha(text, if sc.light { 0.14 } else { 0.18 }),
        text,
        subtext: color(sc.subtext, 1.0),
        accent: color(acc, 1.0),
        on_accent: color(0xFFFFFF, 1.0),
        accent_soft: color(acc, 0.19),
        danger: color(if sc.light { 0xD7362B } else { 0xFF453A }, 1.0),
    };
    t.with_accent(acc)
}

pub fn current() -> Theme {
    let s = settings::shared();
    build(&s.theme_scheme, &s.theme_accent, s.glass, s.glass_strength)
}
