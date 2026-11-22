//! A small immediate-mode UI on top of `Gfx`. Every frame the views describe what to draw; the same call
//! reports what the pointer did to it (hover, press, drag, wheel, typed text). Persistent bits (which slider
//! is held, focus, scroll offsets, text being edited) live in `UiState`.

pub mod icon_data;
pub mod icons;

use std::cell::Cell;
use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use crate::gfx::{self, mix, rect, with_alpha, Align, Color, Gfx, MEDIUM, REGULAR, SEMIBOLD};

// ---------------------------------------------------------------------- theme

#[derive(Clone, Copy)]
pub struct Theme {
    pub pill: Color,
    pub surface: Color,
    pub surface2: Color,
    pub hover: Color,
    pub stroke: Color,
    pub track: Color,
    pub text: Color,
    pub subtext: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub accent_soft: Color,
    pub danger: Color,
}

impl Theme {
    /// Midnight scheme with the Snow (white) accent — the default look.
    pub fn midnight() -> Self {
        Self {
            pill: gfx::color(0x000000, 1.0),
            surface: gfx::color(0x1B1D21, 1.0),
            surface2: gfx::color(0x26292E, 1.0),
            hover: gfx::color(0xFFFFFF, 0.07),
            stroke: gfx::color(0xFFFFFF, 0.08),
            track: gfx::color(0x3A3D43, 1.0),
            text: gfx::color(0xF2F3F5, 1.0),
            subtext: gfx::color(0x8D929B, 1.0),
            accent: gfx::color(0xF2F3F5, 1.0),
            on_accent: gfx::color(0x0B0E17, 1.0),
            accent_soft: gfx::color(0xFFFFFF, 0.14),
            danger: gfx::color(0xFF5A5F, 1.0),
        }
    }

    pub fn with_accent(mut self, rgb: u32) -> Self {
        self.accent = gfx::color(rgb, 1.0);
        // Light accents get dark text on top, dark accents white.
        let c = self.accent;
        let lum = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        self.on_accent = if lum > 0.6 { gfx::color(0x0B0E17, 1.0) } else { gfx::color(0xFFFFFF, 1.0) };
        self.accent_soft = with_alpha(self.accent, 0.18);
        self
    }
}

// ---------------------------------------------------------------------- geometry

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        rect(self.x, self.y, self.w, self.h)
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn pad(&self, l: f32, t: f32, r: f32, b: f32) -> Rect {
        Rect::new(self.x + l, self.y + t, (self.w - l - r).max(0.0), (self.h - t - b).max(0.0))
    }
    /// Takes `w` from the left; returns (taken, rest after `gap`).
    pub fn take_left(&self, w: f32, gap: f32) -> (Rect, Rect) {
        (Rect::new(self.x, self.y, w, self.h), Rect::new(self.x + w + gap, self.y, (self.w - w - gap).max(0.0), self.h))
    }
    pub fn take_right(&self, w: f32, gap: f32) -> (Rect, Rect) {
        (Rect::new(self.right() - w, self.y, w, self.h), Rect::new(self.x, self.y, (self.w - w - gap).max(0.0), self.h))
    }
    pub fn take_top(&self, h: f32, gap: f32) -> (Rect, Rect) {
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y + h + gap, self.w, (self.h - h - gap).max(0.0)))
    }
    pub fn take_bottom(&self, h: f32, gap: f32) -> (Rect, Rect) {
        (Rect::new(self.x, self.bottom() - h, self.w, h), Rect::new(self.x, self.y, self.w, (self.h - h - gap).max(0.0)))
    }
    /// Splits into `n` equal columns with `gap` between.
    pub fn columns(&self, n: usize, gap: f32) -> Vec<Rect> {
        let w = (self.w - gap * (n as f32 - 1.0)) / n as f32;
        (0..n).map(|i| Rect::new(self.x + i as f32 * (w + gap), self.y, w, self.h)).collect()
    }
    pub fn rows(&self, n: usize, gap: f32) -> Vec<Rect> {
        let h = (self.h - gap * (n as f32 - 1.0)) / n as f32;
        (0..n).map(|i| Rect::new(self.x, self.y + i as f32 * (h + gap), self.w, h)).collect()
    }
}

