//! What's playing, from Windows' global media session (Spotify, browsers, Media Player, …), and the
//! transport controls. `start` subscribes once; the state is kept here and the UI reads `current()`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IInputStream};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};
use windows::core::Interface;

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Friendly name of the app playing ("Spotify", "Microsoft Edge", …).
    pub app: String,
    pub playing: bool,
    /// Seconds, as of `updated`.
    pub position: f64,
    /// Seconds; 0 when the player doesn't say.
    pub duration: f64,
    pub updated: Instant,
    /// Encoded thumbnail (PNG/JPEG) as the player supplied it.
    pub art: Option<Arc<Vec<u8>>>,
    /// Changes whenever the art changes (0 = no art).
    pub art_id: u64,
}

impl NowPlaying {
    fn empty(app: String) -> Self {
        NowPlaying {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            app,
            playing: false,
            position: 0.0,
            duration: 0.0,
            updated: Instant::now(),
            art: None,
            art_id: 0,
        }
    }

    /// The playhead now, extrapolated while playing.
    pub fn position_now(&self) -> f64 {
        let mut p = self.position;
        if self.playing {
            p += self.updated.elapsed().as_secs_f64();
        }
        if self.duration > 0.0 {
            p.clamp(0.0, self.duration)
        } else {
            p.max(0.0)
        }
    }
}

struct Hooked {
    session: Session,
    tokens: [EventRegistrationToken; 3],
}

struct State {
    manager: Option<Manager>,
    hooked: Option<Hooked>,
    now: Option<NowPlaying>,
    /// The last seek (wall-clock ticks, and when): timeline reports older than it are stale.
    seek: Option<(i64, Instant, f64)>,
    /// Every player Windows knows about, as of the last look (for the source picker).
    sources: Vec<Source>,
    /// The player the user picked (session pointer, app id); None follows the best one.
    pinned: Option<(usize, String)>,
}

/// A player that can be shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub app: String,
    pub title: String,
    pub playing: bool,
    /// Whether it's the one being shown.
    pub shown: bool,
    raw: usize,
    aumid: String,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { manager: None, hooked: None, now: None, seek: None, sources: Vec::new(), pinned: None }))
}

/// The players to choose from (refreshed every couple of seconds), and whether one was picked by hand.
pub fn sources() -> (Vec<Source>, bool) {
    let s = lock();
    (s.sources.clone(), s.pinned.is_some())
}

/// Show this player (an entry from `sources`), or None to follow the best one again.
pub fn choose(source: Option<Source>) {
    lock().pinned = source.map(|s| (s.raw, s.aumid));
    crate::bg::spawn(|| {
        let m = lock().manager.clone();
        if let Some(m) = m {
            reselect(&m);
        }
    });
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// Subscribes to the media session (once). Safe to call again; later calls do nothing.
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return };
    let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(|m: &Option<Manager>, _| {
        if let Some(m) = m {
            reselect(m);
        }
        Ok(())
    }));
    // Some players come and go without the current session changing; re-check then too.
    let _ = manager.SessionsChanged(&TypedEventHandler::new(|m: &Option<Manager>, _| {
        if let Some(m) = m {
            reselect(m);
        }
        Ok(())
    }));
    lock().manager = Some(manager.clone());
    reselect(&manager);
    // A player starting or pausing doesn't always tell the manager: look again every couple of seconds.
    crate::bg::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        reselect(&manager);
    });
}

