//! Drawing: Direct2D (software) into a 32-bit premultiplied bitmap, shown with UpdateLayeredWindow.
//! Fully transparent pixels let clicks through to the windows below, so the window can stay one fixed
//! size while the notch inside it grows and shrinks. The software renderer keeps the GPU driver out of the
//! process, which is most of what keeps memory low.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{w, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui::icons;

pub type Color = D2D1_COLOR_F;

pub fn color(rgb: u32, a: f32) -> Color {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a,
    }
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    D2D1_COLOR_F { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn pt(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Font weights by name (DirectWrite values).
pub const LIGHT: u16 = 300;
pub const REGULAR: u16 = 400;
pub const MEDIUM: u16 = 500;
pub const SEMIBOLD: u16 = 600;
pub const BOLD: u16 = 700;

pub struct Gfx {
    pub factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    memdc: HDC,
    bitmap: HBITMAP,
    bits: *mut c_void,
    old: HGDIOBJ,
    /// Bitmap size in pixels.
    pub px_w: i32,
    pub px_h: i32,
    /// DIP → pixel scale (monitor DPI / 96).
    pub scale: f32,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    base: RefCell<Matrix3x2>,
    /// `base` without the stagger offset (`set_offset`).
    origin: RefCell<Matrix3x2>,
    fonts: RefCell<HashMap<(u32, u16, bool), IDWriteTextFormat>>,
    /// Inter (the app's UI font, as in the .NET build), loaded from files next to the data.
    inter: Option<IDWriteFontCollection>,
    icons: RefCell<HashMap<&'static str, (ID2D1PathGeometry, f32, f32)>>,
    /// Decoded images by key, with the frame they were last drawn in (least recently used are dropped).
    images: RefCell<HashMap<u64, (ID2D1Bitmap, u64)>>,
    /// image_palette results by image key.
    colors: RefCell<HashMap<u64, Vec<Color>>>,
    /// Frosted glass: a blurred picture of the screen behind the canvas (taken when the panel opens).
    frost: RefCell<Option<ID2D1Bitmap>>,
    tick: std::cell::Cell<u64>,
    /// Rows (px) of the canvas used by the current frame.
    region: std::cell::Cell<i32>,
    /// Columns (px): first one and count.
    region_x: std::cell::Cell<(i32, i32)>,
}

fn identity() -> Matrix3x2 {
    Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 }
}

fn mul(a: &Matrix3x2, b: &Matrix3x2) -> Matrix3x2 {
    Matrix3x2 {
        M11: a.M11 * b.M11 + a.M12 * b.M21,
        M12: a.M11 * b.M12 + a.M12 * b.M22,
        M21: a.M21 * b.M11 + a.M22 * b.M21,
        M22: a.M21 * b.M12 + a.M22 * b.M22,
        M31: a.M31 * b.M11 + a.M32 * b.M21 + b.M31,
        M32: a.M31 * b.M12 + a.M32 * b.M22 + b.M32,
    }
}

impl Gfx {
    pub fn new(dip_w: f32, dip_h: f32, scale: f32) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let inter = inter_fonts(&dwrite);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0 * scale,
                dpiY: 96.0 * scale,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(0xFFFFFF, 1.0), None)?;
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;

            let px_w = (dip_w * scale).ceil() as i32;
            let px_h = (dip_h * scale).ceil() as i32;
            let screen = GetDC(None);
            let memdc = CreateCompatibleDC(screen);
            ReleaseDC(None, screen);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: px_w,
                    biHeight: -px_h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(memdc, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(memdc, bitmap);
            rt.BindDC(memdc, &RECT { left: 0, top: 0, right: px_w, bottom: px_h })?;
            Ok(Self {
                factory,
                dwrite,
                rt,
                memdc,
                bitmap,
                bits,
                old,
                px_w,
                px_h,
                scale,
                brush,
                round,
                base: RefCell::new(identity()),
                origin: RefCell::new(identity()),
                fonts: RefCell::new(HashMap::new()),
                inter,
                icons: RefCell::new(HashMap::new()),
                images: RefCell::new(HashMap::new()),
                colors: RefCell::new(HashMap::new()),
                frost: RefCell::new(None),
                tick: std::cell::Cell::new(0),
                region: std::cell::Cell::new(px_h),
                region_x: std::cell::Cell::new((0, px_w)),
            })
        }
    }

    pub fn begin(&self) {
        self.begin_partial(0.0, f32::MAX, f32::MAX);
    }

    /// Starts a frame that only uses the box `x..x+w` × `0..h` (DIP) of the canvas: only that box is cleared
    /// and put on screen (the closed notch is a small fraction of the canvas, so its animations cost far less).
    pub fn begin_partial(&self, x: f32, w: f32, h: f32) {
        let py = if h >= f32::MAX { self.px_h } else { ((h * self.scale).ceil() as i32).clamp(1, self.px_h) };
        let (px0, pw) = if w >= f32::MAX {
            (0, self.px_w)
        } else {
            let x0 = ((x * self.scale).floor() as i32).clamp(0, self.px_w - 1);
            (x0, (((x + w) * self.scale).ceil() as i32).clamp(x0 + 1, self.px_w) - x0)
        };
        self.region.set(py);
        self.region_x.set((px0, pw));
        unsafe {
            self.rt.BeginDraw();
            self.set_base(1.0, 0.0, 0.0);
            let r = rect(px0 as f32 / self.scale, 0.0, pw as f32 / self.scale, py as f32 / self.scale);
            self.rt.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt.Clear(Some(&color(0, 0.0)));
            self.rt.PopAxisAlignedClip();
        }
    }

    /// First canvas column (px) of the window this frame (the window starts there on screen).
    pub fn region_left(&self) -> i32 {
        self.region_x.get().0
    }

    /// Finishes the frame and puts it on screen at `(x, y)` (pixels).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) -> Result<()> {
        unsafe {
            self.rt.EndDraw(None, None)?;
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT { x: x + self.region_x.get().0, y }),
                Some(&SIZE { cx: self.region_x.get().1, cy: self.region.get() }),
                self.memdc,
                Some(&POINT { x: self.region_x.get().0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }

    /// Finishes the frame and writes it to a PNG instead of the screen (screenshot mode).
    pub fn save_png(&self, path: &str) -> Result<()> {
        unsafe {
            self.rt.EndDraw(None, None)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stride = (self.px_w * 4) as u32;
            let size = stride * self.px_h as u32;
            let data = std::slice::from_raw_parts(self.bits as *const u8, size as usize);
            let bmp = wic.CreateBitmapFromMemory(self.px_w as u32, self.px_h as u32, &GUID_WICPixelFormat32bppPBGRA, stride, data)?;
            let stream = wic.CreateStream()?;
            stream.InitializeFromFilename(&windows::core::HSTRING::from(path), 0x40000000 /* GENERIC_WRITE */)?;
            let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            frame.Initialize(None)?;
            frame.SetSize(self.px_w as u32, self.px_h as u32)?;
            let mut fmt = GUID_WICPixelFormat32bppPBGRA;
            frame.SetPixelFormat(&mut fmt)?;
            frame.WriteSource(&bmp, std::ptr::null())?;
            frame.Commit()?;
            enc.Commit()?;
            Ok(())
        }
    }

    /// Everything drawn next is scaled by `s` around `(ox, oy)` (the open panel's "Panel size").
    pub fn set_base(&self, s: f32, ox: f32, oy: f32) {
        let m = Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox - ox * s, M32: oy - oy * s };
        *self.base.borrow_mut() = m;
        *self.origin.borrow_mut() = m;
        unsafe { self.rt.SetTransform(&m) };
    }

    /// Maps the layout rectangle `from` onto `to` for everything drawn next (a tab growing out of its icon),
    /// on top of the current transform. Returns the transform to hand back to `end_morph`.
    pub fn begin_morph(&self, from: (f32, f32, f32, f32), to: (f32, f32, f32, f32)) -> Matrix3x2 {
        let prev = *self.origin.borrow();
        let sx = if from.2 > 0.0 { to.2 / from.2 } else { 1.0 };
        let sy = if from.3 > 0.0 { to.3 / from.3 } else { 1.0 };
        let local = Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: to.0 - from.0 * sx, M32: to.1 - from.1 * sy };
        let m = mul(&local, &prev);
        *self.origin.borrow_mut() = m;
        *self.base.borrow_mut() = m;
        unsafe { self.rt.SetTransform(&m) };
        prev
    }

    pub fn end_morph(&self, prev: Matrix3x2) {
        *self.origin.borrow_mut() = prev;
        *self.base.borrow_mut() = prev;
        unsafe { self.rt.SetTransform(&prev) };
    }

    /// Moves what's drawn next by (dx, dy) layout units (cards rising into place); (0, 0) puts it back.
    pub fn set_offset(&self, dx: f32, dy: f32) {
        let mut m = *self.origin.borrow();
        m.M31 += dx * m.M11;
        m.M32 += dy * m.M22;
        *self.base.borrow_mut() = m;
        unsafe { self.rt.SetTransform(&m) };
    }

    fn set(&self, c: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&c) };
        &self.brush
    }

    pub fn fill_geometry(&self, geo: &ID2D1Geometry, c: Color) {
        unsafe { self.rt.FillGeometry(geo, self.set(c), None) };
    }

    pub fn stroke_geometry(&self, geo: &ID2D1Geometry, c: Color, width: f32) {
        unsafe { self.rt.DrawGeometry(geo, self.set(c), width, &self.round) };
    }

    pub fn fill_rect(&self, r: D2D_RECT_F, c: Color) {
        unsafe { self.rt.FillRectangle(&r, self.set(c)) };
    }

    pub fn fill_round(&self, r: D2D_RECT_F, radius: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.set(c)) };
    }

    pub fn stroke_round(&self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.set(c), width, None) };
    }

    pub fn fill_circle(&self, x: f32, y: f32, r: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.set(c)) };
    }

    pub fn fill_ellipse(&self, e: D2D1_ELLIPSE, c: Color) {
        unsafe { self.rt.FillEllipse(&e, self.set(c)) };
    }

    pub fn stroke_circle(&self, x: f32, y: f32, r: f32, c: Color, width: f32) {
        let e = D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r };
        unsafe { self.rt.DrawEllipse(&e, self.set(c), width, None) };
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, c: Color, width: f32) {
        unsafe { self.rt.DrawLine(pt(x1, y1), pt(x2, y2), self.set(c), width, &self.round) };
    }

    /// Builds a path with `f` (open figures; use `sink.BeginFigure` / `EndFigure` as needed).
    pub fn path(&self, f: impl FnOnce(&ID2D1GeometrySink)) -> Option<ID2D1PathGeometry> {
        unsafe {
            let geo = self.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            f(&sink);
            sink.Close().ok()?;
            Some(geo)
        }
    }

    /// An arc of a circle from angle `a0` to `a1` (radians, 0 = top, clockwise), stroked.
    pub fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, c: Color, width: f32) {
        if (a1 - a0).abs() < 0.001 {
            return;
        }
        if (a1 - a0).abs() >= std::f32::consts::TAU - 0.001 {
            self.stroke_circle(cx, cy, r, c, width);
            return;
        }
        let p = |a: f32| pt(cx + r * a.sin(), cy - r * a.cos());
        if let Some(geo) = self.path(|s| unsafe {
            s.BeginFigure(p(a0), D2D1_FIGURE_BEGIN_HOLLOW);
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: p(a1),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: if a1 > a0 { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                arcSize: if (a1 - a0).abs() > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            s.EndFigure(D2D1_FIGURE_END_OPEN);
        }) {
            self.stroke_geometry(&geo.into(), c, width);
        }
    }

    pub fn push_clip(&self, r: D2D_RECT_F) {
        unsafe { self.rt.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }

    // ------------------------------------------------------------------ text

    fn font(&self, size: f32, weight: u16, wrap: bool) -> IDWriteTextFormat {
        let key = ((size * 10.0) as u32, weight, wrap);
        if let Some(f) = self.fonts.borrow().get(&key) {
            return f.clone();
        }
        unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    if self.inter.is_some() { w!("Inter") } else { w!("Segoe UI Variable Text") },
                    self.inter.as_ref(),
                    DWRITE_FONT_WEIGHT(weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-US"),
                )
                .expect("text format");
            let _ = f.SetParagraphAlignment(if wrap { DWRITE_PARAGRAPH_ALIGNMENT_NEAR } else { DWRITE_PARAGRAPH_ALIGNMENT_CENTER });
            let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                let trimming = DWRITE_TRIMMING {
                    granularity: if wrap { DWRITE_TRIMMING_GRANULARITY_WORD } else { DWRITE_TRIMMING_GRANULARITY_CHARACTER },
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let _ = f.SetTrimming(&trimming, &sign);
            }
            self.fonts.borrow_mut().insert(key, f.clone());
            f
        }
    }

    /// Width of `text` (DIP).
    pub fn measure(&self, text: &str, size: f32, weight: u16) -> f32 {
        let f = self.font(size, weight, false);
        unsafe {
            let s = wide(text);
            match self.dwrite.CreateTextLayout(&s, &f, 10_000.0, 100.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if layout.GetMetrics(&mut m).is_ok() {
                        m.widthIncludingTrailingWhitespace
                    } else {
                        0.0
                    }
                }
                Err(_) => 0.0,
            }
        }
    }

    /// One line, vertically centred in `r`, trimmed with "…" when too long.
    pub fn text(&self, s: &str, size: f32, weight: u16, r: D2D_RECT_F, align: Align, c: Color) {
        if s.is_empty() || c.a <= 0.0 {
            return;
        }
        self.draw_text(s, &self.font(size, weight, false), r, align, c);
    }

    /// Wrapped text from the top of `r`.
    pub fn text_wrap(&self, s: &str, size: f32, weight: u16, r: D2D_RECT_F, align: Align, c: Color) {
        if s.is_empty() || c.a <= 0.0 {
            return;
        }
        self.draw_text(s, &self.font(size, weight, true), r, align, c);
    }

    fn draw_text(&self, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, align: Align, c: Color) {
        unsafe {
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let s = wide(s);
            self.rt.DrawText(&s, f, &r, self.set(c), D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    // ------------------------------------------------------------------ icons

    /// Draws a named icon in a `size`×`size` box at `(x, y)`.
    pub fn icon(&self, name: &'static str, x: f32, y: f32, size: f32, c: Color) {
        if c.a <= 0.0 {
            return;
        }
        if !self.icons.borrow().contains_key(name) {
            let Some((view, stroke, data)) = icons::find(name) else { return };
            let Some(geo) = icons::build(&self.factory, data, stroke != 0.0 && stroke > 0.0, stroke < 0.0) else { return };
            self.icons.borrow_mut().insert(name, (geo, view, stroke));
        }
        let cache = self.icons.borrow();
        let (geo, view, stroke) = &cache[name];
        let k = size / view;
        let local = Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x, M32: y };
        let base = *self.base.borrow();
        unsafe {
            self.rt.SetTransform(&mul(&local, &base));
            if *stroke > 0.0 {
                // Keep the stroke weight similar at any size (in the icon's own units).
                self.rt.DrawGeometry(geo, self.set(c), *stroke, &self.round);
            } else {
                self.rt.FillGeometry(geo, self.set(c), None);
            }
            self.rt.SetTransform(&base);
        }
    }

    // ------------------------------------------------------------------ images

    pub fn has_image(&self, key: u64) -> bool {
        self.images.borrow().contains_key(&key)
    }

    /// Decodes PNG/JPEG bytes once and keeps the bitmap under `key` (old ones are dropped).
    pub fn load_image(&self, key: u64, bytes: &[u8]) -> bool {
        if self.has_image(key) {
            return true;
        }
        let Some(bmp) = (unsafe { self.decode(bytes) }) else { return false };
        let mut images = self.images.borrow_mut();
        // Keep up to 96 (app icons are tiny; album art is the only big one).
        while images.len() >= 96 {
            let Some(oldest) = images.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k) else { break };
            images.remove(&oldest);
        }
        let t = self.tick.get() + 1;
        self.tick.set(t);
        images.insert(key, (bmp, t));
        true
    }

    unsafe fn decode(&self, bytes: &[u8]) -> Option<ID2D1Bitmap> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
        self.rt.CreateBitmapFromWicBitmap(&conv, None).ok()
    }

    /// The colours an image is known for (album art → the song's colours): up to three distinct vivid hues,
    /// most present first, each brightened enough to glow on black. Cached per key.
    pub fn image_palette(&self, key: u64, bytes: &[u8]) -> Vec<Color> {
        if let Some(c) = self.colors.borrow().get(&key) {
            return c.clone();
        }
        let c = unsafe { Self::palette(bytes) }.unwrap_or_default();
        self.colors.borrow_mut().insert(key, c.clone());
        c
    }

    unsafe fn palette(bytes: &[u8]) -> Option<Vec<Color>> {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, 32, 32, WICBitmapInterpolationModeFant).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
        let mut px = vec![0u8; 32 * 32 * 4];
        conv.CopyPixels(std::ptr::null(), 32 * 4, &mut px).ok()?;
        // Twelve hue buckets; each pixel votes by how vivid it is, keeping a running colour per bucket.
        const N: usize = 12;
        let mut buckets = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); N];
        let mut grey = (0.0f32, 0.0f32);
        for p in px.chunks_exact(4) {
            let (b, g, r) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let d = max - min;
            let sat = if max > 0.0 { d / max } else { 0.0 };
            if sat < 0.18 || max < 0.12 {
                grey.0 += max;
                grey.1 += 1.0;
                continue;
            }
            let hue = if d == 0.0 {
                0.0
            } else if max == r {
                ((g - b) / d).rem_euclid(6.0)
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            } / 6.0;
            let i = ((hue * N as f32) as usize).min(N - 1);
            let w = sat * max;
            let e = &mut buckets[i];
            e.0 += r * w;
            e.1 += g * w;
            e.2 += b * w;
            e.3 += w;
        }
        let mut order: Vec<usize> = (0..N).filter(|&i| buckets[i].3 > 0.0).collect();
        order.sort_by(|a, b| buckets[*b].3.total_cmp(&buckets[*a].3));
        let total: f32 = buckets.iter().map(|b| b.3).sum();
        let mut out: Vec<Color> = Vec::new();
        let mut used: Vec<usize> = Vec::new();
        for i in order {
            // Skip neighbours of a hue already taken, and hues that barely appear.
            if used.iter().any(|&u| (i as i32 - u as i32).rem_euclid(N as i32).min((u as i32 - i as i32).rem_euclid(N as i32)) <= 1) {
                continue;
            }
            if !out.is_empty() && buckets[i].3 < total * 0.06 {
                break;
            }
            let (r, g, b, w) = buckets[i];
            out.push(glowing(r / w, g / w, b / w));
            used.push(i);
            if out.len() == 3 {
                break;
            }
        }
        if out.is_empty() {
            // A grey cover: a soft white light.
            let v = if grey.1 > 0.0 { (grey.0 / grey.1).clamp(0.5, 0.9) } else { 0.8 };
            out.push(Color { r: v, g: v, b: v, a: 1.0 });
        }
        Some(out)
    }

    /// Fills an ellipse with a soft light: `c` in the middle fading to nothing at the edge.
    pub fn glow(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color) {
        if rx <= 0.0 || ry <= 0.0 || c.a <= 0.0 {
            return;
        }
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: c },
                D2D1_GRADIENT_STOP { position: 0.45, color: Color { a: c.a * 0.55, ..c } },
                D2D1_GRADIENT_STOP { position: 1.0, color: Color { a: 0.0, ..c } },
            ];
            let Ok(coll) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                center: pt(cx, cy),
                gradientOriginOffset: pt(0.0, 0.0),
                radiusX: rx,
                radiusY: ry,
            };
            let Ok(brush) = self.rt.CreateRadialGradientBrush(&props, None, &coll) else { return };
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rx, radiusY: ry }, &brush);
        }
    }

    /// Clips what's drawn next to `geo` (anti-aliased edges) until `pop_shape_clip`.
    pub fn push_shape_clip(&self, geo: &ID2D1Geometry) {
        unsafe {
            let mut params = D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F { left: -1.0e6, top: -1.0e6, right: 1.0e6, bottom: 1.0e6 },
                geometricMask: std::mem::ManuallyDrop::new(Some(geo.clone())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            self.rt.PushLayer(&params, None::<&ID2D1Layer>);
            std::mem::ManuallyDrop::drop(&mut params.geometricMask);
        }
    }

    pub fn pop_shape_clip(&self) {
        unsafe { self.rt.PopLayer() };
    }

    /// Draws a loaded image filling `r` (cropped to cover), with rounded corners.
    /// Frosted glass: takes a picture of the screen behind the canvas (window at `x, y`), shrinks it a lot
    /// and blurs it, so `draw_frost` can lay it under the panel. Windows can't blur behind this kind of
    /// window itself, so this is taken once as the panel opens.
    pub fn capture_frost(&self, x: i32, y: i32) {
        const DOWN: i32 = 14;
        let (w, h) = ((self.px_w / DOWN).max(2), (self.px_h / DOWN).max(2));
        let mut px = vec![0u8; (w * h * 4) as usize];
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(dc, bmp);
            SetStretchBltMode(dc, HALFTONE);
            let _ = StretchBlt(dc, 0, 0, w, h, screen, x, y, self.px_w, self.px_h, SRCCOPY);
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader = BITMAPINFOHEADER { biSize: 40, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: 0, ..Default::default() };
            GetDIBits(dc, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
            SelectObject(dc, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
        }
        // A few box-blur passes on the small picture: smooth light and colour, no detail.
        for _ in 0..3 {
            px = box_blur(&px, w as usize, h as usize);
        }
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bmp = unsafe { self.rt.CreateBitmap(D2D_SIZE_U { width: w as u32, height: h as u32 }, Some(px.as_ptr() as *const _), (w * 4) as u32, &props) }.ok();
        *self.frost.borrow_mut() = bmp;
    }

    pub fn has_frost(&self) -> bool {
        self.frost.borrow().is_some()
    }

    /// Lays the frosted picture over the whole canvas (call inside the panel's shape clip).
    pub fn draw_frost(&self, opacity: f32) {
        let frost = self.frost.borrow();
        let Some(bmp) = frost.as_ref() else { return };
        let r = rect(0.0, 0.0, self.px_w as f32 / self.scale, self.px_h as f32 / self.scale);
        unsafe { self.rt.DrawBitmap(bmp, Some(&r), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
    }

    pub fn image(&self, key: u64, r: D2D_RECT_F, radius: f32, opacity: f32) {
        let t = self.tick.get() + 1;
        self.tick.set(t);
        let mut images = self.images.borrow_mut();
        let Some((bmp, used)) = images.get_mut(&key) else { return };
        *used = t;
        let bmp = &*bmp;
        unsafe {
            let size = bmp.GetSize();
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let k = (w / size.width).max(h / size.height);
            let dx = r.left + (w - size.width * k) / 2.0;
            let dy = r.top + (h - size.height * k) / 2.0;
            let props = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_CLAMP,
                extendModeY: D2D1_EXTEND_MODE_CLAMP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            };
            let brush_props = D2D1_BRUSH_PROPERTIES {
                opacity,
                transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: dx, M32: dy },
            };
            if let Ok(brush) = self.rt.CreateBitmapBrush(bmp, Some(&props), Some(&brush_props)) {
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
                self.rt.FillRoundedRectangle(&rr, &brush);
            }
        }
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memdc, self.old);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// A small, chubby white cat head on a dark tile, drawn at runtime for the tray (no image files needed).
/// Same shape as the phone page's logo (assets/phone.html, 100-unit SVG): wide round face, soft ears.
pub fn cat_icon(size: i32) -> Result<HICON> {
    let g = Gfx::new(size as f32, size as f32, 1.0)?;
    let s = size as f32;
    let (white, dark) = (color(0xFFFFFF, 1.0), color(0x0B0E17, 1.0));
    let ell = |x: f32, y: f32, rx: f32, ry: f32| D2D1_ELLIPSE { point: pt(x * s, y * s), radiusX: rx * s, radiusY: ry * s };
    g.begin();
    g.fill_round(rect(0.0, 0.0, s, s), s * 0.22, dark);
    // Ears: triangles with rounded tips (a dot on each corner stands in for the SVG's round join).
    let ears = [((0.19, 0.49), (0.24, 0.20), (0.43, 0.34)), ((0.81, 0.49), (0.76, 0.20), (0.57, 0.34))];
    if let Some(geo) = g.path(|sink| unsafe {
        for (a, b, c) in ears {
            sink.BeginFigure(pt(a.0 * s, a.1 * s), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(pt(b.0 * s, b.1 * s));
            sink.AddLine(pt(c.0 * s, c.1 * s));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
    }) {
        g.fill_geometry(&geo.into(), white);
    }
    for (_, b, c) in ears {
        g.fill_circle(b.0 * s, b.1 * s, s * 0.035, white);
        g.fill_circle(c.0 * s, c.1 * s, s * 0.035, white);
    }
    g.fill_ellipse(ell(0.50, 0.63, 0.37, 0.27), white);
    g.fill_circle(s * 0.37, s * 0.61, s * 0.055, dark);
    g.fill_circle(s * 0.63, s * 0.61, s * 0.055, dark);
    g.fill_ellipse(ell(0.50, 0.695, 0.032, 0.023), dark);
    unsafe {
        g.rt.EndDraw(None, None)?;
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: g.bitmap };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(mask);
        icon
    }
}


/// The Inter font files, built into the exe (SIL Open Font License, see assets/fonts/Inter-LICENSE.txt).
const INTER: [(&str, &[u8]); 5] = [
    ("Inter-Light.ttf", include_bytes!("../assets/fonts/Inter-Light.ttf")),
    ("Inter-Regular.ttf", include_bytes!("../assets/fonts/Inter-Regular.ttf")),
    ("Inter-Medium.ttf", include_bytes!("../assets/fonts/Inter-Medium.ttf")),
    ("Inter-SemiBold.ttf", include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
    ("Inter-Bold.ttf", include_bytes!("../assets/fonts/Inter-Bold.ttf")),
];

/// Writes the fonts to %LOCALAPPDATA%\FathNotch\fonts once and loads them from there: DirectWrite maps
/// files rather than copying them, so the fonts don't add to the app's own memory.
fn inter_fonts(dwrite: &IDWriteFactory) -> Option<IDWriteFontCollection> {
    use windows::core::Interface;
    let dir = crate::services::store::root().join("fonts");
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        let f5: IDWriteFactory5 = dwrite.cast().ok()?;
        let builder = f5.CreateFontSetBuilder().ok()?;
        for (name, bytes) in INTER {
            let path = dir.join(name);
            if std::fs::metadata(&path).map(|m| m.len() as usize != bytes.len()).unwrap_or(true) {
                std::fs::write(&path, bytes).ok()?;
            }
            let file = dwrite.CreateFontFileReference(&windows::core::HSTRING::from(path.as_os_str()), None).ok()?;
            builder.AddFontFile(&file).ok()?;
        }
        let set = builder.CreateFontSet().ok()?;
        let coll = f5.CreateFontCollectionFromFontSet(&set).ok()?;
        coll.cast().ok()
    }
}
/// A colour brightened to glow on black (its brightest channel near full), keeping the hue.
fn glowing(r: f32, g: f32, b: f32) -> Color {
    let max = r.max(g).max(b).max(0.001);
    let lift = (0.95 / max).min(3.0);
    Color { r: (r * lift).min(1.0), g: (g * lift).min(1.0), b: (b * lift).min(1.0), a: 1.0 }
}

/// One 3×3 box-blur pass over BGRA pixels (edges clamp).
fn box_blur(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (sx, sy) = ((x as i32 + dx).clamp(0, w as i32 - 1) as usize, (y as i32 + dy).clamp(0, h as i32 - 1) as usize);
                    let i = (sy * w + sx) * 4;
                    acc[0] += src[i] as u32;
                    acc[1] += src[i + 1] as u32;
                    acc[2] += src[i + 2] as u32;
                    n += 1;
                }
            }
            let o = (y * w + x) * 4;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
        }
    }
    out
}