// ---------------------------------------------------------------------- input

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Enter,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
    SelectAll,
    Copy,
    Paste,
    Cut,
    /// Ctrl+← / Ctrl+→: previous / next tab (handled by the shell, never seen by tools).
    TabPrev,
    TabNext,
    /// Ctrl+1…9: that tab.
    TabAt(u8),
}

#[derive(Default)]
pub struct Input {
    /// Pointer position in panel coordinates (DIP), NaN when outside the window.
    pub mouse: (f32, f32),
    pub down: bool,
    pub right_pressed: bool,
    /// Pressed this frame.
    pub pressed: bool,
    pub released: bool,
    pub wheel: f32,
    pub chars: String,
    pub keys: Vec<Key>,
    /// A press already handled by a widget this frame.
    pub consumed: bool,
}

impl Input {
    /// Clears the one-frame events after a frame has used them.
    pub fn end_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.right_pressed = false;
        self.wheel = 0.0;
        self.chars.clear();
        self.keys.clear();
        self.consumed = false;
    }
}

/// Text being edited in a field.
#[derive(Default, Clone)]
pub struct TextEdit {
    pub text: String,
    /// Caret position (in chars).
    pub caret: usize,
    /// Ctrl+A: everything is selected (typing replaces it, Backspace clears it).
    pub all: bool,
}

#[derive(Default)]
pub struct UiState {
    /// Widget being dragged (slider) or pressed.
    pub active: u64,
    /// Text field with keyboard focus.
    pub focus: u64,
    pub scroll: HashMap<u64, f32>,
    pub edits: HashMap<u64, TextEdit>,
    /// Open dropdown and the rectangle its list covered last frame (clicks there go to it).
    pub dropdown: u64,
    pub overlay: Option<Rect>,
    pub caret_on: bool,
    /// Generic per-view numbers (animations, selection) keyed by id.
    pub values: HashMap<u64, f32>,
}

/// Stable id from a label (FNV-1a).
pub const fn id(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut h: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u64;
        h = h.wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

const LIST_ROW: f32 = 28.0;

pub fn idx(s: &str, n: usize) -> u64 {
    id(s) ^ ((n as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15))
}

struct Dropdown {
    id: u64,
    anchor: Rect,
    options: Vec<String>,
    selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Accent,
    Normal,
    Ghost,
}

pub struct Ui<'a> {
    pub g: &'a Gfx,
    pub input: &'a mut Input,
    pub st: &'a mut UiState,
    pub t: Theme,
    /// Overall opacity (the panel fading in).
    pub alpha: f32,
    /// A widget wants another frame soon (animation, caret).
    pub animate: bool,
    /// The window needs keyboard focus (a text field was clicked).
    pub wants_keyboard: bool,
    dropdowns: Vec<Dropdown>,
    clip: Vec<Rect>,
    /// The panel area (lists stay inside it).
    pub bounds: Rect,
    /// Staggered entrance: seconds since the tool opened (None: off), blocks so far, their opacity, and
    /// whether one is still coming in.
    stagger: Option<f32>,
    blocks: Cell<u32>,
    stagger_alpha: Cell<f32>,
    stagger_busy: Cell<bool>,
}

/// Staggered entrance: each block starts this much after the one before, takes this long, rising this far.
const STAGGER_STEP: f32 = 0.03;
const STAGGER_SECS: f32 = 0.24;
const STAGGER_RISE: f32 = 12.0;

impl<'a> Ui<'a> {
    pub fn new(g: &'a Gfx, input: &'a mut Input, st: &'a mut UiState, t: Theme, alpha: f32) -> Self {
        Self { g, input, st, t, alpha, animate: false, wants_keyboard: false, dropdowns: Vec::new(), clip: Vec::new(), bounds: Rect::new(-1e5, -1e5, 2e5, 2e5), stagger: None, blocks: Cell::new(0), stagger_alpha: Cell::new(1.0), stagger_busy: Cell::new(false) }
    }

