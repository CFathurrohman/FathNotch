//! Home: greeting with the clock, weather and date pills, a Now Playing card (blurred art behind), today's
//! events and open to-dos. Sizes follow the C# HomeView.xaml.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::gfx::{color, with_alpha, Align, Color, BOLD, LIGHT, MEDIUM, REGULAR, SEMIBOLD};
use crate::model;
use crate::notch::{now, DAYS, MONTHS};
use crate::services::{calendar, media, settings, todos, weather};
use crate::ui::{Rect, Ui};

/// The Now Playing card is 440 wide (C#: first column).
const CARD_W: f32 = 440.0;
/// Header row (avatar 40) + its 8 bottom margin.
const HEADER_H: f32 = 40.0;
const HEADER_GAP: f32 = 8.0;

#[derive(Default)]
pub struct State {
    /// When Home was last opened (the cards rise in, staggered).
    shown_at: f64,
    /// Today's events and open to-dos, re-read every couple of seconds (not every frame).
    lists: Option<(Instant, Vec<(String, String)>, Vec<String>, usize)>,
    /// Blurred-art backdrop baked for (art id, surface colour, card size): BMP bytes, made off the UI thread.
    backdrop: Arc<Mutex<Option<(u64, Vec<u8>)>>>,
    backdrop_for: u64,
}

pub fn shown(s: &mut State) {
    s.shown_at = model::clock_secs();
    s.lists = None;
}

fn first_name() -> String {
    let s = settings::get();
    let name = if s.display_name.trim().is_empty() { std::env::var("USERNAME").unwrap_or_else(|_| "there".into()) } else { s.display_name };
    let first = name.split_whitespace().next().unwrap_or("there").to_string();
    let mut c = first.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => first,
    }
}

fn refresh_lists(s: &mut State) {
    if s.lists.as_ref().is_some_and(|(t, ..)| t.elapsed().as_secs_f32() < 2.0) {
        return;
    }
    let (y, m, d) = calendar::today();
    let mut items: Vec<(String, String)> = calendar::on_day(y, m, d)
        .into_iter()
        .map(|e| (e.title.clone(), e.time.map_or("All day".to_string(), |(h, mi)| format!("{h:02}:{mi:02}"))))
        .collect();
    let all = todos::all();
    for td in all.iter().filter(|x| !x.done && x.due_today()) {
        let when = td.due_local().filter(|_| !td.due_all_day).map_or("All day".to_string(), |(_, _, _, h, mi)| format!("{h:02}:{mi:02}"));
        items.push((format!("To-do · {}", td.text), when));
    }
    // C#: open to-dos, starred first.
    let mut open: Vec<&todos::Todo> = all.iter().filter(|x| !x.done).collect();
    open.sort_by_key(|x| !x.starred);
    let count = open.len();
    let open: Vec<String> = open.into_iter().take(2).map(|x| x.text.clone()).collect();
    s.lists = Some((Instant::now(), items, open, count));
}

/// Cards fade in and rise 8 px over 320 ms, each `delay` later (C# Rise).
fn rise(ui: &mut Ui, s: &State, delay: f64) -> (f32, f32) {
    let k = ((model::clock_secs() - s.shown_at - delay) / 0.32).clamp(0.0, 1.0) as f32;
    if k < 1.0 {
        ui.animate = true;
    }
    let e = 1.0 - (1.0 - k).powi(3);
    (e, 8.0 * (1.0 - e))
}

fn shifted(r: Rect, dy: f32) -> Rect {
    Rect::new(r.x, r.y + dy, r.w, r.h)
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    refresh_lists(s);
    let (top, rest) = r.take_top(HEADER_H, HEADER_GAP);
    header(ui, top, go);

    let base_alpha = ui.alpha;
    let (left, right) = rest.take_left(CARD_W, 12.0);
    let rows = right.rows(2, 10.0);

    let (a, dy) = rise(ui, s, 0.0);
    ui.alpha = base_alpha * a;
    now_playing_card(ui, s, shifted(left, dy), go);
    let (a, dy) = rise(ui, s, 0.06);
    ui.alpha = base_alpha * a;
    today_card(ui, s, shifted(rows[0], dy), go);
    let (a, dy) = rise(ui, s, 0.12);
    ui.alpha = base_alpha * a;
    reminders_card(ui, s, shifted(rows[1], dy), go);
    ui.alpha = base_alpha;
}

// ---------------------------------------------------------------------- header