/// Follows the best player: music over video. Windows' own pick is simply the last one that changed,
/// so a YouTube video in a browser would push Spotify or YouTube Music (and their lyrics) aside.
fn reselect(m: &Manager) {
    let windows_pick = m.GetCurrentSession().ok();
    let mut sessions: Vec<Session> = m.GetSessions().map(|v| v.into_iter().collect()).unwrap_or_default();
    if sessions.is_empty() {
        sessions.extend(windows_pick.clone());
    }
    let (hooked, pinned) = {
        let s = lock();
        (s.hooked.as_ref().map(|h| h.session.as_raw()), s.pinned.clone())
    };
    let aumid = |s: &Session| s.SourceAppUserModelId().map(|a| a.to_string_lossy()).unwrap_or_default();
    // The player picked by hand wins while it's around: the same session, else the only one of that app.
    let picked = pinned.as_ref().and_then(|(raw, id)| {
        sessions.iter().find(|s| s.as_raw() as usize == *raw).cloned().or_else(|| {
            let same_app: Vec<&Session> = sessions.iter().filter(|s| aumid(s) == *id).collect();
            (same_app.len() == 1).then(|| same_app[0].clone())
        })
    });
    // Update the pin, unless the user changed it while we looked (choose() starts its own look then).
    {
        let mut st = lock();
        if st.pinned != pinned {
            return;
        }
        match (&picked, &pinned) {
            (Some(p), Some((_, id))) => st.pinned = Some((p.as_raw() as usize, id.clone())),
            (None, Some(_)) => st.pinned = None, // it closed: back to automatic
            _ => {}
        }
    }
    let scored: Vec<(i32, i32, Session)> = sessions
        .into_iter()
        .map(|s| {
            let raw = s.as_raw();
            // Ties: stay on the player we follow, then Windows' pick.
            let tie = if Some(raw) == hooked { 2 } else if windows_pick.as_ref().is_some_and(|w| w.as_raw() == raw) { 1 } else { 0 };
            (score(&s), tie, s)
        })
        .collect();
    let best = picked.or_else(|| scored.iter().max_by_key(|(sc, tie, _)| (*sc, *tie)).map(|(_, _, s)| s.clone()));
    // The list for the picker.
    let list: Vec<Source> = scored
        .iter()
        .map(|(sc, _, s)| {
            let id = aumid(s);
            let title = s.TryGetMediaPropertiesAsync().and_then(|op| op.get()).and_then(|p| p.Title()).map(|t| t.to_string_lossy()).unwrap_or_default();
            Source {
                app: pretty_app(&id),
                title,
                playing: *sc >= 100,
                shown: best.as_ref().is_some_and(|b| b.as_raw() == s.as_raw()),
                raw: s.as_raw() as usize,
                aumid: id,
            }
        })
        .collect();
    let changed = {
        let mut st = lock();
        let c = st.sources != list;
        st.sources = list;
        c
    };
    let same = match (&best, hooked) {
        (Some(b), Some(h)) => b.as_raw() == h,
        (None, None) => true,
        _ => false,
    };
    if !same {
        hook(best);
    } else if changed {
        crate::bg::refresh();
    }
}

/// How much a player is worth following: playing beats paused; a music app (or a browser tab that names
/// an album, as YouTube Music does) beats a video.
fn score(s: &Session) -> i32 {
    let playing = s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == Status::Playing);
    let aumid = s.SourceAppUserModelId().map(|a| a.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kind = if is_music_app(&aumid) {
        50
    } else if is_browser(&aumid) {
        let album = s.TryGetMediaPropertiesAsync().and_then(|op| op.get()).and_then(|p| p.AlbumTitle()).map(|a| !a.is_empty()).unwrap_or(false);
        if album { 40 } else { 0 }
    } else {
        20
    };
    kind + if playing { 100 } else { 0 }
}

fn is_music_app(aumid: &str) -> bool {
    const MUSIC: [&str; 18] = [
        "metrolist", "innertune", "vimusic", "namida", "spotify", "applemusic", "zune", "tidal", "deezer", "amazonmusic", "youtube-music", "youtubemusic",
        // YouTube Music installed as an app from Chrome or Edge.
        "cinhimbnkkaeohfgghhklpknlkffjgod", "musicbee", "foobar", "aimp", "winamp", "itunes",
    ];
    MUSIC.iter().any(|m| aumid.contains(m))
}

fn is_browser(aumid: &str) -> bool {
    ["msedge", "chrome", "firefox", "brave", "opera", "vivaldi"].iter().any(|b| aumid.contains(b)) && !aumid.contains("_crx_")
}