    pub fn c(&self, c: Color) -> Color {
        with_alpha(c, self.alpha * self.stagger_alpha.get())
    }

    /// Staggered entrance (a tool just opened, `secs` ago): every card or row-sized fill starts the next
    /// "block", and what's drawn from there fades in and rises into place a little after the one before.
    pub fn start_stagger(&mut self, secs: f32) {
        self.stagger = Some(secs);
        self.blocks.set(0);
        self.apply_stagger(0);
    }

    /// Ends the entrance: draws normally again. True while some block is still on its way in.
    pub fn end_stagger(&mut self) -> bool {
        self.stagger = None;
        self.stagger_alpha.set(1.0);
        self.g.set_offset(0.0, 0.0);
        self.stagger_busy.replace(false)
    }

    fn block(&self, r: Rect) {
        if self.stagger.is_some() && r.w >= 60.0 && r.h >= 24.0 {
            let n = self.blocks.get() + 1;
            self.blocks.set(n);
            self.apply_stagger(n);
        }
    }

    fn apply_stagger(&self, n: u32) {
        let Some(t) = self.stagger else { return };
        let delay = n.min(10) as f32 * STAGGER_STEP;
        let k = ((t - delay) / STAGGER_SECS).clamp(0.0, 1.0);
        if k < 1.0 {
            self.stagger_busy.set(true);
        }
        let e = 1.0 - (1.0 - k).powi(3);
        self.stagger_alpha.set(e);
        self.g.set_offset(0.0, STAGGER_RISE * (1.0 - e));
    }

    pub fn mouse(&self) -> (f32, f32) {
        self.input.mouse
    }

    fn visible(&self, p: (f32, f32)) -> bool {
        self.clip.last().map_or(true, |c| c.contains(p)) && !self.st.overlay.is_some_and(|o| o.contains(p))
    }

    pub fn hovered(&self, r: Rect) -> bool {
        r.contains(self.input.mouse) && self.visible(self.input.mouse) && (self.st.active == 0)
    }

    /// True once when `r` is pressed (left button) this frame.
    pub fn clicked(&mut self, r: Rect) -> bool {
        if self.input.pressed && !self.input.consumed && r.contains(self.input.mouse) && self.visible(self.input.mouse) {
            self.input.consumed = true;
            true
        } else {
            false
        }
    }

    pub fn right_clicked(&mut self, r: Rect) -> bool {
        self.input.right_pressed && r.contains(self.input.mouse) && self.visible(self.input.mouse)
    }

    pub fn push_clip(&mut self, r: Rect) {
        self.g.push_clip(r.d2d());
        self.clip.push(r);
    }

    pub fn pop_clip(&mut self) {
        self.g.pop_clip();
        self.clip.pop();
    }

    // ------------------------------------------------------------------ drawing shortcuts

    pub fn text(&self, s: &str, r: Rect, size: f32, weight: u16, c: Color, align: Align) {
        self.g.text(s, size, weight, r.d2d(), align, self.c(c));
    }

    pub fn wrap(&self, s: &str, r: Rect, size: f32, weight: u16, c: Color) {
        self.g.text_wrap(s, size, weight, r.d2d(), Align::Left, self.c(c));
    }

    pub fn measure(&self, s: &str, size: f32, weight: u16) -> f32 {
        self.g.measure(s, size, weight)
    }

    pub fn icon(&self, name: &'static str, cx: f32, cy: f32, size: f32, c: Color) {
        self.g.icon(name, cx - size / 2.0, cy - size / 2.0, size, self.c(c));
    }

    pub fn card(&self, r: Rect, radius: f32) {
        self.block(r);
        self.g.fill_round(r.d2d(), radius, self.c(self.t.surface));
    }

    pub fn fill(&self, r: Rect, radius: f32, c: Color) {
        self.block(r);
        self.g.fill_round(r.d2d(), radius, self.c(c));
    }

