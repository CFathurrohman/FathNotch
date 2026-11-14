//! Minimal blocking HTTP over WinRT's HttpClient (TLS and proxies come from Windows; no extra crates).
//! Call from a background thread (see `bg::spawn`).

use windows::core::{Error, Result, HSTRING};
use windows::Foundation::Uri;
use windows::Storage::Streams::UnicodeEncoding;
use windows::Web::Http::{HttpClient, HttpMethod, HttpRequestMessage, HttpStringContent};

pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends a request. `body` is sent as UTF-8 JSON when given.
pub fn send(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Result<Response> {
    let client = HttpClient::new()?;
    let method = match method {
        "POST" => HttpMethod::Post()?,
        "DELETE" => HttpMethod::Delete()?,
        "PUT" => HttpMethod::Put()?,
        _ => HttpMethod::Get()?,
    };
    let request = HttpRequestMessage::Create(&method, &Uri::CreateUri(&HSTRING::from(url))?)?;
    let h = request.Headers()?;
    h.TryAppendWithoutValidation(&HSTRING::from("User-Agent"), &HSTRING::from("FathNotch/0.2"))?;
    // WinRT's HttpClient can't decode "deflate" (some APIs default to it); ask for gzip, which it can.
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Accept-Encoding")) {
        h.TryAppendWithoutValidation(&HSTRING::from("Accept-Encoding"), &HSTRING::from("gzip"))?;
    }
    for (k, v) in headers {
        h.TryAppendWithoutValidation(&HSTRING::from(*k), &HSTRING::from(*v))?;
    }
    if let Some(b) = body {
        let content = HttpStringContent::CreateFromStringWithEncodingAndMediaType(
            &HSTRING::from(b),
            UnicodeEncoding::Utf8,
            &HSTRING::from("application/json"),
        )?;
        request.SetContent(&content)?;
    }
    let response = client.SendRequestAsync(&request)?.get()?;
    let status = response.StatusCode()?.0 as u16;
    let body = response.Content()?.ReadAsStringAsync()?.get()?.to_string_lossy();
    Ok(Response { status, body })
}

pub fn get(url: &str) -> Result<Response> {
    send("GET", url, &[], None)
}

/// GET that must succeed (2xx) and returns the body.
pub fn get_text(url: &str) -> Result<String> {
    let r = get(url)?;
    if r.ok() {
        Ok(r.body)
    } else {
        Err(Error::new(windows::Win32::Foundation::E_FAIL, format!("HTTP {}", r.status)))
    }
}

/// Percent-encodes a query value.
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
