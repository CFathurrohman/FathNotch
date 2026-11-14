//! Icons: SVG path data (see icon_data.rs) turned into Direct2D geometry.

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;

use super::icon_data::ICONS;

/// (viewBox size, stroke width (0 = even-odd fill, < 0 = non-zero fill), path data)
pub fn find(name: &str) -> Option<(f32, f32, &'static str)> {
    ICONS.iter().find(|(n, ..)| *n == name).map(|(_, v, s, d)| (*v, *s, *d))
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Tokens<'a> {
    fn skip(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b',' | b'\n' | b'\t' | b'\r') {
            self.i += 1;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn has_number(&mut self) -> bool {
        self.skip();
        matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'.' || *c == b'+')
    }

    fn number(&mut self) -> f32 {
        self.skip();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                self.i += 1;
            } else if (c == b'e' || c == b'E') && self.i > start {
                self.i += 1;
                if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok().and_then(|t| t.parse().ok()).unwrap_or(0.0)
    }

    /// Arc flags may be written without separators ("011").
    fn flag(&mut self) -> bool {
        self.skip();
        let c = self.s.get(self.i).copied().unwrap_or(b'0');
        self.i += 1;
        c == b'1'
    }
}

fn p(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}

/// Parses SVG path data into a geometry. `stroked` figures are hollow; `winding` picks the non-zero fill rule.
pub fn build(factory: &ID2D1Factory, data: &str, stroked: bool, winding: bool) -> Option<ID2D1PathGeometry> {
    unsafe {
        let geo = factory.CreatePathGeometry().ok()?;
        let sink = geo.Open().ok()?;
        sink.SetFillMode(if winding { D2D1_FILL_MODE_WINDING } else { D2D1_FILL_MODE_ALTERNATE });
        let begin = if stroked { D2D1_FIGURE_BEGIN_HOLLOW } else { D2D1_FIGURE_BEGIN_FILLED };
        let mut t = Tokens { s: data.as_bytes(), i: 0 };
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        let mut last_ctrl: Option<(f32, f32)> = None; // for S/T reflection
        let mut open = false;
        let mut cmd = b'M';
        loop {
            if let Some(c) = t.command() {
                cmd = c;
            } else if !t.has_number() {
                break;
            }
            let rel = cmd.is_ascii_lowercase();
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            if !open && !matches!(cmd.to_ascii_uppercase(), b'M' | b'Z') {
                // Drawing after Z without a moveto starts again at the subpath start.
                sink.BeginFigure(p(x, y), begin);
                open = true;
            }
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    x = ox + t.number();
                    y = oy + t.number();
                    sx = x;
                    sy = y;
                    sink.BeginFigure(p(x, y), begin);
                    open = true;
                    last_ctrl = None;
                    // Further pairs after M are line-tos.
                    cmd = if rel { b'l' } else { b'L' };
                }
                b'L' => {
                    x = ox + t.number();
                    y = oy + t.number();
                    sink.AddLine(p(x, y));
                    last_ctrl = None;
                }
                b'H' => {
                    x = (if rel { x } else { 0.0 }) + t.number();
                    sink.AddLine(p(x, y));
                    last_ctrl = None;
                }
                b'V' => {
                    y = (if rel { y } else { 0.0 }) + t.number();
                    sink.AddLine(p(x, y));
                    last_ctrl = None;
                }
                b'C' => {
                    let (x1, y1) = (ox + t.number(), oy + t.number());
                    let (x2, y2) = (ox + t.number(), oy + t.number());
                    let (ex, ey) = (ox + t.number(), oy + t.number());
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(x2, y2), point3: p(ex, ey) });
                    last_ctrl = Some((x2, y2));
                    x = ex;
                    y = ey;
                }
                b'S' => {
                    let (x1, y1) = last_ctrl.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                    let (x2, y2) = (ox + t.number(), oy + t.number());
                    let (ex, ey) = (ox + t.number(), oy + t.number());
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(x2, y2), point3: p(ex, ey) });
                    last_ctrl = Some((x2, y2));
                    x = ex;
                    y = ey;
                }
                b'Q' => {
                    let (x1, y1) = (ox + t.number(), oy + t.number());
                    let (ex, ey) = (ox + t.number(), oy + t.number());
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(ex, ey) });
                    last_ctrl = Some((x1, y1));
                    x = ex;
                    y = ey;
                }
                b'T' => {
                    let (x1, y1) = last_ctrl.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                    let (ex, ey) = (ox + t.number(), oy + t.number());
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(x1, y1), point2: p(ex, ey) });
                    last_ctrl = Some((x1, y1));
                    x = ex;
                    y = ey;
                }
                b'A' => {
                    let rx = t.number().abs();
                    let ry = t.number().abs();
                    let rot = t.number();
                    let large = t.flag();
                    let sweep = t.flag();
                    let (ex, ey) = (ox + t.number(), oy + t.number());
                    if rx < 1e-4 || ry < 1e-4 {
                        sink.AddLine(p(ex, ey));
                    } else {
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: p(ex, ey),
                            size: D2D_SIZE_F { width: rx, height: ry },
                            rotationAngle: rot,
                            sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                            arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                    }
                    x = ex;
                    y = ey;
                    last_ctrl = None;
                }
                b'Z' => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    x = sx;
                    y = sy;
                    last_ctrl = None;
                    // A command letter must follow; if numbers follow, treat as an implicit moveto.
                    if t.has_number() {
                        cmd = if rel { b'm' } else { b'M' };
                    }
                }
                _ => break,
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close().ok()?;
        Some(geo)
    }
}