/// Follows `session` (or nothing), dropping the previous one's subscriptions.
fn hook(session: Option<Session>) {
    static HOOKING: Mutex<()> = Mutex::new(());
    let _serial = HOOKING.lock().unwrap_or_else(|e| e.into_inner());
    let old = {
        let mut s = lock();
        s.now = session.as_ref().map(|x| NowPlaying::empty(app_name(x)));
        s.hooked.take()
    };
    if let Some(old) = old {
        let _ = old.session.RemoveMediaPropertiesChanged(old.tokens[0]);
        let _ = old.session.RemovePlaybackInfoChanged(old.tokens[1]);
        let _ = old.session.RemoveTimelinePropertiesChanged(old.tokens[2]);
    }
    let Some(session) = session else {
        crate::bg::refresh();
        return;
    };
    let t0 = session
        .MediaPropertiesChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_media(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    let t1 = session
        .PlaybackInfoChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_playback(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    let t2 = session
        .TimelinePropertiesChanged(&TypedEventHandler::new(|s: &Option<Session>, _| {
            if let Some(s) = s {
                refresh_timeline(s);
            }
            Ok(())
        }))
        .unwrap_or_default();
    lock().hooked = Some(Hooked { session: session.clone(), tokens: [t0, t1, t2] });
    refresh_playback(&session);
    refresh_timeline(&session);
    refresh_media(&session);
}

/// Runs `f` on the shared state if `session` is still the one being followed, then asks for a redraw.
fn update(session: &Session, f: impl FnOnce(&mut NowPlaying)) {
    let mut s = lock();
    let current = s.hooked.as_ref().is_some_and(|h| h.session.as_raw() == session.as_raw());
    if !current {
        return;
    }
    if let Some(now) = s.now.as_mut() {
        f(now);
    }
    drop(s);
    crate::stat("media update");
    crate::bg::refresh();
}

fn refresh_media(session: &Session) {
    let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.get()) else { return };
    let text = |r: windows::core::Result<windows::core::HSTRING>| r.map(|s| s.to_string_lossy()).unwrap_or_default();
    let title = text(props.Title());
    let mut artist = text(props.Artist());
    let album = text(props.AlbumTitle());
    if artist.trim().is_empty() {
        artist = text(props.AlbumArtist());
    }
    // YouTube's auto-generated music channels: "Taylor Swift - Topic" → "Taylor Swift".
    if let Some(stripped) = artist.strip_suffix(" - Topic") {
        artist = stripped.to_string();
    }
    let art = props.Thumbnail().ok().and_then(|t| read_thumbnail(&t));
    let art_id = art.as_ref().map(|bytes| {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish() | 1
    });
    update(session, |now| {
        now.title = title;
        now.artist = artist;
        now.album = album;
        now.art_id = art_id.unwrap_or(0);
        now.art = art.map(Arc::new);
    });
}

fn read_thumbnail(r: &windows::Storage::Streams::IRandomAccessStreamReference) -> Option<Vec<u8>> {
    let stream = r.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let input: IInputStream = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let got = reader.LoadAsync(size).ok()?.get().ok()?;
    let mut bytes = vec![0u8; got as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    let _ = stream.cast::<windows::Foundation::IClosable>().map(|c| c.Close());
    (!bytes.is_empty()).then_some(bytes)
}

fn refresh_playback(session: &Session) {
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|s| s == Status::Playing)
        .unwrap_or(false);
    update(session, |now| {
        if now.playing != playing {
            // Freeze (or restart) the extrapolation at the moment the state flips.
            now.position = now.position_now();
            now.updated = Instant::now();
            now.playing = playing;
        }
    });
}

/// Current time as a Windows FILETIME-style tick count (100 ns since 1601), like WinRT DateTime.
fn now_ticks() -> i64 {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    EPOCH_DIFF + (since.as_nanos() / 100) as i64
}

fn refresh_timeline(session: &Session) {
    let Ok(t) = session.GetTimelineProperties() else { return };
    let ticks = |v: i64| v as f64 / 10_000_000.0;
    let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
    let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
    let pos = t.Position().map(|d| d.Duration).unwrap_or(0);
    let stamp = t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
    let duration = ticks((end - start).max(0));
    let mut position = ticks(pos - start).max(0.0);
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|s| s == Status::Playing)
        .unwrap_or(false);
    if playing && stamp > 0 {
        let age = ticks(now_ticks() - stamp);
        if age > 0.0 && age < 24.0 * 3600.0 {
            position += age;
        }
    }
    let stale = stale_after_seek(stamp, position);
    update(session, |now| {
        now.duration = duration;
        if stale {
            // Keep our post-seek playhead; only follow a play/pause change.
            if now.playing != playing {
                now.position = now.position_now();
                now.updated = Instant::now();
            }
        } else {
            now.position = if duration > 0.0 { position.min(duration) } else { position };
            now.updated = Instant::now();
        }
        now.playing = playing;
    });
}

/// "Spotify.exe" → "Spotify", "MSEdge" → "Microsoft Edge", "Microsoft.ZuneMusic_…!Microsoft.ZuneMusic" → "Media Player".
fn app_name(session: &Session) -> String {
    let aumid = session.SourceAppUserModelId().map(|s| s.to_string_lossy()).unwrap_or_default();
    pretty_app(&aumid)
}