fn header(ui: &mut Ui, top: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let n = now();
    let name = first_name();

    // Avatar: accent ring (1.6, 70 % → 100 % on hover), a soft accent disc 4 in, the initial.
    let av = Rect::new(top.x, top.cy() - 20.0, 40.0, 40.0);
    let ring = if ui.hovered(av) { 1.0 } else { 0.7 };
    ui.g.stroke_circle(av.cx(), av.cy(), 19.2, ui.c(with_alpha(t.accent, ring)), 1.6);
    ui.g.fill_circle(av.cx(), av.cy(), 16.0, ui.c(t.accent_soft));
    ui.text(&name.chars().next().unwrap_or('?').to_uppercase().to_string(), av, 15.0, SEMIBOLD, t.accent, Align::Center);

    // Greeting (icon + caps line) over the name, centred on the avatar.
    let part = match n.hour {
        0..=4 => "EVENING",
        5..=11 => "MORNING",
        12..=16 => "AFTERNOON",
        _ => "EVENING",
    };
    let gx = av.right() + 12.0;
    let gy = av.cy() - 17.5;
    ui.icon(if (6..18).contains(&n.hour) { "Sun" } else { "Moon" }, gx + 5.5, gy + 6.5, 11.0, t.accent);
    ui.text(&format!("GOOD {part}"), Rect::new(gx + 16.0, gy, 240.0, 13.0), 10.5, BOLD, t.accent, Align::Left);
    ui.text(&name, Rect::new(gx, gy + 13.0, 320.0, 22.0), 18.0, SEMIBOLD, t.text, Align::Left);

    // Right: weather pill, date pill, divider, clock.
    let time = format!("{:02}:{:02}", n.hour, n.minute);
    let cw = ui.measure(&time, 26.0, LIGHT) + 2.0;
    let clock_r = Rect::new(top.right() - cw, top.y, cw, top.h);
    ui.text(&time, clock_r, 26.0, LIGHT, t.text, Align::Right);
    let div_x = clock_r.x - 14.0 - 1.0;
    ui.fill(Rect::new(div_x, top.cy() - 11.0, 1.0, 22.0), 0.0, t.stroke);

    let date = format!("{}, {} {}", &DAYS[n.dow][..3], &MONTHS[n.month - 1][..3], n.day);
    let dw = ui.measure(&date, 12.0, MEDIUM) + 39.0;
    let date_r = Rect::new(div_x - 14.0 - dw, top.cy() - 12.0, dw, 24.0);
    pill(ui, date_r, "CalendarDays", 12.0, t.subtext, &date, MEDIUM, false);
    if let Some(w) = model::weather_now() {
        let city = settings::get().weather_city;
        let label = if city.trim().is_empty() { format!("{} {}", weather::degrees(w.temp), w.city) } else { format!("{} {}", weather::degrees(w.temp), city.trim()) };
        let ww = ui.measure(&label, 12.0, SEMIBOLD) + 40.0;
        let wr = Rect::new(date_r.x - 6.0 - ww, date_r.y, ww, 24.0);
        if pill(ui, wr, model::weather_icon(w.code, w.is_day), 13.0, t.text, &label, SEMIBOLD, true) {
            *go = Some("weather");
        }
    }
}

/// Rounded chip: surface fill, hairline stroke, icon + label (C#: padding 9,4,10,4, radius 16).
#[allow(clippy::too_many_arguments)]
fn pill(ui: &mut Ui, r: Rect, icon: &'static str, icon_size: f32, icon_c: Color, label: &str, weight: u16, button: bool) -> bool {
    let t = ui.t;
    let hov = button && ui.hovered(r);
    ui.fill(r, r.h / 2.0, if hov { t.surface2 } else { t.surface });
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), r.h / 2.0 - 0.5, ui.c(t.stroke), 1.0);
    ui.icon(icon, r.x + 10.0 + icon_size / 2.0, r.cy(), icon_size, icon_c);
    let tx = r.x + 10.0 + icon_size + 6.0;
    ui.text(label, Rect::new(tx, r.y, r.right() - tx, r.h), 12.0, weight, t.text, Align::Left);
    button && ui.clicked(r)
}

// ---------------------------------------------------------------------- now playing

/// Decodes `bytes` and scales the whole picture to `w`×`h` (Fant: each pixel is an average). BGRA rows.
pub fn art_pixels(bytes: &[u8], w: u32, h: u32) -> Option<(Vec<u8>, (u32, u32))> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut aw, mut ah) = (0u32, 0u32);
        frame.GetSize(&mut aw, &mut ah).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&conv, w, h, WICBitmapInterpolationModeFant).ok()?;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        scaler.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
        Some((buf, (aw.max(1), ah.max(1))))
    }
}