    /// Small uppercase label, as section captions.
    pub fn caps(&self, s: &str, r: Rect) {
        self.text(&s.to_uppercase(), r, 11.5, crate::gfx::BOLD, self.t.subtext, Align::Left);
    }

    // ------------------------------------------------------------------ widgets

    /// A clickable area with a soft hover; returns true when clicked.
    pub fn hit(&mut self, r: Rect, radius: f32) -> bool {
        if self.hovered(r) {
            let h = self.t.hover;
            self.fill(r, radius, h);
        }
        self.clicked(r)
    }

    pub fn button(&mut self, r: Rect, label: &str, style: ButtonStyle) -> bool {
        let hov = self.hovered(r);
        let (bg, fg) = match style {
            ButtonStyle::Accent => (if hov { mix(self.t.accent, self.t.pill, 0.12) } else { self.t.accent }, self.t.on_accent),
            ButtonStyle::Normal => (if hov { mix(self.t.surface2, self.t.text, 0.08) } else { self.t.surface2 }, self.t.text),
            ButtonStyle::Ghost => (if hov { self.t.hover } else { gfx::color(0, 0.0) }, self.t.text),
        };
        self.fill(r, 8.0, bg);
        self.text(label, r, 12.0, SEMIBOLD, fg, Align::Center);
        self.clicked(r)
    }

    /// Round-cornered icon button (hover background).
    pub fn icon_button(&mut self, r: Rect, icon: &'static str, size: f32, c: Color) -> bool {
        let clicked = self.hit(r, 8.0);
        self.icon(icon, r.cx(), r.cy(), size, c);
        clicked
    }

    /// Toggle switch (pill). Returns true when flipped.
    pub fn toggle(&mut self, r: Rect, on: bool) -> bool {
        let track = if on { self.t.accent } else { self.t.track };
        self.fill(r, r.h / 2.0, track);
        let k = r.h - 6.0;
        let x = if on { r.right() - 3.0 - k } else { r.x + 3.0 };
        let knob = if on { self.t.on_accent } else { self.t.text };
        self.g.fill_circle(x + k / 2.0, r.cy(), k / 2.0, self.c(knob));
        self.clicked(r)
    }

    /// Horizontal slider 0..1. Returns the new value while dragged (or scrolled).
    pub fn slider(&mut self, id: u64, r: Rect, value: f32) -> Option<f32> {
        let v = value.clamp(0.0, 1.0);
        let ty = r.cy();
        // As the C# slider: a 3 DIP track, 12 DIP thumb with a dark ring.
        self.g.line(r.x, ty, r.right(), ty, self.c(self.t.track), 3.0);
        self.g.line(r.x, ty, r.x + r.w * v, ty, self.c(self.t.accent), 3.0);
        let grab = Rect::new(r.x - 6.0, r.y - 4.0, r.w + 12.0, r.h + 8.0);
        let mut out = None;
        if self.clicked(grab) {
            self.st.active = id;
        }
        if self.st.active == id {
            if self.input.down {
                let nv = ((self.input.mouse.0 - r.x) / r.w).clamp(0.0, 1.0);
                out = Some(nv);
            } else {
                self.st.active = 0;
            }
        } else if self.input.wheel != 0.0 && grab.contains(self.input.mouse) && self.visible(self.input.mouse) {
            out = Some((v + self.input.wheel.signum() * 0.02).clamp(0.0, 1.0));
            self.input.wheel = 0.0;
        }
        let shown = out.unwrap_or(v);
        let kx = r.x + r.w * shown;
        self.g.fill_circle(kx, ty, 6.0, self.c(self.t.text));
        self.g.stroke_circle(kx, ty, 6.0, self.c(self.t.pill), 2.0);
        out
    }

    /// Thin progress bar.
    pub fn progress(&self, r: Rect, frac: f32, fg: Color) {
        // Rounded bars that stay inside r (round-capped lines would stick out by h/2 at each end).
        self.g.fill_round(r.d2d(), r.h / 2.0, self.c(self.t.track));
        let w = r.w * frac.clamp(0.0, 1.0);
        if w > 0.5 {
            self.g.fill_round(Rect::new(r.x, r.y, w.max(r.h), r.h).d2d(), r.h / 2.0, self.c(fg));
        }
    }

