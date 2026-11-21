//! The tools (tabs): id, title, tab icon and the height their content wants.

pub struct Tool {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub height: f32,
}

/// Content width of the open panel (DIP, before "Panel size" scaling).
pub const CONTENT_W: f32 = 720.0;

pub const TOOLS: &[Tool] = &[
    Tool { id: "home", title: "", icon: "Grid", height: 220.0 },
    Tool { id: "control", title: "Control", icon: "Adjust", height: 236.0 },
    Tool { id: "shelf", title: "Shelf", icon: "ArchiveBox", height: 220.0 },
    Tool { id: "phone", title: "Phone", icon: "DevicePhone", height: 220.0 },
    Tool { id: "clipboard", title: "Clipboard", icon: "ClipboardList", height: 220.0 },
    Tool { id: "teleprompter", title: "Teleprompter", icon: "Teleprompter", height: 220.0 },
    Tool { id: "shortcuts", title: "Shortcuts", icon: "Stack", height: 220.0 },
    Tool { id: "nowplaying", title: "Now Playing", icon: "MusicNote", height: 220.0 },
    Tool { id: "note", title: "Quick Note", icon: "BookOpen", height: 220.0 },
    Tool { id: "todos", title: "To-Dos", icon: "ListBullet", height: 220.0 },
    Tool { id: "timer", title: "Timers", icon: "Clock", height: 220.0 },
    Tool { id: "calendar", title: "Calendar", icon: "CalendarDays", height: 245.0 },
    Tool { id: "prayer", title: "Prayer", icon: "Moon", height: 220.0 },
    Tool { id: "notifications", title: "Notifications", icon: "Bell", height: 220.0 },
    Tool { id: "weather", title: "Weather", icon: "Cloud", height: 245.0 },
    Tool { id: "system", title: "Hardware", icon: "Cpu", height: 330.0 },
    Tool { id: "emoji", title: "Emoji", icon: "FaceSmile", height: 245.0 },
];

/// Settings opens taller than the tools, without a tab bar (as the C# app).
pub const SETTINGS_H: f32 = 438.0;
pub const SETTINGS: Tool = Tool { id: "settings", title: "Settings", icon: "Gear", height: SETTINGS_H };

/// Tallest a resizable tool may get (content DIP).
pub const MAX_H: f32 = 340.0;
pub const MIN_H: f32 = 160.0;

/// Tools whose height the user drags (saved in settings, as the C# app did).
pub fn resizable(id: &str) -> bool {
    matches!(id, "note" | "notifications" | "phone" | "teleprompter")
}

/// Content height of tool `id`, including the user's resize.
pub fn height(id: &str) -> f32 {
    let s = crate::services::settings::get();
    let h = match id {
        "note" => s.note_height,
        "notifications" => s.notifications_height,
        "phone" => s.phone_height,
        "teleprompter" => s.prompter_height,
        _ => return find(id).height,
    };
    (h as f32).clamp(MIN_H, MAX_H)
}

pub fn set_height(id: &str, h: f32, save: bool) {
    let h = h.clamp(MIN_H, MAX_H).round() as f64;
    let f = |s: &mut crate::services::settings::Settings| match id {
        "note" => s.note_height = h,
        "notifications" => s.notifications_height = h,
        "phone" => s.phone_height = h,
        "teleprompter" => s.prompter_height = h,
        _ => {}
    };
    if save {
        crate::services::settings::update(f);
    } else {
        crate::services::settings::update_quiet(f);
    }
}

pub fn find(id: &str) -> &'static Tool {
    TOOLS.iter().find(|t| t.id == id).unwrap_or(if id == "settings" { &SETTINGS } else { &TOOLS[0] })
}

/// Tools in the user's order (Settings → Tabs), hidden ones left out.
pub fn ordered(order: &[String], hidden: &[String]) -> Vec<&'static Tool> {
    let mut out: Vec<&'static Tool> = order.iter().filter_map(|id| TOOLS.iter().find(|t| t.id == id)).collect();
    for t in TOOLS {
        if !out.iter().any(|o| o.id == t.id) {
            out.push(t);
        }
    }
    out.retain(|t| !hidden.iter().any(|h| h == t.id));
    if out.is_empty() {
        out.push(&TOOLS[0]);
    }
    out
}