/// An uncompressed 32-bit BMP (opaque) from RGB rows, top row first.
pub fn bmp(w: u32, h: u32, rgb: &[[f32; 3]]) -> Vec<u8> {
    let size = 54 + w * h * 4;
    let mut b = Vec::with_capacity(size as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]); // BI_RGB, sizes, resolution, palette
    for p in rgb {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        b.extend_from_slice(&[q(p[2]), q(p[1]), q(p[0]), 255]);
    }
    b
}

/// Bakes the C# card look into one small picture: the art blurred (UniformToFill, ×1.4, 60 %) over the
/// surface, under a diagonal surface scrim (25 % → 66 % at 0.55 → 88 %). Drawn stretched, it stays soft.
fn bake_backdrop(art: &[u8], surface: [f32; 3], card: (f32, f32)) -> Option<Vec<u8>> {
    const N: u32 = 10;
    let (px, (aw, ah)) = art_pixels(art, N, N)?;
    let sample = |u: f32, v: f32| -> [f32; 3] {
        // Bilinear between the N×N cell centres.
        let fx = (u * N as f32 - 0.5).clamp(0.0, N as f32 - 1.0);
        let fy = (v * N as f32 - 0.5).clamp(0.0, N as f32 - 1.0);
        let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(N - 1), (y0 + 1).min(N - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let at = |x: u32, y: u32, c: usize| px[((y * N + x) * 4) as usize + (2 - c)] as f32 / 255.0;
        let mut o = [0.0; 3];
        for (c, v) in o.iter_mut().enumerate() {
            let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
            let bot = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
            *v = top * (1.0 - ty) + bot * ty;
        }
        o
    };
    let (cw, ch) = card;
    let gw = 48u32;
    let gh = ((gw as f32 * ch / cw).round() as u32).max(2);
    let fill = (cw / aw as f32).max(ch / ah as f32) * 1.4;
    let mut out = Vec::with_capacity((gw * gh) as usize);
    for gy in 0..gh {
        for gx in 0..gw {
            let nx = (gx as f32 + 0.5) / gw as f32;
            let ny = (gy as f32 + 0.5) / gh as f32;
            let u = 0.5 + (nx - 0.5) * cw / (fill * aw as f32);
            let v = 0.5 + (ny - 0.5) * ch / (fill * ah as f32);
            let a = sample(u, v);
            let t = (nx + ny) / 2.0;
            let scrim = if t < 0.55 { 0.25 + (0.66 - 0.25) * t / 0.55 } else { 0.66 + (0.88 - 0.66) * (t - 0.55) / 0.45 };
            let mut c = [0.0; 3];
            for i in 0..3 {
                let under = surface[i] * 0.4 + a[i] * 0.6;
                c[i] = under * (1.0 - scrim) + surface[i] * scrim;
            }
            out.push(c);
        }
    }
    Some(bmp(gw, gh, &out))
}

fn now_playing_card(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let np = model::now_playing();

    // Card: surface, or the baked blurred-art backdrop when there's art; hairline stroke.
    let mut drew_backdrop = false;
    if let Some(p) = np.as_ref().filter(|p| p.art_id != 0) {
        let sc = t.surface;
        let key = p.art_id ^ ((sc.r * 255.0) as u64) << 8 ^ ((sc.g * 255.0) as u64) << 16 ^ ((sc.b * 255.0) as u64) << 24 ^ 0x5EED_BAC0_0000_0000;
        if s.backdrop_for != key {
            s.backdrop_for = key;
            if let Some(art) = p.art.clone() {
                let slot = s.backdrop.clone();
                let surface = [sc.r, sc.g, sc.b];
                let size = (r.w, r.h);
                crate::bg::spawn(move || {
                    let baked = bake_backdrop(&art, surface, size);
                    *slot.lock().unwrap() = baked.map(|b| (key, b));
                });
            }
        }
        let ready = s.backdrop.lock().unwrap().as_ref().filter(|(k, _)| *k == key).map(|(_, b)| ui.g.load_image(key, b)).unwrap_or(false);
        if ready {
            ui.fill(r, 18.0, t.surface);
            ui.g.image(key, r.inset(1.0, 1.0).d2d(), 17.0, ui.alpha);
            drew_backdrop = true;
        }
    }
    if !drew_backdrop {
        ui.card(r, 18.0);
    }
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 17.5, ui.c(t.stroke), 1.0);

    let inner = r.inset(15.0, 15.0);
    // Art (116, radius 16, soft shadow below).
    let art = Rect::new(inner.x, inner.cy() - 58.0, 116.0, 116.0);
    for i in 1..=6 {
        let k = i as f32 * 1.6;
        let sh = Rect::new(art.x - k, art.y + 4.0 - k, art.w + 2.0 * k, art.h + 2.0 * k);
        ui.fill(sh, 16.0 + k, color(0, 0.075 * (1.0 - i as f32 / 7.0)));
    }
    ui.fill(art, 16.0, t.surface2);
    match &np {
        Some(p) if ui.g.has_image(p.art_id) => ui.g.image(p.art_id, art.d2d(), 12.0, ui.alpha),
        _ => ui.icon("MusicNote", art.cx(), art.cy(), 26.0, t.subtext),
    }

    let x = art.right() + 16.0;
    let w = inner.right() - x;
    let label = match &np {
        Some(p) if p.playing => "NOW PLAYING",
        Some(_) => "PAUSED",
        None => "NOTHING PLAYING",
    };
    ui.text(label, Rect::new(x, inner.y, w - 28.0, 12.0), 10.0, BOLD, t.accent, Align::Left);
    let open = Rect::new(inner.right() + 6.0 - 26.0, inner.y - 4.0, 26.0, 26.0);
    if ui.icon_button(open, "ChevronRight", 13.0, t.subtext) {
        *go = Some("nowplaying");
    }
    let (title, by) = match &np {
        Some(p) => (p.title.clone(), [p.artist.as_str(), p.app.as_str()].iter().filter(|s| !s.trim().is_empty()).cloned().collect::<Vec<_>>().join(" · ")),
        None => ("Nothing playing".to_string(), String::new()),
    };
    ui.text(&title, Rect::new(x, inner.y + 14.0, w - 28.0, 19.0), 15.5, SEMIBOLD, t.text, Align::Left);
    ui.text(&by, Rect::new(x, inner.y + 33.0, w - 28.0, 16.0), 12.5, REGULAR, t.subtext, Align::Left);

    // Controls row (40) at the bottom; progress + times (4 + 3 + 13 + 4) above it.
    let row_y = inner.bottom() - 40.0;
    let bar = Rect::new(x, row_y - 24.0, w, 4.0);
    let (pos, dur) = np.as_ref().map_or((0.0, 0.0), |p| (p.position_now(), p.duration));
    let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
    ui.fill(bar, 2.0, t.track);
    if frac > 0.0 {
        ui.fill(Rect::new(bar.x, bar.y, (bar.w * frac.clamp(0.0, 1.0)).max(4.0), bar.h), 2.0, t.accent);
    }
    {
        ui.text(&crate::views::nowplaying::fmt(pos), Rect::new(x, bar.bottom() + 3.0, 60.0, 14.0), 11.0, REGULAR, t.subtext, Align::Left);
        ui.text(&crate::views::nowplaying::fmt(dur), Rect::new(bar.right() - 60.0, bar.bottom() + 3.0, 60.0, 14.0), 11.0, REGULAR, t.subtext, Align::Right);
    }
    if np.as_ref().is_some_and(|p| p.playing) {
        ui.animate = true;
    }

    let cy = row_y + 20.0;
    let lyr = Rect::new(x, cy - 15.0, 30.0, 30.0);
    let on = settings::get().lyrics_enabled;
    if ui.icon_button(lyr, "Quote", 15.0, if on { t.accent } else { t.subtext }) {
        if !on {
            settings::update(|s| s.lyrics_enabled = true);
        }
        *go = Some("nowplaying");
    }

    // Previous · play/pause (40, glowing) · next, centred; dimmed and inert without a session.
    let has = np.is_some();
    let mid = x + w / 2.0;
    let prev = Rect::new(mid - 20.0 - 16.0 - 34.0, cy - 17.0, 34.0, 34.0);
    let next = Rect::new(mid + 20.0 + 16.0, cy - 17.0, 34.0, 34.0);
    let play = Rect::new(mid - 20.0, cy - 20.0, 40.0, 40.0);
    let fg = if has { t.text } else { with_alpha(t.text, 0.35) };
    if has {
        if ui.icon_button(prev, "Backward", 18.0, fg) {
            crate::bg::spawn(media::previous);
        }
        if ui.icon_button(next, "Forward", 18.0, fg) {
            crate::bg::spawn(media::next);
        }
    } else {
        ui.icon("Backward", prev.cx(), prev.cy(), 18.0, fg);
        ui.icon("Forward", next.cx(), next.cy(), 18.0, fg);
    }
    let dim = if has { 1.0 } else { 0.35 };
    if has {
        let glow = if ui.hovered(play) { 0.6 } else { 0.35 };
        for i in 0..5 {
            let rr = 23.0 + i as f32 * 2.0;
            ui.g.fill_circle(play.cx(), play.cy(), rr, ui.c(with_alpha(t.accent, glow * 0.16 * (1.0 - i as f32 / 5.0))));
        }
    }
    ui.g.fill_circle(play.cx(), play.cy(), 20.0, ui.c(with_alpha(t.accent, dim)));
    let playing = np.as_ref().is_some_and(|p| p.playing);
    ui.icon(if playing { "Pause" } else { "Play" }, play.cx() + if playing { 0.0 } else { 1.0 }, play.cy(), 18.0, with_alpha(t.on_accent, dim));
    if has && ui.clicked(play) {
        crate::bg::spawn(media::play_pause);
    }
}