    /// Small selectable pill with an optional coloured dot. Returns true when clicked.
    pub fn chip(&mut self, r: Rect, label: &str, selected: bool, dot: Option<Color>) -> bool {
        let hov = self.hovered(r);
        let bg = if selected { self.t.accent_soft } else if hov { mix(self.t.surface2, self.t.text, 0.06) } else { self.t.surface2 };
        self.fill(r, 8.0, bg);
        let mut x = r.x + 10.0;
        if let Some(d) = dot {
            self.g.fill_circle(x + 3.5, r.cy(), 3.5, self.c(d));
            x += 12.0;
        }
        let fg = if selected { self.t.text } else { self.t.subtext };
        self.text(label, Rect::new(x, r.y, r.right() - x - 8.0, r.h), 12.0, SEMIBOLD, fg, Align::Left);
        self.clicked(r)
    }

    /// Width a chip needs for `label`.
    pub fn chip_w(&self, label: &str, dot: bool) -> f32 {
        self.measure(label, 12.0, SEMIBOLD) + 20.0 + if dot { 12.0 } else { 0.0 }
    }

    /// Vertical scroll area: `body(ui, top)` draws at y = `top` (already offset). Returns nothing.
    pub fn scroll(&mut self, id: u64, r: Rect, content_h: f32, body: impl FnOnce(&mut Ui, f32)) {
        let max = (content_h - r.h).max(0.0);
        let mut off = self.st.scroll.get(&id).copied().unwrap_or(0.0).clamp(0.0, max);
        if self.input.wheel != 0.0 && r.contains(self.input.mouse) && self.visible(self.input.mouse) {
            off = (off - self.input.wheel * 48.0).clamp(0.0, max);
            self.input.wheel = 0.0;
        }
        self.st.scroll.insert(id, off);
        self.push_clip(r);
        body(self, r.y - off);
        self.pop_clip();
        if max > 0.0 {
            // A slim scroll indicator on the right.
            let bar_h = (r.h * r.h / content_h).max(18.0);
            let y = r.y + (r.h - bar_h) * (off / max);
            self.fill(Rect::new(r.right() - 3.0, y, 3.0, bar_h), 1.5, with_alpha(self.t.text, 0.18));
        }
    }

