# Writing a tool view (Rust)

How a tool view is built, and the helpers it can use. Keep new tools consistent with the existing ones
(sizes, spacing, colors from the theme) and write idiomatic Rust.

## Where things are
- `src/views/<tool>.rs` — one file per tool: `pub struct State` (Default), `pub fn shown(&mut State)` (tool opened:
  kick off background refreshes), `pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>)`.
  `r` is the content rectangle (720 wide; height from `src/tools.rs`). Set `*go = Some("id")` to switch tool.
  The header (title, date/time, pin) and the tab bar are drawn by the shell — don't draw them.
- `src/ui/mod.rs` — immediate-mode kit. `Rect` (take_left/right/top/bottom, columns, rows, inset, pad, contains),
  `Ui` methods: `text(s, rect, size, weight, color, Align)`, `wrap` (multi-line), `measure`, `icon(name, cx, cy, size, color)`,
  `card(rect, radius)`, `fill(rect, radius, color)`, `caps(label, rect)`, `hit(rect, radius) -> clicked` (hover bg),
  `hovered`, `clicked`, `right_clicked`, `button(rect, label, ButtonStyle::{Accent,Normal,Ghost})`, `icon_button`,
  `toggle(rect, on) -> flipped`, `slider(id, rect, 0..1) -> Option<new>`, `progress(rect, frac, color)`,
  `chip(rect, label, selected, dot) / chip_w`, `scroll(id, rect, content_h, |ui, top| ..)`,
  `text_field(id, rect, placeholder, size) -> Option<submitted>` (+ `field_text`, `set_field_text`),
  `dropdown(id, rect, &options, selected) -> Option<picked>`. Ids: `crate::ui::id("view.thing")`, `idx("view.row", i)`.
  `ui.g` is the raw `Gfx` (fill_circle, stroke_circle, line, arc, path(|sink| ..) + fill_geometry/stroke_geometry,
  image(key, rect, radius, opacity), load_image(key, bytes), push_clip/pop_clip). Colours: `ui.t` (Theme: pill, surface,
  surface2, hover, stroke, track, text, subtext, accent, on_accent, accent_soft, danger); wrap raw colours with `ui.c(..)`
  when drawing through `ui.g` so the panel fade applies. Weights: `crate::gfx::{LIGHT, REGULAR, MEDIUM, SEMIBOLD, BOLD}`.
  Icons: names from `src/ui/icon_data.rs` (Heroicons solid names like "Trash", "Plus", "Star"; add ".o" for outline).
- If the kit lacks a widget, write a private helper in your own view file (don't edit `src/ui/mod.rs`).
- Data: `crate::services::*` (settings, todos, ticktick, notes, calendar, prayer, weather, media, lyrics, notifications,
  radios, audio, brightness, system, timers, store), `crate::model` (now_playing, lyric_line, lyrics_all, prayer_times,
  weather_now, idle_items, ...). Never block the UI thread on network/devices: use `crate::bg::spawn(move || ..)` and keep
  results in an `Arc<Mutex<..>>` inside your State (see `src/views/control.rs`); `bg::spawn` triggers a redraw when done.
- Set `ui.animate = true` while something in your view moves (keeps frames coming); live values redraw once a second anyway.
- New back-end code goes in a new `src/services/<name>.rs` (add one `pub mod <name>;` line to `src/services/mod.rs`).

## Build and look at it
- Build into your own target dir so parallel agents don't lock each other:
  `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release --offline --target-dir target\<yourname>`
- Screenshot (off screen, safe while other copies run):
  `powershell -NoProfile -ExecutionPolicy Bypass -File tools\shot.ps1 -Tool <id> -Out <png> -Exe target\<yourname>\release\fathnotch.exe -DataDir <scratch copy>`
  Use a scratch copy of `%LOCALAPPDATA%\FathNotch` for `-DataDir` whenever your view could write data (never modify the user's
  real files). Compare with the C# reference PNG and iterate until it matches.
- Don't edit: `src/main.rs`, `src/notch.rs`, `src/model.rs`, `src/ui/*`, `src/gfx.rs`, `src/views/mod.rs`, `src/tools.rs`,
  other agents' views. Don't kill processes named fathnotch/FathNotch (the user's app runs).