// ---------------------------------------------------------------------- widgets

/// The widget card (surface, hairline, radius 14) and its header row button: tile, title, count pill.
fn widget(ui: &mut Ui, r: Rect, icon: &'static str, title: &str, count: usize) -> bool {
    let t = ui.t;
    ui.card(r, 14.0);
    ui.g.stroke_round(r.inset(0.5, 0.5).d2d(), 13.5, ui.c(t.stroke), 1.0);
    let row = Rect::new(r.x + 9.0, r.y + 5.0, r.w - 22.0, 32.0);
    if ui.hovered(row) {
        ui.fill(row, 10.0, t.surface2);
    }
    let tile = Rect::new(row.x + 8.0, row.y + 5.0, 22.0, 22.0);
    ui.fill(tile, 6.0, t.accent_soft);
    ui.icon(icon, tile.cx(), tile.cy(), 13.0, t.accent);
    ui.text(title, Rect::new(tile.right() + 8.0, tile.y, 160.0, tile.h), 13.5, SEMIBOLD, t.text, Align::Left);
    if count > 0 {
        let c = count.to_string();
        let cw = (ui.measure(&c, 11.0, SEMIBOLD) + 14.0).max(20.0);
        let cr = Rect::new(row.right() - 8.0 - cw, row.cy() - 8.0, cw, 16.0);
        ui.fill(cr, 8.0, t.accent_soft);
        ui.text(&c, cr, 11.0, SEMIBOLD, t.accent, Align::Center);
    }
    ui.clicked(row)
}