fn pretty_app(aumid: &str) -> String {
    if aumid.is_empty() {
        return String::new();
    }
    let lower = aumid.to_lowercase();
    if lower.contains("zune") {
        return "Media Player".into();
    }
    let mut name = aumid.rsplit('!').next().unwrap_or(aumid);
    name = name.rsplit('\\').next().unwrap_or(name);
    if name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        name = &name[..name.len() - 4];
    }
    let first = name.split(['.', '_']).find(|s| !s.is_empty()).unwrap_or(name);
    let known = match first.to_lowercase().as_str() {
        "msedge" => Some("Microsoft Edge"),
        "chrome" => Some("Google Chrome"),
        "firefox" => Some("Firefox"),
        "spotify" | "spotifyab" => Some("Spotify"),
        "brave" => Some("Brave"),
        "opera" => Some("Opera"),
        "vlc" => Some("VLC"),
        "applemusic" | "appleinc" if lower.contains("music") => Some("Apple Music"),
        _ => None,
    };
    if let Some(k) = known {
        return k.into();
    }
    if lower.contains("spotify") {
        return "Spotify".into();
    }
    let mut c = first.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => aumid.to_string(),
    }
}

/// What's playing now, or None when no app has a media session.
pub fn current() -> Option<NowPlaying> {
    lock().now.clone()
}

fn session() -> Option<Session> {
    lock().hooked.as_ref().map(|h| h.session.clone())
}

/// Play or pause. Some players ignore "toggle" (or report it disabled) while they accept a plain Play or
/// Pause, and the hooked session can lag behind a player that just took over; so: ask for the specific
/// action the player says it supports, then toggle, and if the player still says no, press the keyboard's
/// media Play/Pause key, which Windows hands to whichever player is current.
pub fn play_pause() {
    let ok = session().is_some_and(|s| {
        let info = s.GetPlaybackInfo().ok();
        let playing = info.as_ref().and_then(|i| i.PlaybackStatus().ok()).is_some_and(|st| st == Status::Playing);
        let controls = info.as_ref().and_then(|i| i.Controls().ok());
        let can_pause = controls.as_ref().and_then(|c| c.IsPauseEnabled().ok()).unwrap_or(false);
        let can_play = controls.as_ref().and_then(|c| c.IsPlayEnabled().ok()).unwrap_or(false);
        let specific = if playing && can_pause {
            s.TryPauseAsync().and_then(|op| op.get()).unwrap_or(false)
        } else if !playing && can_play {
            s.TryPlayAsync().and_then(|op| op.get()).unwrap_or(false)
        } else {
            false
        };
        specific || s.TryTogglePlayPauseAsync().and_then(|op| op.get()).unwrap_or(false)
    });
    // As in skip(): the media key only when it would reach the player shown (or nothing is followed).
    let shown = session();
    let windows_pick = lock().manager.as_ref().and_then(|m| m.GetCurrentSession().ok());
    if !ok && shown.as_ref().map_or(true, |s| windows_pick.is_some_and(|w| w.as_raw() == s.as_raw())) {
        media_key(VK_MEDIA_PLAY_PAUSE);
    }
}

pub fn next() {
    skip(true);
}

pub fn previous() {
    skip(false);
}

/// Next or previous track on the player shown. If it says no, the keyboard's media key is the fallback,
/// but Windows hands that key to its own pick, so it's only pressed when that is the player shown;
/// otherwise the key would skip a track in another player (a YouTube tab instead of Metrolist).
fn skip(forward: bool) {
    let Some(s) = session() else {
        media_key(if forward { VK_MEDIA_NEXT_TRACK } else { VK_MEDIA_PREV_TRACK });
        return;
    };
    let ok = if forward { s.TrySkipNextAsync() } else { s.TrySkipPreviousAsync() }.and_then(|op| op.get()).unwrap_or(false);
    if ok {
        return;
    }
    let windows_pick = lock().manager.as_ref().and_then(|m| m.GetCurrentSession().ok());
    if windows_pick.is_some_and(|w| w.as_raw() == s.as_raw()) {
        media_key(if forward { VK_MEDIA_NEXT_TRACK } else { VK_MEDIA_PREV_TRACK });
    }
}

/// Presses and releases a media key (the player didn't take the request).
fn media_key(vk: VIRTUAL_KEY) {
    unsafe {
        keybd_event(vk.0 as u8, 0, KEYEVENTF_EXTENDEDKEY, 0);
        keybd_event(vk.0 as u8, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP, 0);
    }
}

