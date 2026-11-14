//! The Shelf: a holding area for files dropped on the notch, kept in shelf.json in the .NET build's schema
//! ([{ Path, AddedAt, Owned }], newest first). "Owned" files are copies FathNotch made itself (dropped text,
//! images from a browser) and live in the data folder's `shelf` directory; they're deleted with their item.
//! Items older than the retention setting (days, 0 = forever) or whose file is gone drop out on load.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::store;
use super::todos::{format_local, local_from_utc, local_now_secs, parse_local};

const FILE: &str = "shelf.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub added_at: String,
    #[serde(default)]
    pub owned: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Item {
    pub fn name(&self) -> String {
        let p = self.path.trim_end_matches(['\\', '/']);
        match Path::new(p).file_name() {
            Some(n) if !n.is_empty() => n.to_string_lossy().into_owned(),
            _ => self.path.clone(),
        }
    }

    pub fn is_folder(&self) -> bool {
        Path::new(&self.path).is_dir()
    }

    pub fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }

    pub fn is_image(&self) -> bool {
        is_image_path(&self.path)
    }
}

pub fn is_image_path(p: &str) -> bool {
    let ext = Path::new(p).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff")
}

pub(crate) fn now_string() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format_local(local_from_utc(d.as_secs() as i64), d.subsec_nanos() / 100)
}

/// The folder for files the Shelf owns (created on demand).
pub fn dir() -> PathBuf {
    let p = store::path("shelf");
    let _ = std::fs::create_dir_all(&p);
    p
}

fn cell() -> &'static Mutex<Vec<Item>> {
    static CELL: OnceLock<Mutex<Vec<Item>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(load()))
}

fn load() -> Vec<Item> {
    let mut items: Vec<Item> = store::load(FILE);
    let days = super::settings::get().shelf_retention_days as i64;
    let now = local_now_secs();
    let before = items.len();
    items.retain(|item| {
        let expired = days > 0 && parse_local(&item.added_at).is_some_and(|(t, _)| now - t > days * 86_400);
        let keep = !expired && item.exists();
        if !keep && item.owned {
            let _ = std::fs::remove_file(&item.path);
        }
        keep
    });
    if items.len() != before {
        store::save(FILE, &items);
    }
    items
}

fn change<R>(f: impl FnOnce(&mut Vec<Item>) -> R) -> R {
    let r = {
        let mut items = cell().lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut items);
        store::save(FILE, &*items);
        r
    };
    super::phone::push("shelf"); // the phone page lists shelf files (also asks for a redraw)
    r
}

/// Everything on the shelf, newest first.
pub fn all() -> Vec<Item> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Adds files or folders (already shelved paths are skipped). Any thread.
pub fn add(paths: &[String]) {
    add_files(paths, false);
}

/// `owned`: the files are copies made for the Shelf and are deleted with their item.
pub fn add_files(paths: &[String], owned: bool) {
    change(|items| {
        for p in paths {
            if p.is_empty() || items.iter().any(|i| i.path.eq_ignore_ascii_case(p)) {
                continue;
            }
            items.insert(0, Item { path: p.clone(), added_at: now_string(), owned, extra: Map::new() });
        }
    });
}

/// Saves dropped text as "Text yyyy-MM-dd HHmmss.txt" in the shelf folder and shelves it.
pub fn add_text(text: &str) -> Option<String> {
    let t = local_now_secs();
    let (y, m, d) = super::todos::civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    let name = format!("Text {y:04}-{m:02}-{d:02} {:02}{:02}{:02}.txt", s / 3600, s % 3600 / 60, s % 60);
    let path = dir().join(name);
    std::fs::write(&path, text).ok()?;
    let p = path.to_string_lossy().into_owned();
    add_files(&[p.clone()], true);
    Some(p)
}

pub fn remove(path: &str) {
    change(|items| {
        if let Some(i) = items.iter().position(|x| x.path == path) {
            let item = items.remove(i);
            if item.owned {
                let _ = std::fs::remove_file(&item.path);
            }
        }
    });
}

pub fn clear() {
    change(|items| {
        for i in items.iter().filter(|i| i.owned) {
            let _ = std::fs::remove_file(&i.path);
        }
        items.clear();
    });
}

/// Drops items whose file disappeared (moved away by a drag, deleted in Explorer).
pub fn prune_missing() {
    let missing = cell().lock().unwrap_or_else(|e| e.into_inner()).iter().any(|i| !i.exists());
    if missing {
        change(|items| items.retain(|i| i.exists()));
    }
}

// ---------------------------------------------------------------- thumbnails

/// A small PNG for `path`: the Explorer thumbnail for images and documents, else the file type's icon.
/// Blocking (COM + disk): call from a background thread.
pub fn thumbnail(path: &str, size: i32) -> Option<Vec<u8>> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK};

    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_BIGGERSIZEOK).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(hbmp, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            let _ = DeleteObject(hbmp);
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(hbmp);
        if lines == 0 {
            return None;
        }
        // Thumbnails of photos often come with alpha = 0 everywhere: make them opaque.
        if px.chunks_exact(4).all(|p| p[3] == 0) {
            for p in px.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        encode_png(w as u32, h as u32, &px)
    }
}

/// Encodes premultiplied BGRA pixels as PNG bytes.
pub fn encode_png(w: u32, h: u32, bgra: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
        WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, STATFLAG_NONAME, STREAM_SEEK_SET};
    use windows::Win32::UI::Shell::SHCreateMemStream;
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let bmp = wic.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, bgra).ok()?;
        let mem = SHCreateMemStream(None)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).ok()?;
        enc.Initialize(&mem, WICBitmapEncoderNoCache).ok()?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut()).ok()?;
        let frame = frame?;
        frame.Initialize(None).ok()?;
        frame.SetSize(w, h).ok()?;
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt).ok()?;
        frame.WriteSource(&bmp, std::ptr::null()).ok()?;
        frame.Commit().ok()?;
        enc.Commit().ok()?;
        let mut stat = Default::default();
        mem.Stat(&mut stat, STATFLAG_NONAME).ok()?;
        let len = stat.cbSize as usize;
        mem.Seek(0, STREAM_SEEK_SET, None).ok()?;
        let mut out = vec![0u8; len];
        let mut read = 0u32;
        mem.Read(out.as_mut_ptr() as *mut _, len as u32, Some(&mut read)).ok().ok()?;
        out.truncate(read as usize);
        Some(out)
    }
}