fn today_card(ui: &mut Ui, s: &State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let items = s.lists.as_ref().map(|l| l.1.clone()).unwrap_or_default();
    if widget(ui, r, "CalendarDays", "Today", items.len()) {
        *go = Some("calendar");
    }
    let x = r.x + 15.0;
    if items.is_empty() {
        ui.text("Nothing scheduled", Rect::new(x, r.y + 40.0, r.w - 28.0, 16.0), 12.0, REGULAR, t.subtext, Align::Left);
    }
    for (i, (title, when)) in items.iter().take(2).enumerate() {
        let row = Rect::new(x, r.y + 39.0 + i as f32 * 18.0, r.right() - 13.0 - x, 18.0);
        ui.fill(Rect::new(row.x, row.cy() - 7.0, 3.0, 14.0), 1.5, t.accent);
        let tw = ui.measure(when, 11.5, REGULAR);
        ui.text(title, Rect::new(row.x + 11.0, row.y, row.w - 11.0 - tw - 8.0, row.h), 13.0, REGULAR, t.text, Align::Left);
        ui.text(when, Rect::new(row.right() - tw - 2.0, row.y, tw + 2.0, row.h), 11.5, REGULAR, t.subtext, Align::Right);
    }
}

fn reminders_card(ui: &mut Ui, s: &State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let (open, count) = s.lists.as_ref().map(|l| (l.2.clone(), l.3)).unwrap_or_default();
    if widget(ui, r, "ListBullet", "Reminders", count) {
        *go = Some("todos");
    }
    let x = r.x + 15.0;
    if open.is_empty() {
        let good = color(0x32D74B, 1.0);
        ui.icon("Check", x + 6.0, r.y + 48.0, 12.0, good);
        ui.text("All done", Rect::new(x + 18.0, r.y + 40.0, r.w - 40.0, 16.0), 12.0, REGULAR, t.subtext, Align::Left);
    }
    for (i, text) in open.iter().enumerate() {
        let row = Rect::new(x, r.y + 39.0 + i as f32 * 18.0, r.right() - 13.0 - x, 18.0);
        ui.g.stroke_circle(row.x + 6.0, row.cy(), 5.35, ui.c(t.subtext), 1.3);
        ui.text(text, Rect::new(row.x + 20.0, row.y, (row.w - 20.0).min(200.0), row.h), 13.0, REGULAR, t.text, Align::Left);
    }
}

