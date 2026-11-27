//! Hardware: a "Performance | Apps" switch.
//! Performance: interactive cards from `services::hwmon` (sampled every second while on screen): CPU with
//! per-thread bars (click for the last minute), GPU with temperature and video memory (hover for the last
//! minute; the chip switches GPUs), RAM and drive rings (the chip switches drives), the network graph
//! (hover for any second of the last minute), battery, ping and uptime, and the top processes by CPU or
//! memory (a click opens Apps sorted the same way).
//! Apps: running programs grouped by name, sorted by CPU / memory / name, with End task (first click arms
//! it as "Sure?", the second ends the group), sampled every 2 s; the arrow opens Windows Task Manager.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};

use crate::bg;
use crate::gfx::{color, mix, pt, with_alpha, Align, Color, BOLD, REGULAR, SEMIBOLD};
use crate::services::hwmon::{self, HISTORY};
use crate::services::appicons;
use crate::services::system::{self, Proc};
use crate::ui::{id, ButtonStyle, Rect, Ui};

const MAX_ROWS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Performance,
    Apps,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Cpu,
    Memory,
    Name,
}

#[derive(Default)]
struct Data {
    procs: Vec<Proc>,
    proc_samples: u32,
    busy: bool,
    message: Option<(String, Instant)>,
}

#[derive(Default)]
pub struct State {
    data: Arc<Mutex<Data>>,
    mode: Mode,
    sort: Sort,
    last: Option<Instant>,
    /// Program (name) whose End task was clicked once.
    confirming: Option<String>,
    /// CPU card shows the last minute instead of per-thread bars.
    cpu_history: bool,
    /// Which GPU and drive the cards show.
    gpu: usize,
    disk: usize,
    /// Processes card sorted by memory instead of CPU.
    proc_by_mem: bool,
}

pub fn shown(s: &mut State) {
    // Dev aid for screenshots: FATHNOTCH_DEMO_SUB=apps opens on the Apps page.
    if std::env::var("FATHNOTCH_DEMO_SUB").is_ok_and(|v| v == "apps") {
        s.mode = Mode::Apps;
    }
    s.confirming = None;
    {
        let mut d = s.data.lock().unwrap();
        d.proc_samples = 0;
        d.message = None;
    }
    tick(s);
}

/// Reads the process list (both pages show it) on a background thread.
fn tick(s: &mut State) {
    s.last = Some(Instant::now());
    let data = s.data.clone();
    {
        let mut d = data.lock().unwrap();
        if d.busy {
            return;
        }
        d.busy = true;
    }
    bg::spawn(move || {
        let procs = system::processes();
        let mut d = data.lock().unwrap();
        d.busy = false;
        d.procs = procs;
        d.proc_samples += 1;
    });
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    if s.last.map_or(true, |t| t.elapsed().as_millis() >= 2000) {
        tick(s);
    }
    let (top, body) = r.take_top(32.0, 8.0);
    switch(ui, s, top);
    match s.mode {
        Mode::Performance => performance(ui, s, body, go),
        Mode::Apps => apps(ui, s, body),
    }
}

fn switch(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let labels = ["Performance", "Apps"];
    let w = 96.0;
    let bg = Rect::new(r.cx() - w - 3.0, r.y, w * 2.0 + 6.0, r.h);
    ui.fill(bg, 9.0, t.surface);
    for (i, label) in labels.iter().enumerate() {
        let c = Rect::new(bg.x + 3.0 + i as f32 * w, bg.y + 3.0, w, bg.h - 6.0);
        let mode = if i == 0 { Mode::Performance } else { Mode::Apps };
        let on = s.mode == mode;
        if on {
            ui.fill(c, 7.0, t.surface2);
        }
        let hov = ui.hovered(c);
        ui.text(label, c, 12.0, SEMIBOLD, if on || hov { t.text } else { t.subtext }, Align::Center);
        if ui.clicked(c) && !on {
            s.mode = mode;
            s.confirming = None;
            s.data.lock().unwrap().proc_samples = 0;
            tick(s);
        }
    }
}

// ------------------------------------------------------------------ performance

fn gb(b: u64) -> String {
    let v = b as f64 / (1u64 << 30) as f64;
    let s = format!("{v:.1}");
    format!("{} GB", s.trim_end_matches(".0"))
}

