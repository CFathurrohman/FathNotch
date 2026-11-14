//! The tools' contents. Each view draws into the content rectangle it's given and handles its own clicks.
//! Views keep their state in their own `State`; data comes from `crate::model` and `crate::services`.

pub mod alarm;
pub mod calendar;
pub mod clipboard;
pub mod control;
pub mod emoji;
pub mod home;
pub mod note;
pub mod notifications;
pub mod nowplaying;
pub mod phone;
pub mod picker;
pub mod prayer;
pub mod settings;
pub mod shelf;
pub mod shortcuts;
pub mod system;
pub mod teleprompter;
pub mod timer;
pub mod todos;
pub mod weather;

use crate::ui::{Rect, Ui};

#[derive(Default)]
pub struct Views {
    pub home: home::State,
    pub control: control::State,
    pub shelf: shelf::State,
    pub phone: phone::State,
    pub clipboard: clipboard::State,
    pub teleprompter: teleprompter::State,
    pub shortcuts: shortcuts::State,
    pub nowplaying: nowplaying::State,
    pub note: note::State,
    pub todos: todos::State,
    pub timer: timer::State,
    pub calendar: calendar::State,
    pub prayer: prayer::State,
    pub notifications: notifications::State,
    pub weather: weather::State,
    pub system: system::State,
    pub emoji: emoji::State,
    pub settings: settings::State,
}

/// Draws tool `id`. A view can ask to switch tools by setting `go`.
pub fn draw(id: &str, ui: &mut Ui, v: &mut Views, r: Rect, go: &mut Option<&'static str>) {
    match id {
        "home" => home::draw(ui, &mut v.home, r, go),
        "control" => control::draw(ui, &mut v.control, r, go),
        "shelf" => shelf::draw(ui, &mut v.shelf, r, go),
        "phone" => phone::draw(ui, &mut v.phone, r, go),
        "clipboard" => clipboard::draw(ui, &mut v.clipboard, r, go),
        "teleprompter" => teleprompter::draw(ui, &mut v.teleprompter, r, go),
        "shortcuts" => shortcuts::draw(ui, &mut v.shortcuts, r, go),
        "nowplaying" => nowplaying::draw(ui, &mut v.nowplaying, r, go),
        "note" => note::draw(ui, &mut v.note, r, go),
        "todos" => todos::draw(ui, &mut v.todos, r, go),
        "timer" => timer::draw(ui, &mut v.timer, r, go),
        "calendar" => calendar::draw(ui, &mut v.calendar, r, go),
        "prayer" => prayer::draw(ui, &mut v.prayer, r, go),
        "notifications" => notifications::draw(ui, &mut v.notifications, r, go),
        "weather" => weather::draw(ui, &mut v.weather, r, go),
        "system" => system::draw(ui, &mut v.system, r, go),
        "emoji" => emoji::draw(ui, &mut v.emoji, r, go),
        "settings" => settings::draw(ui, &mut v.settings, r, go),
        _ => {}
    }
}

/// A tool was opened: views refresh their data in the background here.
pub fn shown(id: &str, v: &mut Views) {
    match id {
        "home" => home::shown(&mut v.home),
        "control" => control::shown(&mut v.control),
        "shelf" => shelf::shown(&mut v.shelf),
        "phone" => phone::shown(&mut v.phone),
        "clipboard" => clipboard::shown(&mut v.clipboard),
        "teleprompter" => teleprompter::shown(&mut v.teleprompter),
        "shortcuts" => shortcuts::shown(&mut v.shortcuts),
        "nowplaying" => nowplaying::shown(&mut v.nowplaying),
        "note" => note::shown(&mut v.note),
        "todos" => todos::shown(&mut v.todos),
        "timer" => timer::shown(&mut v.timer),
        "calendar" => calendar::shown(&mut v.calendar),
        "prayer" => prayer::shown(&mut v.prayer),
        "notifications" => notifications::shown(&mut v.notifications),
        "weather" => weather::shown(&mut v.weather),
        "system" => system::shown(&mut v.system),
        "emoji" => emoji::shown(&mut v.emoji),
        "settings" => settings::shown(&mut v.settings),
        _ => {}
    }
}