    /// Single-line text field. Returns `Some(text)` when Enter is pressed (the field clears).
    pub fn text_field(&mut self, id: u64, r: Rect, placeholder: &str, size: f32) -> Option<String> {
        let focused = self.st.focus == id;
        if self.clicked(r) {
            self.st.focus = id;
            self.wants_keyboard = true;
        } else if self.input.pressed && !r.contains(self.input.mouse) && focused {
            self.st.focus = 0;
        }
        let focused = self.st.focus == id;
        let mut submitted = None;
        let edit = self.st.edits.entry(id).or_default();
        if focused {
            if edit.all && (!self.input.chars.is_empty() || self.input.keys.iter().any(|k| matches!(k, Key::Backspace | Key::Delete | Key::Paste))) {
                // Typing over a full selection replaces it.
                edit.text.clear();
                edit.caret = 0;
                edit.all = false;
                self.input.keys.retain(|k| !matches!(k, Key::Backspace | Key::Delete));
            }
            for ch in self.input.chars.chars() {
                if !ch.is_control() {
                    let byte = edit.text.char_indices().nth(edit.caret).map_or(edit.text.len(), |(b, _)| b);
                    edit.text.insert(byte, ch);
                    edit.caret += 1;
                }
            }
            for key in self.input.keys.clone() {
                let len = edit.text.chars().count();
                match key {
                    Key::Backspace if edit.caret > 0 => {
                        let b = edit.text.char_indices().nth(edit.caret - 1).map(|(b, _)| b).unwrap_or(0);
                        edit.text.remove(b);
                        edit.caret -= 1;
                    }
                    Key::Delete if edit.caret < len => {
                        let b = edit.text.char_indices().nth(edit.caret).map(|(b, _)| b).unwrap_or(0);
                        edit.text.remove(b);
                    }
                    Key::Left => {
                        edit.caret = edit.caret.saturating_sub(1);
                        edit.all = false;
                    }
                    Key::Right => {
                        edit.caret = (edit.caret + 1).min(len);
                        edit.all = false;
                    }
                    Key::Home => edit.caret = 0,
                    Key::End => edit.caret = len,
                    Key::Enter => {
                        if !edit.text.trim().is_empty() {
                            submitted = Some(edit.text.trim().to_string());
                            edit.text.clear();
                            edit.caret = 0;
                        }
                    }
                    Key::Escape => {
                        self.st.focus = 0;
                    }
                    Key::Paste => {
                        if let Some(p) = crate::clipboard::get_text() {
                            let p = p.replace(['\r', '\n'], " ");
                            let byte = edit.text.char_indices().nth(edit.caret).map_or(edit.text.len(), |(b, _)| b);
                            edit.text.insert_str(byte, &p);
                            edit.caret += p.chars().count();
                        }
                    }
                    Key::SelectAll => {
                        edit.all = !edit.text.is_empty();
                        edit.caret = edit.text.chars().count();
                    }
                    Key::Cut => {
                        crate::clipboard::set_text(&edit.text);
                        edit.text.clear();
                        edit.caret = 0;
                        edit.all = false;
                    }
                    Key::Copy => crate::clipboard::set_text(&edit.text),
                    _ => {}
                }
            }
        }
        let edit = self.st.edits.get(&id).cloned().unwrap_or_default();
        let inner = r.pad(0.0, 0.0, 0.0, 0.0);
        if edit.text.is_empty() {
            self.text(placeholder, inner, size, REGULAR, self.t.subtext, Align::Left);
        } else {
            if focused && edit.all {
                let w = self.measure(&edit.text, size, REGULAR).min(inner.w);
                let sel = self.t.accent_soft;
                self.fill(Rect::new(inner.x - 1.0, inner.cy() - size * 0.75, w + 2.0, size * 1.5), 3.0, sel);
            }
            self.text(&edit.text, inner, size, REGULAR, self.t.text, Align::Left);
        }
        if focused {
            self.animate = true;
            if self.st.caret_on {
                let before: String = edit.text.chars().take(edit.caret).collect();
                let x = inner.x + self.measure(&before, size, REGULAR);
                self.g.line(x + 0.5, r.cy() - size * 0.62, x + 0.5, r.cy() + size * 0.62, self.c(self.t.text), 1.2);
            }
        }
        submitted
    }

    /// Current text of a field.
    pub fn field_text(&self, id: u64) -> String {
        self.st.edits.get(&id).map(|e| e.text.clone()).unwrap_or_default()
    }

    pub fn set_field_text(&mut self, id: u64, s: &str) {
        let e = self.st.edits.entry(id).or_default();
        e.text = s.to_string();
        e.caret = s.chars().count();
    }

    /// A dropdown: shows `options[selected]`; clicking opens a list (drawn on top at the end of the frame).
    /// Returns the newly picked index.
    pub fn dropdown(&mut self, id: u64, r: Rect, options: &[String], selected: usize) -> Option<usize> {
        let open = self.st.dropdown == id;
        let hov = self.hovered(r);
        self.fill(r, 9.0, if hov || open { mix(self.t.surface2, self.t.text, 0.06) } else { self.t.surface2 });
        let label = options.get(selected).cloned().unwrap_or_default();
        self.text(&label, r.pad(12.0, 0.0, 30.0, 0.0), 13.0, MEDIUM, self.t.text, Align::Left);
        self.icon(if open { "ChevronUp" } else { "ChevronDown" }, r.right() - 16.0, r.cy(), 13.0, self.t.subtext);
        let mut picked = None;
        if open {
            // Clicks inside the list (last frame's rectangle) pick an option; the wheel scrolls a long one.
            let (list, first, rows) = self.list_rect(id, r, options.len(), selected);
            if self.input.wheel != 0.0 && list.contains(self.input.mouse) {
                let max = options.len().saturating_sub(rows) as f32;
                let v = (first as f32 - self.input.wheel.signum()).clamp(0.0, max);
                self.st.values.insert(id, v);
                self.input.wheel = 0.0;
            }
            if self.input.pressed && !self.input.consumed {
                if list.contains(self.input.mouse) {
                    let i = first + ((self.input.mouse.1 - list.y - 4.0) / LIST_ROW).floor() as usize;
                    if i < options.len() {
                        picked = Some(i);
                    }
                    self.input.consumed = true;
                }
                self.st.dropdown = 0;
                self.st.overlay = None;
                if r.contains(self.input.mouse) {
                    self.input.consumed = true;
                }
            } else {
                self.dropdowns.push(Dropdown { id, anchor: r, options: options.to_vec(), selected });
            }
        } else if self.clicked(r) {
            self.st.dropdown = id;
            self.st.values.remove(&id);
            self.animate = true;
        }
        picked
    }

