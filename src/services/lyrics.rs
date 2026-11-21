//! Lyrics, line-timed where possible: LRCLIB (https://lrclib.net), then KuGou; else untimed ones (LRCLIB,
//! YouTube Music) spread over the song. Results, including "no lyrics", are cached in memory for the
//! session; network failures aren't, so they retry later.

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

/// A search result counts as the song playing only within this many seconds of its length: another
/// version (a longer intro, a radio edit) has its lines at other times.
const MATCH_SECS: f64 = 4.0;

fn cache() -> &'static Mutex<HashMap<(String, String), Lyrics>> {
    static C: OnceLock<Mutex<HashMap<(String, String), Lyrics>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lyrics for a track, or None when no source has any (or none can be reached).
pub fn fetch(artist: &str, title: &str, album: &str, duration: f64) -> Option<Arc<Vec<Line>>> {
    let (artist, title) = (artist.trim(), title.trim());
    // Browsers often leave the artist empty and put "Artist - Song" in the title; that's still worth a try.
    let dashed = [" - ", " – ", " — ", " | "].iter().any(|s| title.contains(s));
    if title.is_empty() || (artist.is_empty() && !dashed) {
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

/// Every source, best first: line-timed lyrics from LRCLIB and KuGou for each way of reading the
/// track's name (players, browsers and YouTube put "Official Video", "Artist - Topic" or "Artist - Song" in
/// it), then untimed lyrics spread over the song as a last resort. Err only when no source could be reached.
fn lookup(artist: &str, title: &str, album: &str, duration: f64) -> Result<Lyrics, ()> {
    let names = candidates(artist, title);
    if names.is_empty() {
        return Ok(None);
    }
    let mut reached = false;
    for (i, (a, t)) in names.iter().enumerate() {
        let alb = if i == 0 { album } else { "" };
        match lrclib(a, t, alb, duration) {
            Ok(Some(l)) => return Ok(Some(l)),
            Ok(None) => reached = true,
            Err(()) => {}
        }
    }
    // Free-text search on LRCLIB catches artist/title mix-ups ("Song - Artist").
    if let Some(r) = get(&format!("https://lrclib.net/api/search?q={}", http::escape(&format!("{} {}", names[0].0, names[0].1)))) {
        reached |= r.ok();
        if let Some(l) = best_lrclib(&r.body, &names, duration, synced) {
            return Ok(Some(Arc::new(l)));
        }
    }
    for (a, t) in names.iter().take(2) {
        match ytmusic(a, t, duration, true) {
            Ok(Some(l)) => return Ok(Some(Arc::new(l))),
            Ok(None) => reached = true,
            Err(()) => {}
        }
    }
    for (a, t) in &names {
        match kugou(a, t, duration) {
            Ok(Some(l)) => return Ok(Some(Arc::new(l))),
            Ok(None) => reached = true,
            Err(()) => {}
        }
    }
    // No timed lyrics anywhere: untimed ones from LRCLIB, each line given a share of the song.
    for (a, t) in &names {
        let url = format!("https://lrclib.net/api/search?artist_name={}&track_name={}", http::escape(a), http::escape(t));
        if let Some(r) = get(&url) {
            reached |= r.ok();
            if let Some(l) = best_lrclib(&r.body, &names, duration, |item| plain(item, duration)) {
                return Ok(Some(Arc::new(l)));
            }
        }
    }
    // Then YouTube Music's untimed text.
    if duration >= 20.0 {
        for (a, t) in names.iter().take(2) {
            match ytmusic(a, t, duration, false) {
                Ok(Some(l)) => return Ok(Some(Arc::new(l))),
                Ok(None) => reached = true,
                Err(()) => {}
            }
        }
    }
    if reached {
        Ok(None)
    } else {
        Err(())
    }
}

/// The ways to read a track's artist and title, the given ones first, then cleaned up.
fn candidates(artist: &str, title: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if !artist.trim().is_empty() {
        out.push((artist.trim().to_string(), title.trim().to_string()));
    }
    let mut push = |a: String, t: String| {
        let (a, t) = (a.trim().to_string(), t.trim().to_string());
        if !a.is_empty() && !t.is_empty() && !out.iter().any(|(x, y)| x.eq_ignore_ascii_case(&a) && y.eq_ignore_ascii_case(&t)) {
            out.push((a, t));
        }
    };
    let (ca, ct) = (clean_artist(artist), clean_title(title));
    push(ca.clone(), ct.clone());
    // "Artist - Song" in the title (YouTube, browsers): the part before the dash is the artist.
    for sep in [" - ", " – ", " — ", " | "] {
        if let Some((left, right)) = ct.split_once(sep) {
            push(clean_artist(left), clean_title(right));
            push(clean_artist(right), clean_title(left));
            break;
        }
    }
    out
}

/// Drops "(Official Video)", "[MV]", "(feat. X)", "| Lyrics" and the like from a title.
fn clean_title(t: &str) -> String {
    const NOISE: [&str; 22] = [
        "official", "video", "audio", "lyric", "lirik", "mv", "m/v", "visualizer", "visualiser", "remaster", "hd", "4k",
        "explicit", "clip", "music", "feat", "ft.", "prod", "version", "performance", "color coded", "eng sub",
    ];
    let mut s = t.to_string();
    // Bracketed bits with any of the words above.
    for (open, close) in [('(', ')'), ('[', ']'), ('【', '】'), ('「', '」')] {
        loop {
            let Some(a) = s.find(open) else { break };
            let Some(b) = s[a..].find(close).map(|b| a + b) else { break };
            let inner = s[a + open.len_utf8()..b].to_lowercase();
            if NOISE.iter().any(|w| inner.contains(w)) {
                s.replace_range(a..b + close.len_utf8(), " ");
            } else {
                break;
            }
        }
    }
    let lower = s.to_lowercase();
    for cut in [" feat. ", " feat ", " ft. ", " ft ", " | official", " | lyric", " | lirik"] {
        if let Some(i) = lower.find(cut) {
            s.truncate(i);
            break;
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(|c: char| c == '-' || c == '|' || c.is_whitespace()).to_string()
}

/// "Artist - Topic", "ArtistVEVO", "Artist, Other & More" → "Artist".
fn clean_artist(a: &str) -> String {
    let mut s = a.trim().to_string();
    for suffix in [" - Topic", "VEVO", " Official", " official"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.trim().to_string();
        }
    }
    for sep in [", ", " & ", " x ", " X ", " feat. ", " feat ", " ft. ", "; ", " / "] {
        if let Some((first, _)) = s.split_once(sep) {
            s = first.to_string();
        }
    }
    s.trim().to_string()
}

/// Lowercase letters and digits only, for loose comparisons.
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// True when a result's title matches one of the names we looked for (one containing the other).
fn same_song(found: &str, names: &[(String, String)]) -> bool {
    let f = norm(&clean_title(found));
    !f.is_empty()
        && names.iter().any(|(_, t)| {
            let t = norm(t);
            !t.is_empty() && (f.contains(&t) || t.contains(&f))
        })
}

/// The best LRCLIB search result that `pick` turns into lines: right song, closest length.
fn best_lrclib(body: &str, names: &[(String, String)], duration: f64, pick: impl Fn(&Value) -> Option<Vec<Line>>) -> Option<Vec<Line>> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(body) else { return None };
    let mut best: Option<(f64, Vec<Line>)> = None;
    for item in &items {
        if !item.get("trackName").and_then(Value::as_str).is_some_and(|n| same_song(n, names)) {
            continue;
        }
        let off = match (item.get("duration").and_then(Value::as_f64), duration > 0.0) {
            (Some(d), true) => (d - duration).abs(),
            _ => 1000.0,
        };
        if duration > 0.0 && off > MATCH_SECS && off < 1000.0 {
            continue;
        }
        if best.as_ref().is_some_and(|(b, _)| off >= *b) {
            continue;
        }
        if let Some(lines) = pick(item) {
            best = Some((off, lines));
        }
    }
    best.map(|(_, l)| l)
}

/// Untimed lyrics, each line given a share of the song by its length (the first few seconds and the end
/// left out), so the notch still moves through them roughly with the music.
fn plain(item: &Value, duration: f64) -> Option<Vec<Line>> {
    if item.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = item.get("plainLyrics")?.as_str()?;
    spread(text, if duration > 0.0 { duration } else { item.get("duration").and_then(Value::as_f64).unwrap_or(0.0) })
}

fn spread(text: &str, duration: f64) -> Option<Vec<Line>> {
    let rows: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if rows.is_empty() || duration < 20.0 {
        return None;
    }
    let (start, end) = (duration * 0.08, duration * 0.94);
    let weight = |l: &str| 8.0 + l.chars().count() as f64;
    let total: f64 = rows.iter().map(|l| weight(l)).sum();
    let mut at = start;
    let mut out = Vec::with_capacity(rows.len());
    for l in rows {
        out.push(Line { at, text: l.to_string() });
        at += (end - start) * weight(l) / total;
    }
    Some(out)
}

/// YouTube Music: search the song, open its lyrics tab, and read the lines with their times (what the
/// Android app shows). With `timed_only` false, falls back to the untimed text spread over the song.
fn ytmusic(artist: &str, title: &str, duration: f64, timed_only: bool) -> Result<Option<Vec<Line>>, ()> {
    const WEB: &str = r#"{"client":{"clientName":"WEB_REMIX","clientVersion":"1.20240101.01.00","hl":"en"}}"#;
    const ANDROID: &str = r#"{"client":{"clientName":"ANDROID_MUSIC","clientVersion":"7.27.52","androidSdkVersion":34,"hl":"en"}}"#;
    const H: [(&str, &str); 2] = [("Origin", "https://music.youtube.com"), ("Referer", "https://music.youtube.com/")];
    const HA: [(&str, &str); 1] = [("User-Agent", "com.google.android.apps.youtube.music/7.27.52 (Linux; U; Android 14) gzip")];
    let post = |what: &str, headers: &[(&str, &str)], body: String| -> Result<String, ()> {
        let r = http::send("POST", &format!("https://music.youtube.com/youtubei/v1/{what}?prettyPrint=false"), headers, Some(&body)).map_err(|_| ())?;
        if r.ok() {
            Ok(r.body)
        } else {
            Err(())
        }
    };
    let q = serde_json::to_string(&format!("{artist} {title}")).map_err(|_| ())?;
    // "Songs" filter, so the first hit is a track rather than a video or a playlist.
    let found = post("search", &H, format!(r#"{{"context":{WEB},"query":{q},"params":"EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D"}}"#))?;
    let Some(vid) = found.split("\"videoId\":\"").nth(1).and_then(|s| s.get(..11)).map(str::to_string) else { return Ok(None) };
    let next = post("next", &H, format!(r#"{{"context":{WEB},"videoId":"{vid}"}}"#))?;
    // The track we got must be the one we asked for, and the same length (another version's times differ).
    let after_title = next.split("\"title\":{\"runs\":[{\"text\":\"").nth(1).unwrap_or("");
    let got = after_title.split('"').next().unwrap_or("");
    if !same_song(got, &[(artist.to_string(), title.to_string())]) {
        return Ok(None);
    }
    let length = after_title
        .split("\"lengthText\":{\"runs\":[{\"text\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.split(':').try_fold(0.0, |acc, p| p.parse::<f64>().ok().map(|n| acc * 60.0 + n)));
    if let (Some(len), true) = (length, duration > 0.0) {
        if (len - duration).abs() > MATCH_SECS {
            return Ok(None);
        }
    }
    let Some(lyrics_id) = next.split("\"browseId\":\"MPLY").nth(1).and_then(|s| s.split('"').next()).map(|s| format!("MPLY{s}")) else {
        return Ok(None);
    };
    let page = post("browse", &HA, format!(r#"{{"context":{ANDROID},"browseId":"{lyrics_id}"}}"#))?;
    let v: Value = serde_json::from_str(&page).map_err(|_| ())?;
    if let Some(Value::Array(cues)) = find_key(&v, "timedLyricsData") {
        let lines: Vec<Line> = cues
            .iter()
            .filter_map(|c| {
                let at = c.pointer("/cueRange/startTimeMilliseconds")?.as_str()?.parse::<f64>().ok()? / 1000.0;
                let text = c.get("lyricLine")?.as_str()?.trim();
                // "♪" marks an instrumental part: show nothing there (the notch shows the title).
                Some(Line { at, text: if text == "♪" { String::new() } else { text.to_string() } })
            })
            .collect();
        if lines.iter().any(|l| !l.text.is_empty()) {
            return Ok(Some(lines));
        }
    }
    if timed_only {
        return Ok(None);
    }
    let page = post("browse", &H, format!(r#"{{"context":{WEB},"browseId":"{lyrics_id}"}}"#))?;
    let v: Value = serde_json::from_str(&page).map_err(|_| ())?;
    let text = find_key(&v, "musicDescriptionShelfRenderer").and_then(|s| s.pointer("/description/runs/0/text")).and_then(Value::as_str);
    Ok(text.and_then(|t| spread(t, duration)))
}

/// KuGou: another large catalogue (many Asian and Indonesian songs), line-timed LRC.
fn kugou(artist: &str, title: &str, duration: f64) -> Result<Option<Vec<Line>>, ()> {
    let q = format!("{artist} {title}");
    let url = format!(
        "https://mobileservice.kugou.com/api/v3/search/song?format=json&keyword={}&page=1&pagesize=10&showtype=1",
        http::escape(&q)
    );
    let r = http::get(&url).map_err(|_| ())?;
    if !r.ok() {
        return Err(());
    }
    let v: Value = serde_json::from_str(&r.body).map_err(|_| ())?;
    let names = [(artist.to_string(), title.to_string())];
    let songs = v.pointer("/data/info").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut best: Option<(f64, String, f64)> = None;
    for song in &songs {
        let name = song.get("songname").and_then(Value::as_str).unwrap_or("");
        let singer = norm(song.get("singername").and_then(Value::as_str).unwrap_or(""));
        let want = norm(artist);
        if !same_song(name, &names) || singer.is_empty() || want.is_empty() || !(singer.contains(&want) || want.contains(&singer)) {
            continue;
        }
        let secs = song.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
        let off = if duration > 0.0 && secs > 0.0 { (secs - duration).abs() } else { 1000.0 };
        if duration > 0.0 && off > MATCH_SECS && off < 1000.0 {
            continue;
        }
        if let Some(hash) = song.get("hash").and_then(Value::as_str) {
            if best.as_ref().map_or(true, |(b, _, _)| off < *b) {
                best = Some((off, hash.to_string(), secs));
            }
        }
    }
    let Some((_, hash, secs)) = best else { return Ok(None) };
    let ms = ((if duration > 0.0 { duration } else { secs }) * 1000.0) as i64;
    let url = format!("https://krcs.kugou.com/search?ver=1&man=yes&client=mobi&keyword=&duration={ms}&hash={hash}");
    let v: Value = serde_json::from_str(&http::get(&url).map_err(|_| ())?.body).map_err(|_| ())?;
    let Some(c) = v.get("candidates").and_then(Value::as_array).and_then(|c| c.first().cloned()) else { return Ok(None) };
    let id = c.get("id").map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).unwrap_or_default();
    let key = c.get("accesskey").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || key.is_empty() {
        return Ok(None);
    }
    let url = format!("https://lyrics.kugou.com/download?ver=1&client=pc&id={id}&accesskey={key}&fmt=lrc&charset=utf8");
    let v: Value = serde_json::from_str(&http::get(&url).map_err(|_| ())?.body).map_err(|_| ())?;
    let Some(bytes) = v.get("content").and_then(Value::as_str).and_then(base64) else { return Ok(None) };
    let lrc = String::from_utf8_lossy(&bytes);
    Ok(parse(lrc.trim_start_matches('\u{feff}')).map(drop_credits).filter(|l| !l.is_empty()))
}

/// Drops the credit lines some catalogues put first ("作词 : …", "Composer: …", "KuGou …").
fn drop_credits(lines: Vec<Line>) -> Vec<Line> {
    const CREDIT: [&str; 14] = ["作词", "作曲", "编曲", "制作", "词：", "曲：", "演唱", "lyricist", "composer", "arranger", "producer", "written by", "kugou", "酷狗"];
    lines
        .into_iter()
        .filter(|l| {
            let t = l.text.to_lowercase();
            !CREDIT.iter().any(|c| t.contains(c))
        })
        .collect()
}

/// Standard base64 (padding optional).
fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// LRCLIB, exact match then search. Err only when LRCLIB couldn't be reached; Ok(None) when it answered
/// without synced lyrics.
fn lrclib(artist: &str, title: &str, album: &str, duration: f64) -> Result<Lyrics, ()> {
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
        if duration > 0.0 && off > MATCH_SECS && off < 1000.0 {
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
    // [offset:+500]: shows every line 0.5 s sooner (negative: later).
    let mut offset = 0.0;
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        if let Some(v) = rest.strip_prefix("[offset:").and_then(|r| r.strip_suffix(']')) {
            offset = v.trim().trim_start_matches('+').parse::<f64>().unwrap_or(0.0) / 1000.0;
            continue;
        }
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
    if offset != 0.0 {
        for l in &mut lines {
            l.at = (l.at - offset).max(0.0);
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
        let o = parse("[offset:+500]
[00:02.00]Hi
").unwrap();
        assert_eq!(o[0].at, 1.5);
    }

    #[test]
    fn cleans_names() {
        assert_eq!(clean_title("Hati-Hati di Jalan (Official Music Video)"), "Hati-Hati di Jalan");
        assert_eq!(clean_title("Song [MV] | Lyrics"), "Song");
        assert_eq!(clean_title("Love Story (Taylor's Version)"), "Love Story");
        assert_eq!(clean_title("Halu (feat. Someone)"), "Halu");
        assert_eq!(clean_artist("Tulus - Topic"), "Tulus");
        assert_eq!(clean_artist("Artist, Other & More"), "Artist");
        let c = candidates("", "Tulus - Hati-Hati di Jalan (Official Video)");
        assert_eq!(c[0], ("Tulus".to_string(), "Hati-Hati di Jalan".to_string()));
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        let s = spread("one\n\ntwo two\nthree", 100.0).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s[0].at > 5.0 && s[2].at < 94.0 && s[1].at > s[0].at);
    }

    /// Each source on its own, and the whole chain for names the way players report them.
    #[test]
    #[ignore]
    fn lyrics_sources() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        for (a, t, d) in [("Tulus", "Hati-Hati di Jalan", 242.0), ("Nadin Amizah", "Bertaut", 286.0), ("Hindia", "Evaluasi", 214.0)] {
            let n = ytmusic(a, t, d, true).map(|l| l.map_or(0, |l| l.len()));
            let k = kugou(a, t, d).map(|l| l.map_or(0, |l| l.len()));
            println!("{a} - {t}: ytmusic {n:?}, kugou {k:?}");
        }
        for (a, t, d) in [
            ("", "Tulus - Hati-Hati di Jalan (Official Music Video)", 0.0),
            ("Nadin Amizah - Topic", "Bertaut", 286.0),
            ("Sal Priadi", "Gala Bunga Matahari", 0.0),
        ] {
            let tm = std::time::Instant::now();
            let l = fetch(a, t, "", d);
            println!("fetch {t:?}: {} lines in {:?}; first: {:?}", l.as_ref().map_or(0, |l| l.len()), tm.elapsed(), l.as_ref().and_then(|l| l.first().map(|x| x.at)));
        }
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

/// The first value under `key` anywhere in a JSON tree.
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}