/// A ring gauge: track, value arc (from the top, clockwise) and the value in the middle.
fn ring(ui: &mut Ui, cx: f32, cy: f32, rad: f32, frac: f32, c: Color, width: f32) {
    let t = ui.t;
    ui.g.stroke_circle(cx, cy, rad, ui.c(t.track), width);
    let f = frac.clamp(0.0, 1.0);
    if f > 0.005 {
        ui.g.arc(cx, cy, rad, 0.0, std::f32::consts::TAU * f, ui.c(c), width);
    }
}

/// A line graph of `values` (0..=max) filling `r`, newest on the right; with a soft fill under it.
fn graph(ui: &mut Ui, r: Rect, values: &[f32], max: f32, c: Color, fill: bool) {
    if values.len() < 2 || max <= 0.0 {
        return;
    }
    let n = HISTORY;
    let x_of = |i: usize| r.x + r.w * (n - values.len() + i) as f32 / (n - 1) as f32;
    let y_of = |v: f32| r.bottom() - r.h * (v / max).clamp(0.0, 1.0);
    if fill {
        if let Some(geo) = ui.g.path(|s| unsafe {
            s.BeginFigure(pt(x_of(0), r.bottom()), D2D1_FIGURE_BEGIN_FILLED);
            for (i, v) in values.iter().enumerate() {
                s.AddLine(pt(x_of(i), y_of(*v)));
            }
            s.AddLine(pt(x_of(values.len() - 1), r.bottom()));
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
        }) {
            ui.g.fill_geometry(&geo.into(), ui.c(with_alpha(c, 0.16)));
        }
    }
    let cc = ui.c(c);
    for (i, w) in values.windows(2).enumerate() {
        ui.g.line(x_of(i), y_of(w[0]), x_of(i + 1), y_of(w[1]), cc, 1.6);
    }
}

/// Hovering a graph: a guide line and the value under the pointer (seconds ago, text).
fn graph_hover(ui: &mut Ui, r: Rect, count: usize, label: impl Fn(usize) -> String) {
    if count < 2 || !ui.hovered(r) {
        return;
    }
    let t = ui.t;
    let mx = ui.mouse().0;
    let k = (((mx - r.x) / r.w) * (HISTORY - 1) as f32).round() as i64 - (HISTORY - count) as i64;
    let Ok(i) = usize::try_from(k) else { return };
    if i >= count {
        return;
    }
    let x = r.x + r.w * (HISTORY - count + i) as f32 / (HISTORY - 1) as f32;
    ui.g.line(x, r.y, x, r.bottom(), ui.c(with_alpha(t.text, 0.35)), 1.0);
    let ago = count - 1 - i;
    let text = format!("{}  ·  {}", label(i), if ago == 0 { "now".to_string() } else { format!("{ago}s ago") });
    let w = ui.measure(&text, 11.0, SEMIBOLD) + 16.0;
    let bx = (x - w / 2.0).clamp(r.x, r.right() - w);
    let b = Rect::new(bx, r.y - 2.0, w, 20.0);
    ui.fill(b, 6.0, t.surface2);
    ui.text(&text, b, 11.0, SEMIBOLD, t.text, Align::Center);
    ui.animate = true;
}

/// "1/2"-style chip that cycles through `n` items; returns the next index when clicked.
fn cycle_chip(ui: &mut Ui, r: Rect, i: usize, n: usize) -> Option<usize> {
    let t = ui.t;
    let hov = ui.hovered(r);
    ui.fill(r, r.h / 2.0, if hov { mix(t.surface2, t.text, 0.1) } else { t.surface2 });
    ui.text(&format!("{}/{}", i + 1, n), r, 10.5, SEMIBOLD, t.subtext, Align::Center);
    ui.clicked(r).then_some((i + 1) % n)
}

fn card_bg(ui: &mut Ui, r: Rect) -> bool {
    let hov = ui.hovered(r);
    ui.card(r, 18.0);
    if hov {
        ui.fill(r, 18.0, with_alpha(ui.t.text, 0.025));
    }
    hov
}

