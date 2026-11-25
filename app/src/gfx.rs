//! Anti-aliased shapes for the custom drawn windows (panel, menu, buttons,
//! check boxes, drop-down lists): drawn with tiny-skia in memory, then
//! copied onto a GDI device context, where GDI draws the text on top
//! (ClearType needs an opaque background, which the canvas gives).

use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{COLORREF, RECT};

#[cfg(not(windows))]
#[allow(clippy::upper_case_acronyms)]
pub type COLORREF = u32;
#[cfg(not(windows))]
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A canvas covering `rect` of a device context.
pub struct Canvas {
    pixmap: Pixmap,
    pub left: i32,
    pub top: i32,
}

fn color(c: COLORREF, alpha: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8((c & 0xFF) as u8, (c >> 8 & 0xFF) as u8, (c >> 16 & 0xFF) as u8, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

fn paint(c: COLORREF, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(c, alpha));
    p.anti_alias = true;
    p
}

/// Rounded rectangle path (radius clamped to half the smaller side).
fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    // Cubic approximation of a quarter circle.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

impl Canvas {
    /// Canvas for `r` (device coordinates), filled with `bg`.
    pub fn new(r: RECT, bg: COLORREF) -> Option<Self> {
        let (w, h) = ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32);
        let mut pixmap = Pixmap::new(w, h)?;
        pixmap.fill(color(bg, 1.0));
        Some(Self { pixmap, left: r.left, top: r.top })
    }

    fn local(&self, r: RECT) -> (f32, f32, f32, f32) {
        ((r.left - self.left) as f32, (r.top - self.top) as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32)
    }

    /// Filled rounded rectangle, with an optional 1 px border inside it.
    pub fn round_rect(&mut self, r: RECT, radius: f32, fill: COLORREF, alpha: f32, border: Option<COLORREF>) {
        let (x, y, w, h) = self.local(r);
        if let Some(path) = rounded(x, y, w, h, radius) {
            self.pixmap.fill_path(&path, &paint(fill, alpha), FillRule::Winding, Transform::identity(), None);
        }
        if let Some(b) = border {
            if let Some(path) = rounded(x + 0.5, y + 0.5, w - 1.0, h - 1.0, (radius - 0.5).max(0.0)) {
                let stroke = Stroke { width: 1.0, ..Stroke::default() };
                self.pixmap.stroke_path(&path, &paint(b, alpha), &stroke, Transform::identity(), None);
            }
        }
    }

    /// Outline only.
    pub fn outline(&mut self, r: RECT, radius: f32, c: COLORREF, width: f32) {
        let (x, y, w, h) = self.local(r);
        let half = width / 2.0;
        if let Some(path) = rounded(x + half, y + half, w - width, h - width, (radius - half).max(0.0)) {
            let stroke = Stroke { width, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(c, 1.0), &stroke, Transform::identity(), None);
        }
    }

    pub fn circle(&mut self, cx: i32, cy: i32, radius: f32, c: COLORREF, alpha: f32) {
        let (x, y) = ((cx - self.left) as f32, (cy - self.top) as f32);
        if let Some(path) = PathBuilder::from_circle(x, y, radius) {
            self.pixmap.fill_path(&path, &paint(c, alpha), FillRule::Winding, Transform::identity(), None);
        }
    }

    /// Open polyline (device coordinates).
    pub fn polyline(&mut self, points: &[(f32, f32)], c: COLORREF, width: f32) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            let (x, y) = (x - self.left as f32, y - self.top as f32);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        if points.len() == 1 {
            let (x, y) = points[0];
            pb.line_to(x - self.left as f32 + 0.1, y - self.top as f32);
        }
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(c, 1.0), &stroke, Transform::identity(), None);
        }
    }

    /// Area under a polyline down to `base` (device y), fading from `alpha`
    /// at the top to nothing at the base.
    pub fn area(&mut self, points: &[(f32, f32)], base: f32, c: COLORREF, alpha: f32) {
        if points.len() < 2 {
            return;
        }
        let (l, t) = (self.left as f32, self.top as f32);
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0 - l, base - t);
        for &(x, y) in points {
            pb.line_to(x - l, y - t);
        }
        pb.line_to(points[points.len() - 1].0 - l, base - t);
        pb.close();
        let top = points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let Some(path) = pb.finish() else { return };
        let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, top - t),
            tiny_skia::Point::from_xy(0.0, base - t),
            vec![tiny_skia::GradientStop::new(0.0, color(c, alpha)), tiny_skia::GradientStop::new(1.0, color(c, 0.0))],
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let p = Paint { shader, anti_alias: true, ..Paint::default() };
        self.pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }

    /// Check mark inside `r`.
    pub fn check(&mut self, r: RECT, c: COLORREF, width: f32) {
        let (x, y, w, h) = self.local(r);
        let pts = [(x + w * 0.24, y + h * 0.52), (x + w * 0.43, y + h * 0.70), (x + w * 0.76, y + h * 0.32)];
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0, pts[0].1);
        pb.line_to(pts[1].0, pts[1].1);
        pb.line_to(pts[2].0, pts[2].1);
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(c, 1.0), &stroke, Transform::identity(), None);
        }
    }

    /// Downward chevron centred in `r`.
    pub fn chevron(&mut self, r: RECT, c: COLORREF, width: f32) {
        let (x, y, w, h) = self.local(r);
        let (cx, cy, s) = (x + w / 2.0, y + h / 2.0, w.min(h) * 0.22);
        let mut pb = PathBuilder::new();
        pb.move_to(cx - s, cy - s / 2.0);
        pb.line_to(cx, cy + s / 2.0);
        pb.line_to(cx + s, cy - s / 2.0);
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(c, 1.0), &stroke, Transform::identity(), None);
        }
    }

    /// Fills a plain rectangle (no anti-aliasing needed).
    pub fn fill(&mut self, r: RECT, c: COLORREF) {
        let (x, y, w, h) = self.local(r);
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            self.pixmap.fill_rect(rect, &paint(c, 1.0), Transform::identity(), None);
        }
    }

    /// Composites straight-alpha RGBA pixels (an icon) at (x, y).
    pub fn image(&mut self, rgba: &[u8], n: usize, x: i32, y: i32) {
        let Some(size) = tiny_skia::IntSize::from_wh(n as u32, n as u32) else { return };
        let mut data = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            let a = px[3] as u32;
            let pm = |c: u8| (c as u32 * a / 255) as u8;
            data.extend_from_slice(&[pm(px[0]), pm(px[1]), pm(px[2]), px[3]]);
        }
        let Some(img) = Pixmap::from_vec(data, size) else { return };
        self.pixmap.draw_pixmap(x - self.left, y - self.top, img.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }

    /// Copies the canvas onto `dc` at its place.
    #[cfg(windows)]
    pub fn blit(&self, dc: windows_sys::Win32::Graphics::Gdi::HDC) {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::Graphics::Gdi::{SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS};
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        // Opaque canvas: premultiplied RGBA is plain RGBA; GDI wants BGRA.
        let mut bgra = self.pixmap.data().to_vec();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        // SAFETY: BITMAPINFO describes exactly `bgra`.
        unsafe {
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w as i32;
            info.bmiHeader.biHeight = -(h as i32);
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            SetDIBitsToDevice(dc, self.left, self.top, w, h, 0, 0, 0, h, bgra.as_ptr().cast(), &info, DIB_RGB_COLORS);
        }
    }

    #[cfg(test)]
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let p = self.pixmap.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn shapes_land_where_asked() {
        let mut c = Canvas::new(r(10, 10, 110, 60), 0x000000).unwrap();
        c.round_rect(r(20, 20, 60, 40), 6.0, 0x0000FF, 1.0, None);
        // Centre of the rectangle is red (COLORREF 0x0000FF = red).
        assert_eq!(c.pixel(30, 20), [255, 0, 0, 255]);
        // Its rounded corner stays background.
        assert_eq!(c.pixel(10, 10), [0, 0, 0, 255]);
        // Outside: background.
        assert_eq!(c.pixel(80, 40), [0, 0, 0, 255]);
    }

    #[test]
    fn degenerate_sizes_are_ignored() {
        let mut c = Canvas::new(r(0, 0, 10, 10), 0xFFFFFF).unwrap();
        c.round_rect(r(5, 5, 5, 9), 3.0, 0, 1.0, Some(0));
        c.polyline(&[], 0, 1.0);
        c.area(&[(1.0, 1.0)], 9.0, 0, 0.5);
        assert_eq!(c.pixel(5, 6), [255, 255, 255, 255]);
    }
}
