//! The notch outline. One path covers every state: a notch hanging from the top edge (concave ears,
//! rounded bottom), a floating capsule (all corners round), and everything in between.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Outline {
    /// Left edge of the body (DIP), not counting the ears.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Concave flare where the body meets the top edge (0 = none).
    pub ear: f32,
    /// Bottom corner radius.
    pub radius: f32,
    /// Top corner radius; only used once the ears are gone.
    pub top_radius: f32,
}

fn pt(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}

fn arc(sink: &ID2D1GeometrySink, to: D2D_POINT_2F, r: f32, clockwise: bool) {
    unsafe {
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: to,
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
    }
}

impl Outline {
    pub fn build(&self, factory: &ID2D1Factory) -> Result<ID2D1PathGeometry> {
        let Outline { x, y, w, h, ear, .. } = *self;
        let top_r = if ear > 0.01 { 0.0 } else { self.top_radius.clamp(0.0, (w / 2.0).min(h / 2.0)) };
        let r = self.radius.clamp(0.0, (w / 2.0).min(h - ear.max(top_r)));
        let (l, rgt, bottom) = (x, x + w, y + h);
        unsafe {
            let geo = factory.CreatePathGeometry()?;
            let sink = geo.Open()?;
            if top_r > 0.01 {
                sink.BeginFigure(pt(l, y + top_r), D2D1_FIGURE_BEGIN_FILLED);
            } else {
                sink.BeginFigure(pt(l - ear, y), D2D1_FIGURE_BEGIN_FILLED);
                if ear > 0.01 {
                    arc(&sink, pt(l, y + ear), ear, true);
                }
            }
            sink.AddLine(pt(l, bottom - r));
            arc(&sink, pt(l + r, bottom), r, false);
            sink.AddLine(pt(rgt - r, bottom));
            arc(&sink, pt(rgt, bottom - r), r, false);
            if top_r > 0.01 {
                sink.AddLine(pt(rgt, y + top_r));
                arc(&sink, pt(rgt - top_r, y), top_r, false);
                sink.AddLine(pt(l + top_r, y));
                arc(&sink, pt(l, y + top_r), top_r, false);
            } else {
                sink.AddLine(pt(rgt, y + ear));
                if ear > 0.01 {
                    arc(&sink, pt(rgt + ear, y), ear, true);
                }
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            Ok(geo)
        }
    }
}