    /// Where an open list goes: below the anchor when it fits in the panel, else above, else on the roomier side,
    /// cut to whole rows (then it scrolls). Returns (list rect, first row shown, rows shown).
    fn list_rect(&mut self, id: u64, anchor: Rect, n: usize, selected: usize) -> (Rect, usize, usize) {
        let b = self.bounds;
        let need = LIST_ROW * n as f32 + 8.0;
        let below = b.bottom() - anchor.bottom() - 6.0;
        let above = anchor.y - b.y - 6.0;
        let (down, space) = if need <= below { (true, below) } else if need <= above { (false, above) } else if below >= above { (true, below) } else { (false, above) };
        let rows = (((space.min(need) - 8.0) / LIST_ROW).floor() as usize).clamp(1, n.max(1));
        let h = LIST_ROW * rows as f32 + 8.0;
        let y = if down { anchor.bottom() + 4.0 } else { anchor.y - 4.0 - h };
        let max_first = n.saturating_sub(rows);
        // First time open: scroll so the selected row shows.
        let first = match self.st.values.get(&id) {
            Some(v) => (*v as usize).min(max_first),
            None => {
                let f = selected.saturating_sub(rows.saturating_sub(1)).min(max_first);
                self.st.values.insert(id, f as f32);
                f
            }
        };
        (Rect::new(anchor.x, y, anchor.w, h), first, rows)
    }

    /// Draws what floats above everything (open dropdown lists). Call once at the end of the frame.
    pub fn finish(&mut self) {
        self.st.overlay = None;
        let dropdowns = std::mem::take(&mut self.dropdowns);
        for d in dropdowns {
            if self.st.dropdown != d.id {
                continue;
            }
            let row_h = LIST_ROW;
            let (list, first, rows) = self.list_rect(d.id, d.anchor, d.options.len(), d.selected);
            // Always solid: in Translucent / Frosted the panel behind would show through the options.
            self.g.fill_round(list.d2d(), 8.0, self.c(crate::gfx::Color { a: 1.0, ..self.t.surface2 }));
            self.g.stroke_round(list.d2d(), 8.0, self.c(self.t.stroke), 1.0);
            for (i, o) in d.options.iter().enumerate().skip(first).take(rows) {
                let row = Rect::new(list.x + 4.0, list.y + 4.0 + (i - first) as f32 * row_h, list.w - 8.0, row_h);
                if row.contains(self.input.mouse) {
                    self.g.fill_round(row.d2d(), 7.0, self.c(self.t.hover));
                }
                let fg = if i == d.selected { self.t.text } else { self.t.subtext };
                self.text(o, row.pad(10.0, 0.0, 26.0, 0.0), 12.5, if i == d.selected { SEMIBOLD } else { REGULAR }, fg, Align::Left);
                if i == d.selected {
                    self.icon("Check", row.right() - 14.0, row.cy(), 12.0, self.t.text);
                }
            }
            self.st.overlay = Some(list);
        }
    }
}

/// Formats seconds as m:ss.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
