//! Time-synced lyrics from LRCLIB (https://lrclib.net), a free public lyrics API. Results, including
//! "no lyrics", are cached in memory for the session; network failures aren't, so they retry later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::http;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Seconds into the track.
    pub at: f64,
    pub text: String,
}

type Lyrics = Option<Arc<Vec<Line>>>;

fn cache() -> &'static Mutex<HashMap<(String, String), Lyrics>> {
    static C: OnceLock<Mutex<HashMap<(String, String), Lyrics>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Synced lyrics for a track, or None when LRCLIB has none (or it can't be reached).
pub fn fetch(artist: &str, title: &str, album: &str, duration: f64) -> Option<Arc<Vec<Line>>> {
    let (artist, title) = (artist.trim(), title.trim());
    if artist.is_empty() || title.is_empty() {
        return None;
    }
    let key = (artist.to_lowercase(), title.to_lowercase());
    if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return hit.clone();
    }
    let result = lookup(artist, title, album.trim(), duration).ok()?; // offline: don't cache
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key, result.clone());
    result
}

/// Err only when LRCLIB couldn't be reached; Ok(None) when it answered without synced lyrics.
fn lookup(artist: &str, title: &str, album: &str, duration: f64) -> Result<Lyrics, ()> {
    let mut url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        http::escape(artist),
        http::escape(title)
    );
    if !album.is_empty() {
        url += &format!("&album_name={}", http::escape(album));
    }
    if duration > 0.0 {
        url += &format!("&duration={}", duration.round() as i64);
    }
    let exact = get(&url);
    if let Some(r) = &exact {
        if r.ok() {
            if let Some(lines) = serde_json::from_str::<Value>(&r.body).ok().as_ref().and_then(synced) {
                return Ok(Some(Arc::new(lines)));
            }
        }
    }

    // No exact match (album or duration differ, or only plain lyrics there): search instead.
    let url = format!(
        "https://lrclib.net/api/search?artist_name={}&track_name={}",
        http::escape(artist),
        http::escape(title)
    );
    let Some(r) = get(&url) else { return Err(()) };
    if !r.ok() {
        // A server error on both requests: try again another time rather than remembering "none".
        let flaky = r.status >= 500 && exact.map_or(true, |e| e.status >= 500);
        return if flaky { Err(()) } else { Ok(None) };
    }
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&r.body) else { return Ok(None) };
    let mut best: Option<(f64, Vec<Line>)> = None;
    for item in &items {
        let Some(lines) = synced(item) else { continue };
        // Prefer the closest length; unknown lengths rank behind any close match.
        let off = match (item.get("duration").and_then(Value::as_f64), duration > 0.0) {
            (Some(d), true) => (d - duration).abs(),
            _ => 1000.0,
        };
        if duration > 0.0 && off > 10.0 && off < 1000.0 {
            continue; // a different version of the song
        }
        if best.as_ref().map_or(true, |(b, _)| off < *b) {
            best = Some((off, lines));
        }
    }
    Ok(best.map(|(_, l)| Arc::new(l)))
}

/// GET with one retry on a transport failure (DNS hiccups, dropped connections); None when both fail.
fn get(url: &str) -> Option<http::Response> {
    http::get(url).or_else(|_| {
        std::thread::sleep(std::time::Duration::from_millis(400));
        http::get(url)
    })
    .ok()
}

fn synced(item: &Value) -> Option<Vec<Line>> {
    if item.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    parse(item.get("syncedLyrics")?.as_str()?)
}

/// Parses LRC: `[mm:ss.xx]text`, several stamps per line allowed; metadata tags are skipped.
fn parse(lrc: &str) -> Option<Vec<Line>> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { break };
            match stamp(&inner[..end]) {
                Some(t) => stamps.push(t),
                None => break,
            }
            rest = inner[end + 1..].trim_start();
        }
        let text = rest.trim();
        for at in stamps {
            lines.push(Line { at, text: text.to_string() });
        }
    }
    lines.sort_by(|a, b| a.at.total_cmp(&b.at));
    (!lines.is_empty()).then_some(lines)
}

/// "mm:ss.xx" (or "mm:ss" / "mm:ss:xx") → seconds.
fn stamp(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: u32 = m.trim().parse().ok()?;
    let rest = rest.trim();
    let secs: f64 = match rest.split_once(':') {
        Some((sec, frac)) => format!("{sec}.{frac}").parse().ok()?,
        None => rest.parse().ok()?,
    };
    (secs >= 0.0 && secs < 60.0 + 1e-9).then_some(m as f64 * 60.0 + secs)
}

/// The line being sung at `position` seconds and how far through it we are (0..1), or None before
/// the first line.
pub fn line_at(lines: &[Line], position: f64) -> Option<(usize, f32)> {
    const LEAD: f64 = 0.15; // show a line a touch early, like the player apps do
    let p = position + LEAD;
    let index = lines.partition_point(|l| l.at <= p).checked_sub(1)?;
    let start = lines[index].at;
    let end = lines.get(index + 1).map(|n| n.at).unwrap_or(start + 5.0);
    let len = (end - start).max(0.01);
    Some((index, ((p - start) / len).clamp(0.0, 1.0) as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let l = parse("[ar:Someone]\n[00:01.50]Hello\n[00:03.00][00:10.25]Again\n[01:02.5]\n").unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(l[0], Line { at: 1.5, text: "Hello".into() });
        assert_eq!(l[1].at, 3.0);
        assert_eq!(l[2], Line { at: 10.25, text: "Again".into() });
        assert_eq!(l[3].at, 62.5);
        assert_eq!(line_at(&l, 0.5), None);
        let (i, f) = line_at(&l, 2.1).unwrap();
        assert_eq!(i, 0);
        assert!((f - 0.5).abs() < 0.01);
        assert_eq!(line_at(&l, 100.0).unwrap(), (3, 1.0));
    }

    #[test]
    #[ignore]
    fn lyrics_fetch() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let t = std::time::Instant::now();
        let l = fetch("Queen", "Bohemian Rhapsody", "A Night at the Opera", 354.0);
        println!("{} lines in {:?}", l.as_ref().map_or(0, |l| l.len()), t.elapsed());
        if let Some(l) = &l {
            for line in l.iter().take(5) {
                println!("  {:7.2} {}", line.at, line.text);
            }
            println!("at 60s: {:?}", line_at(l, 60.0).map(|(i, f)| (l[i].text.clone(), f)));
        }
        let t = std::time::Instant::now();
        let again = fetch("queen", "bohemian rhapsody", "", 0.0);
        println!("cached: {} in {:?}", again.is_some(), t.elapsed());
        let miss = fetch("Nobody Real Xyzzy", "No Such Song Qwerty", "", 100.0);
        println!("miss: {:?}", miss.is_none());
    }
}