fn performance(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let hw = hwmon::snapshot();
    let ready = hw.samples > 1;
    let procs = s.data.lock().unwrap().procs.clone();
    let green = color(0x3DD68C, 1.0);
    let warm = color(0xF5A15A, 1.0);
    let (top, bottom) = r.take_top(120.0, 8.0);
    let cols = [0.34f32, 0.34, 0.16, 0.16];
    let gap = 8.0;
    let total_w = top.w - gap * 3.0;
    let mut x = top.x;
    let mut cells = Vec::new();
    for f in cols {
        let w = total_w * f;
        cells.push(Rect::new(x, top.y, w, top.h));
        x += w + gap;
    }

    // ---- CPU: ring, name, clock; per-thread bars or (click) the last minute.
    let c = cells[0];
    let hov = card_bg(ui, c);
    let inner = c.pad(14.0, 12.0, 14.0, 12.0);
    ring(ui, inner.x + 20.0, inner.y + 20.0, 17.0, hw.cpu / 100.0, t.accent, 3.5);
    ui.icon("Cpu", inner.x + 20.0, inner.y + 20.0, 14.0, t.text);
    let tx = inner.x + 48.0;
    ui.text("CPU", Rect::new(tx, inner.y + 1.0, 80.0, 20.0), 15.0, SEMIBOLD, t.text, Align::Left);
    let ghz = if hw.mhz > 0 { format!(" · {:.2} GHz", hw.mhz as f32 / 1000.0) } else { String::new() };
    // Temperature when something can read it (LibreHardwareMonitor, or a believable Windows sensor).
    let temp = hw.cpu_temp.map_or(String::new(), |c| format!(" · {c:.0} °C"));
    let spec = format!("{}C {}T{ghz}{temp}", hw.cores, hw.logical);
    ui.text(&spec, Rect::new(tx, inner.y + 21.0, inner.w - 48.0 - 58.0, 15.0), 11.0, REGULAR, t.subtext, Align::Left);
    let pct = if ready { format!("{:.0}%", hw.cpu) } else { "–".into() };
    ui.text(&pct, Rect::new(inner.right() - 56.0, inner.y - 1.0, 56.0, 22.0), 18.0, SEMIBOLD, t.text, Align::Right);
    let area = Rect::new(inner.x, inner.y + 46.0, inner.w, inner.bottom() - inner.y - 46.0);
    if s.cpu_history {
        let v: Vec<f32> = hw.cpu_history.iter().copied().collect();
        graph(ui, area, &v, 100.0, t.accent, true);
        graph_hover(ui, area, v.len(), |i| format!("{:.0}%", v[i]));
    } else if !hw.threads.is_empty() {
        let n = hw.threads.len() as f32;
        let bw = ((area.w - (n - 1.0) * 3.0) / n).max(2.0);
        for (i, v) in hw.threads.iter().enumerate() {
            let bx = area.x + i as f32 * (bw + 3.0);
            ui.fill(Rect::new(bx, area.y, bw, area.h), 2.0, t.track);
            let h = (area.h * v / 100.0).max(2.0);
            ui.fill(Rect::new(bx, area.bottom() - h, bw, h), 2.0, if *v >= 85.0 { warm } else { t.accent });
        }
        if ui.hovered(area) {
            let i = (((ui.mouse().0 - area.x) / (bw + 3.0)) as usize).min(hw.threads.len() - 1);
            let text = format!("Thread {}: {:.0}%", i + 1, hw.threads[i]);
            let w = ui.measure(&text, 11.0, SEMIBOLD) + 16.0;
            let bx = (area.x + i as f32 * (bw + 3.0) - w / 2.0).clamp(area.x, area.right() - w);
            let b = Rect::new(bx, area.y - 4.0, w, 20.0);
            ui.fill(b, 6.0, t.surface2);
            ui.text(&text, b, 11.0, SEMIBOLD, t.text, Align::Center);
        }
    }
    let _ = hov;
    if ui.clicked(c) {
        s.cpu_history = !s.cpu_history;
    }

    // ---- GPU: ring, name, temperature and video memory bars, the last minute on hover.
    let c = cells[1];
    let hov = card_bg(ui, c);
    let inner = c.pad(14.0, 12.0, 14.0, 12.0);
    if hw.gpus.is_empty() {
        ui.text("No GPU found", inner, 12.0, REGULAR, t.subtext, Align::Center);
    } else {
        let gi = s.gpu.min(hw.gpus.len() - 1);
        let g = &hw.gpus[gi];
        ring(ui, inner.x + 20.0, inner.y + 20.0, 17.0, g.usage / 100.0, color(0x5AD1C8, 1.0), 3.5);
        ui.icon("Monitor", inner.x + 20.0, inner.y + 20.0, 13.0, t.text);
        let tx = inner.x + 48.0;
        ui.text("GPU", Rect::new(tx, inner.y + 1.0, 40.0, 20.0), 15.0, SEMIBOLD, t.text, Align::Left);
        if hw.gpus.len() > 1 {
            if let Some(n) = cycle_chip(ui, Rect::new(tx + 40.0, inner.y + 2.0, 32.0, 18.0), gi, hw.gpus.len()) {
                s.gpu = n;
            }
        }
        ui.text(&g.name, Rect::new(tx, inner.y + 21.0, inner.w - 48.0 - 58.0, 15.0), 11.0, REGULAR, t.subtext, Align::Left);
        ui.text(&format!("{:.0}%", g.usage), Rect::new(inner.right() - 56.0, inner.y - 1.0, 56.0, 22.0), 18.0, SEMIBOLD, t.text, Align::Right);
        let area = Rect::new(inner.x, inner.y + 46.0, inner.w, inner.bottom() - inner.y - 46.0);
        if hov {
            let v: Vec<f32> = g.history.iter().copied().collect();
            graph(ui, area, &v, 100.0, color(0x5AD1C8, 1.0), true);
            graph_hover(ui, area, v.len(), |i| format!("{:.0}%", v[i]));
        } else {
            // Two bars: temperature (when the driver says) and video memory.
            let mut y = area.y + 6.0;
            let mut bar_row = |ui: &mut Ui, icon: &'static str, text: String, frac: f32, c: Color| {
                ui.icon(icon, inner.x + 6.0, y + 7.0, 12.0, t.subtext);
                ui.text(&text, Rect::new(inner.x + 18.0, y, 110.0, 15.0), 11.5, SEMIBOLD, t.text, Align::Left);
                let b = Rect::new(inner.x + 130.0, y + 5.0, (inner.w - 130.0).max(20.0), 5.0);
                ui.fill(b, 2.5, t.track);
                ui.fill(Rect::new(b.x, b.y, (b.w * frac.clamp(0.0, 1.0)).max(5.0), b.h), 2.5, c);
                y += 22.0;
            };
            if let Some(temp) = g.temp {
                let c = if temp >= 80.0 { t.danger } else if temp >= 65.0 { warm } else { green };
                bar_row(ui, "Sun", format!("{temp:.0} °C"), temp / 100.0, c);
            }
            if g.vram_total > 0 {
                bar_row(ui, "Memory", format!("{} / {}", gb(g.vram_used), gb(g.vram_total)), g.vram_used as f32 / g.vram_total as f32, t.accent);
            }
        }
    }

    // ---- RAM ring.
    let c = cells[2];
    let hov = card_bg(ui, c);
    let frac = if hw.ram_total > 0 { hw.ram_used as f32 / hw.ram_total as f32 } else { 0.0 };
    let (cx, cy) = (c.cx(), c.y + 46.0);
    if hov {
        let v: Vec<f32> = hw.ram_history.iter().copied().collect();
        let area = Rect::new(c.x + 12.0, c.y + 14.0, c.w - 24.0, 62.0);
        graph(ui, area, &v, 100.0, color(0x8AB4FF, 1.0), true);
        graph_hover(ui, area, v.len(), |i| format!("{:.0}%", v[i]));
    } else {
        ring(ui, cx, cy, 30.0, frac, color(0x8AB4FF, 1.0), 6.0);
        ui.text(&format!("{:.0}%", frac * 100.0), Rect::new(cx - 30.0, cy - 11.0, 60.0, 22.0), 15.0, SEMIBOLD, t.text, Align::Center);
    }
    ui.text("RAM", Rect::new(c.x, c.y + 82.0, c.w, 15.0), 11.5, SEMIBOLD, t.text, Align::Center);
    ui.text(&format!("{} / {}", gb(hw.ram_used), gb(hw.ram_total)), Rect::new(c.x + 4.0, c.y + 97.0, c.w - 8.0, 14.0), 10.5, REGULAR, t.subtext, Align::Center);

    // ---- Drive ring, cycling through the fixed drives.
    let c = cells[3];
    card_bg(ui, c);
    if hw.disks.is_empty() {
        ui.text("No drives", c, 11.0, REGULAR, t.subtext, Align::Center);
    } else {
        let di = s.disk.min(hw.disks.len() - 1);
        let d = &hw.disks[di];
        let frac = d.used as f32 / d.total.max(1) as f32;
        let col = if frac >= 0.9 { t.danger } else { color(0xE5A8D6, 1.0) };
        ring(ui, cx_of(c), c.y + 46.0, 30.0, frac, col, 6.0);
        ui.text(&format!("{:.0}%", frac * 100.0), Rect::new(cx_of(c) - 30.0, c.y + 35.0, 60.0, 22.0), 15.0, SEMIBOLD, t.text, Align::Center);
        let label = format!("{}:", d.letter);
        if hw.disks.len() > 1 {
            let lw = ui.measure(&label, 11.5, SEMIBOLD);
            let total = lw + 6.0 + 30.0;
            let lx = c.cx() - total / 2.0;
            ui.text(&label, Rect::new(lx, c.y + 82.0, lw + 2.0, 15.0), 11.5, SEMIBOLD, t.text, Align::Left);
            if let Some(n) = cycle_chip(ui, Rect::new(lx + lw + 6.0, c.y + 81.0, 30.0, 16.0), di, hw.disks.len()) {
                s.disk = n;
            }
        } else {
            ui.text(&label, Rect::new(c.x, c.y + 82.0, c.w, 15.0), 11.5, SEMIBOLD, t.text, Align::Center);
        }
        let free = format!("{} / {}", gb(d.used), gb(d.total));
        ui.text(&free, Rect::new(c.x + 4.0, c.y + 97.0, c.w - 8.0, 14.0), 10.5, REGULAR, t.subtext, Align::Center);
    }

    // ---- Bottom: network graph and three small cards on the left, top processes on the right.
    let (left, right) = bottom.take_left((bottom.w - gap) * 0.66, gap);
    let (net, smalls) = left.take_top((left.h - gap) * 0.5, gap);

    let hov = card_bg(ui, net);
    let inner = net.pad(14.0, 10.0, 14.0, 10.0);
    ui.icon("Wifi", inner.x + 6.0, inner.y + 8.0, 13.0, t.accent);
    ui.text("Net", Rect::new(inner.x + 18.0, inner.y, 40.0, 16.0), 12.5, SEMIBOLD, t.text, Align::Left);
    let rates = if ready { format!("↓ {}    ↑ {}", system::rate(hw.net_down), system::rate(hw.net_up)) } else { "–".into() };
    ui.text(&rates, Rect::new(inner.x + 52.0, inner.y, inner.w - 130.0, 16.0), 11.5, REGULAR, t.subtext, Align::Left);
    let peak = hw.net_history.iter().map(|(d, u)| (*d).max(*u)).max().unwrap_or(0).max(64 * 1024);
    ui.text(&system::rate(peak), Rect::new(inner.right() - 80.0, inner.y, 80.0, 16.0), 11.0, SEMIBOLD, t.accent, Align::Right);
    let area = Rect::new(inner.x, inner.y + 22.0, inner.w, inner.h - 22.0);
    let down: Vec<f32> = hw.net_history.iter().map(|(d, _)| *d as f32).collect();
    let up: Vec<f32> = hw.net_history.iter().map(|(_, u)| *u as f32).collect();
    ui.g.line(area.x, area.bottom(), area.right(), area.bottom(), ui.c(t.track), 1.0);
    graph(ui, area, &down, peak as f32, t.accent, true);
    graph(ui, area, &up, peak as f32, green, false);
    if hov {
        let hist: Vec<(u64, u64)> = hw.net_history.iter().copied().collect();
        graph_hover(ui, area, hist.len(), |i| format!("↓ {}  ↑ {}", system::rate(hist[i].0), system::rate(hist[i].1)));
    }

    let small = smalls.columns(3, gap);
    // Battery (or AC power).
    let inner = small_card(ui, small[0], if hw.battery.map_or(true, |(_, p)| p) { "Bolt" } else { "BatteryFull" }, "Battery", green);
    match hw.battery {
        Some((pct, plugged)) => {
            big_value(ui, inner, &format!("{pct}%"));
            let text = if plugged {
                if pct >= 99 { "Fully charged".to_string() } else { "Charging".to_string() }
            } else {
                match hw.battery_secs {
                    Some(secs) if secs > 0 => format!("{} h {} min left", secs / 3600, secs / 60 % 60),
                    _ => "On battery".to_string(),
                }
            };
            sub_value(ui, inner, &text);
        }
        None => {
            big_value(ui, inner, "AC");
            sub_value(ui, inner, "No battery");
        }
    }
    // Ping.
    let inner = small_card(ui, small[1], "Wifi", "Ping", t.accent);
    match hw.ping {
        Some(ms) => {
            big_value(ui, inner, &format!("{ms} ms"));
            sub_value(ui, inner, "1.1.1.1");
        }
        None => {
            big_value(ui, inner, "–");
            sub_value(ui, inner, if ready { "No reply" } else { "1.1.1.1" });
        }
    }
    // Uptime.
    let inner = small_card(ui, small[2], "Clock", "Uptime", warm);
    let ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let (days, hours, mins) = (ms / 86_400_000, ms / 3_600_000 % 24, ms / 60_000 % 60);
    big_value(ui, inner, &if days >= 1 { format!("{days}d {hours}h") } else { format!("{hours}h {mins}m") });
    sub_value(ui, inner, "since start");

    // Top processes, by CPU or memory; a click opens Apps sorted the same way.
    card_bg(ui, right);
    let inner = right.pad(12.0, 10.0, 12.0, 8.0);
    caps(ui, "Processes", Rect::new(inner.x, inner.y + 2.0, 100.0, 16.0), 10.5, Align::Left);
    let seg = Rect::new(inner.right() - 116.0, inner.y, 116.0, 22.0);
    ui.fill(seg, 11.0, t.surface2);
    for (i, label) in ["CPU", "Memory"].iter().enumerate() {
        let cell = Rect::new(seg.x + 2.0 + i as f32 * 56.0, seg.y + 2.0, 56.0, 18.0);
        let on = (i == 1) == s.proc_by_mem;
        if on {
            ui.fill(cell, 9.0, mix(t.surface2, t.text, 0.14));
        }
        ui.text(label, cell, 10.5, SEMIBOLD, if on { t.text } else { t.subtext }, Align::Center);
        if ui.clicked(cell) {
            s.proc_by_mem = i == 1;
        }
    }
    let mut list = procs;
    if s.proc_by_mem {
        list.sort_by(|a, b| b.mem.cmp(&a.mem));
    } else {
        list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
    }
    let rows_top = inner.y + 28.0;
    let row_h = 22.0;
    let n = (((inner.bottom() - rows_top) / row_h).floor() as usize).max(1);
    if list.is_empty() {
        ui.text("Reading…", Rect::new(inner.x, rows_top, inner.w, 20.0), 11.0, REGULAR, t.subtext, Align::Left);
    }
    for (i, p) in list.iter().take(n).enumerate() {
        let row = Rect::new(inner.x - 4.0, rows_top + i as f32 * row_h, inner.w + 8.0, row_h);
        if ui.hovered(row) {
            ui.fill(row, 6.0, t.surface2);
        }
        let ic = Rect::new(row.x + 6.0, row.cy() - 7.0, 14.0, 14.0);
        let px = (14.0 * ui.g.scale).round() as u32;
        let icon = if p.path.is_empty() { appicons::pid(p.pid, px) } else { appicons::exe(&p.path, px) };
        let drawn = icon.is_some_and(|(k, png)| ui.g.load_image(k, &png) && {
            ui.g.image(k, ic.d2d(), 0.0, ui.alpha);
            true
        });
        if !drawn {
            ui.icon("Cpu", ic.cx(), ic.cy(), 12.0, t.subtext);
        }
        let value = if s.proc_by_mem { mem_text(p.mem) } else { format!("{:.1}%", p.cpu) };
        ui.text(&p.title, Rect::new(ic.right() + 8.0, row.y, row.w - 94.0, row_h), 11.5, REGULAR, t.text, Align::Left);
        ui.text(&value, Rect::new(row.right() - 70.0, row.y, 64.0, row_h), 11.5, SEMIBOLD, t.text, Align::Right);
        if ui.clicked(row) {
            s.mode = Mode::Apps;
            s.sort = if s.proc_by_mem { Sort::Memory } else { Sort::Cpu };
        }
    }
    let _ = go;
}