/// Jumps to `seconds` into the track.
pub fn seek(seconds: f64) {
    let Some(s) = session() else { return };
    let seconds = seconds.max(0.0);
    // Show the new position right away (the request can take a moment), and remember the seek so a
    // timeline report the player made before it doesn't pull the playhead back.
    let before = current().map(|n| (n.position, n.updated));
    lock().seek = Some((now_ticks(), Instant::now(), seconds));
    update(&s, |now| {
        now.position = seconds;
        now.updated = Instant::now();
    });
    // The player counts from its timeline's start, which isn't always 0.
    let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|d| d.Duration).unwrap_or(0);
    let ticks = start + (seconds * 10_000_000.0) as i64;
    let ok = s.TryChangePlaybackPositionAsync(ticks).and_then(|op| op.get()).unwrap_or(false);
    if !ok {
        // The player said no: put the playhead back where it really is.
        lock().seek = None;
        if let Some((pos, at)) = before {
            update(&s, |now| {
                now.position = pos;
                now.updated = at;
            });
        }
    }
}

/// True when a timeline report should be ignored: made before the last seek, or (when the player
/// doesn't stamp its reports) arriving right after it with a position far from where we jumped.
fn stale_after_seek(stamp: i64, position: f64) -> bool {
    let Some((seek_ticks, at, target)) = lock().seek else { return false };
    let since = at.elapsed().as_secs_f64();
    if since > 6.0 {
        return false;
    }
    if stamp > 0 {
        return stamp < seek_ticks;
    }
    since < 2.0 && (position - (target + since)).abs() > 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_reports_from_before_a_seek() {
        let t = now_ticks();
        lock().seek = Some((t, Instant::now(), 120.0));
        assert!(stale_after_seek(t - 5_000_000, 30.0)); // stamped half a second before the seek
        assert!(!stale_after_seek(t + 5_000_000, 120.5)); // stamped after it: follow it
        assert!(stale_after_seek(0, 30.0)); // unstamped and far from the target, right after
        assert!(!stale_after_seek(0, 120.2)); // unstamped and where we jumped
        lock().seek = Some((t, Instant::now() - std::time::Duration::from_secs(10), 120.0));
        assert!(!stale_after_seek(t - 5_000_000, 30.0)); // long after the seek: trust the player again
        lock().seek = None;
    }

    #[test]
    fn app_names() {
        assert_eq!(pretty_app("Spotify.exe"), "Spotify");
        assert_eq!(pretty_app("MSEdge"), "Microsoft Edge");
        assert_eq!(pretty_app("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "Media Player");
        assert_eq!(pretty_app("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(pretty_app("Chrome"), "Google Chrome");
    }

    #[test]
    fn music_beats_video() {
        assert!(is_music_app("spotify.exe"));
        assert!(is_music_app("spotifyab.spotifymusic_zpdnekdrzrea0!spotify"));
        assert!(is_music_app("chrome._crx_cinhimbnkkaeohfgghhklpknlkffjgod"));
        assert!(is_music_app("com.github.th-ch.youtube-music"));
        assert!(is_music_app("metrolist"));
        assert!(!is_music_app("msedge"));
        assert!(is_browser("msedge"));
        assert!(is_browser("chrome"));
        assert!(!is_browser("chrome._crx_cinhimbnkkaeohfgghhklpknlkffjgod"));
        assert!(!is_browser("spotify.exe"));
    }

    /// The players Windows knows about right now, with the score each gets (app and state only).
    #[test]
    #[ignore]
    fn session_scores() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let m = Manager::RequestAsync().and_then(|op| op.get()).unwrap();
        for s in m.GetSessions().unwrap() {
            println!("{:<30} score {}", pretty_app(&s.SourceAppUserModelId().unwrap().to_string_lossy()), score(&s));
        }
        start();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        println!("following: {:?}", current().map(|n| n.app));
        let (list, pinned) = sources();
        for s in &list {
            println!("source: {:<20} playing={} shown={}", s.app, s.playing, s.shown);
        }
        // Pick a player that isn't shown, check it is followed, then go back to automatic.
        if let Some(other) = list.iter().find(|s| !s.shown).cloned() {
            choose(Some(other.clone()));
            std::thread::sleep(std::time::Duration::from_millis(1500));
            println!("picked {} -> following {:?} (pinned before: {pinned})", other.app, current().map(|n| n.app));
            choose(None);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            println!("auto -> following {:?}", current().map(|n| n.app));
        }
    }

    #[test]
    #[ignore]
    fn media_report() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        start();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        match current() {
            Some(n) => println!(
                "app={:?} title={:?} artist={:?} album={:?} playing={} pos={:.1}/{:.1} art={} bytes id={:x}",
                n.app,
                n.title,
                n.artist,
                n.album,
                n.playing,
                n.position_now(),
                n.duration,
                n.art.as_ref().map(|a| a.len()).unwrap_or(0),
                n.art_id
            ),
            None => println!("nothing playing"),
        }
    }
}