fn cx_of(r: Rect) -> f32 {
    r.cx()
}

fn small_card(ui: &mut Ui, r: Rect, icon: &'static str, label: &str, c: Color) -> Rect {
    let t = ui.t;
    card_bg(ui, r);
    let inner = r.pad(12.0, 9.0, 12.0, 8.0);
    ui.icon(icon, inner.x + 6.0, inner.y + 7.0, 12.0, c);
    ui.text(label, Rect::new(inner.x + 18.0, inner.y, inner.w - 18.0, 15.0), 11.5, SEMIBOLD, t.subtext, Align::Left);
    inner
}

fn big_value(ui: &mut Ui, inner: Rect, v: &str) {
    ui.text(v, Rect::new(inner.x, inner.y + 16.0, inner.w, 26.0), 19.0, SEMIBOLD, ui.t.text, Align::Left);
}

fn sub_value(ui: &mut Ui, inner: Rect, v: &str) {
    ui.text(v, Rect::new(inner.x, inner.y + 42.0, inner.w, 14.0), 10.5, REGULAR, ui.t.subtext, Align::Left);
}

// ------------------------------------------------------------------ apps

fn mem_text(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    }
}

fn apps(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let (procs, first, mem_pct, message) = {
        let d = s.data.lock().unwrap();
        let hw = hwmon::snapshot();
        let mem = (hw.ram_total > 0).then(|| hw.ram_used as f64 * 100.0 / hw.ram_total as f64);
        (d.procs.clone(), d.proc_samples < 2, mem, d.message.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 5).map(|(m, _)| m.clone()))
    };
    // Inside the C# view's 18 px side margins.
    let r = r.pad(18.0, 0.0, 18.0, 4.0);
    let (head, rest) = r.take_top(30.0, 0.0);

    // Totals: "CPU 3%  ·  Memory 51%   58 apps".
    let cpu_total: f32 = procs.iter().map(|p| p.cpu).sum::<f32>().min(100.0);
    let cpu = if first { "CPU …".to_string() } else { format!("CPU {cpu_total:.0}%") };
    let mem = mem_pct.map_or("Memory".to_string(), |m| format!("Memory {m:.0}%"));
    let mut x = head.x;
    let w = ui.measure(&cpu, 12.0, SEMIBOLD);
    ui.text(&cpu, Rect::new(x, head.y, w + 4.0, head.h), 12.0, SEMIBOLD, t.text, Align::Left);
    x += w;
    let sep = "  ·  ";
    let w = ui.measure(sep, 12.0, SEMIBOLD);
    ui.text(sep, Rect::new(x, head.y, w + 2.0, head.h), 12.0, SEMIBOLD, t.subtext, Align::Left);
    x += w;
    let w = ui.measure(&mem, 12.0, SEMIBOLD);
    ui.text(&mem, Rect::new(x, head.y, w + 4.0, head.h), 12.0, SEMIBOLD, t.text, Align::Left);
    x += w + 10.0;

    // Sort chips and the Task Manager arrow.
    let arrow = Rect::new(head.right() - 26.0, head.cy() - 13.0, 26.0, 26.0);
    if ui.icon_button(arrow, "ArrowRight", 13.0, t.subtext) {
        crate::shell::open("taskmgr.exe");
    }
    let mut cx = arrow.x - 4.0;
    for (sort, label) in [(Sort::Name, "Name"), (Sort::Memory, "Memory"), (Sort::Cpu, "CPU")] {
        let cw = ui.measure(label, 11.0, SEMIBOLD) + 20.0;
        cx -= cw + 4.0;
        let c = Rect::new(cx, head.cy() - 13.0, cw, 26.0);
        let on = s.sort == sort;
        let hov = ui.hovered(c);
        if on {
            ui.fill(c, 8.0, t.accent_soft);
        } else if hov {
            ui.fill(c, 8.0, t.surface2);
        }
        ui.text(label, c, 11.0, SEMIBOLD, if on { t.text } else { t.subtext }, Align::Center);
        if ui.clicked(c) {
            s.sort = sort;
        }
    }
    let count = message.unwrap_or_else(|| format!("{} apps", procs.len()));
    ui.text(&count, Rect::new(x, head.y, (cx - 8.0 - x).max(0.0), head.h), 11.0, REGULAR, t.subtext, Align::Left);

    // Column captions (margin 10,6,14,4).
    let (caps_r, list) = rest.take_top(6.0 + 14.0 + 4.0, 0.0);
    let caps_r = Rect::new(caps_r.x + 10.0, caps_r.y + 6.0, caps_r.w - 24.0, 14.0);
    let cols = Columns::new(caps_r);
    caps(ui, "App", Rect::new(caps_r.x + 30.0, caps_r.y, 100.0, caps_r.h), 10.0, Align::Left);
    caps(ui, "CPU", cols.cpu, 10.0, Align::Right);
    caps(ui, "Memory", cols.mem, 10.0, Align::Right);

    let mut sorted = procs;
    match s.sort {
        Sort::Cpu => sorted.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem))),
        Sort::Memory => sorted.sort_by(|a, b| b.mem.cmp(&a.mem)),
        Sort::Name => sorted.sort_by_key(|p| p.title.to_lowercase()),
    }
    sorted.truncate(MAX_ROWS);
    if sorted.is_empty() {
        ui.text("Reading running apps…", list, 12.0, REGULAR, t.subtext, Align::Center);
        return;
    }

    let row_h = 32.0;
    let mut end: Option<Proc> = None;
    let mut arm: Option<Option<String>> = None;
    let confirming = s.confirming.clone();
    ui.scroll(id("sys.apps"), list, row_h * sorted.len() as f32, |ui, top| {
        let t = ui.t;
        for (i, p) in sorted.iter().enumerate() {
            let row = Rect::new(list.x, top + i as f32 * row_h, list.w - 8.0, row_h);
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let hov = ui.hovered(row);
            if hov {
                ui.fill(row, 8.0, t.surface);
            }
            let cols = Columns::new(row.pad(10.0, 0.0, 4.0, 0.0));
            // The exe's icon; a CPU glyph until it loads (or when there's none).
            let ic = Rect::new(cols.icon.x, row.cy() - 8.0, 16.0, 16.0);
            let px = (16.0 * ui.g.scale).round() as u32;
            let icon = if p.path.is_empty() { appicons::pid(p.pid, px) } else { appicons::exe(&p.path, px) };
            let drawn = icon.is_some_and(|(k, png)| {
                ui.g.load_image(k, &png) && {
                    ui.g.image(k, ic.d2d(), 0.0, ui.alpha);
                    true
                }
            });
            if !drawn {
                ui.icon("Cpu", ic.cx(), ic.cy(), 14.0, t.subtext);
            }
            let name = if p.pids.len() > 1 { format!("{}  ({})", p.title, p.pids.len()) } else { p.title.clone() };
            ui.text(&name, cols.name, 12.0, REGULAR, t.text, Align::Left);
            let cpu_c = if p.cpu >= 25.0 { t.danger } else if p.cpu >= 5.0 { t.text } else { t.subtext };
            ui.text(&if first { "…".to_string() } else { format!("{:.1}%", p.cpu) }, cols.cpu, 12.0, SEMIBOLD, cpu_c, Align::Right);
            ui.text(&mem_text(p.mem), cols.mem, 12.0, SEMIBOLD, if p.mem >= 1 << 30 { t.text } else { t.subtext }, Align::Right);
            let armed = confirming.as_deref() == Some(p.name.as_str());
            if hov || armed {
                let b = Rect::new(cols.end.right() - 70.0, row.cy() - 12.0, 70.0, 24.0);
                let clicked = if armed {
                    let hb = ui.hovered(b);
                    ui.fill(b, 8.0, if hb { mix(t.danger, t.text, 0.12) } else { t.danger });
                    ui.text("Sure?", b, 11.0, SEMIBOLD, color(0xFFFFFF, 1.0), Align::Center);
                    ui.clicked(b)
                } else {
                    ui.button(b, "End task", ButtonStyle::Normal)
                };
                if clicked {
                    if armed {
                        end = Some(p.clone());
                    } else {
                        arm = Some(Some(p.name.clone()));
                    }
                }
            }
        }
    });
    if let Some(a) = arm {
        s.confirming = a;
    }
    if let Some(p) = end {
        s.confirming = None;
        let data = s.data.clone();
        bg::spawn(move || {
            let failed = system::end_all(&p.pids);
            let mut d = data.lock().unwrap();
            d.message = (failed > 0).then(|| (format!("Couldn't end {} (it may need administrator rights)", p.title), Instant::now()));
            d.procs.retain(|x| x.name != p.name || failed > 0);
        });
    }
}

/// Column layout shared by the caption row and the list rows (like the .NET grid: 26 | * | 64 | 84 | 86).
struct Columns {
    icon: Rect,
    name: Rect,
    cpu: Rect,
    mem: Rect,
    end: Rect,
}

impl Columns {
    fn new(r: Rect) -> Self {
        let (end, rest) = r.take_right(86.0, 0.0);
        let (mem, rest) = rest.take_right(84.0, 0.0);
        let (cpu, rest) = rest.take_right(64.0, 0.0);
        let (icon, name) = rest.take_left(26.0, 0.0);
        Self { icon, name: name.pad(0.0, 0.0, 8.0, 0.0), cpu, mem, end }
    }
}

/// Small uppercase caption in the C# "Caps" style (bold, subtext).
fn caps(ui: &mut Ui, s: &str, r: Rect, size: f32, align: Align) {
    ui.text(&s.to_uppercase(), r, size, BOLD, ui.t.subtext, align);
}
