//! Software renderer for egui: rasterises the tessellated meshes (vertex
//! colours × texture, premultiplied alpha, clip rectangles) into an RGBA
//! buffer that the window then copies to the screen. No GPU, no driver:
//! the windows look the same on every PC, and in a virtual machine.
//!
//! egui anti-aliases with "feathering" (thin extra triangles fading to
//! transparent), so plain coverage of pixel centres is enough here.

use egui::epaint::{ClippedPrimitive, Primitive, Vertex};
use egui::{Color32, TextureId, TexturesDelta};
use std::collections::HashMap;

struct Texture {
    size: [usize; 2],
    /// Premultiplied RGBA.
    pixels: Vec<Color32>,
    linear: bool,
}

pub struct Raster {
    pub width: usize,
    pub height: usize,
    /// Premultiplied RGBA, row by row.
    pub buf: Vec<[u8; 4]>,
    textures: HashMap<TextureId, Texture>,
}

impl Default for Raster {
    fn default() -> Self {
        Self::new()
    }
}

impl Raster {
    pub fn new() -> Self {
        Self { width: 0, height: 0, buf: Vec::new(), textures: HashMap::new() }
    }

    /// Applies the new textures of `delta` (call before painting) and
    /// empties it; returns the ids to free once painted.
    pub fn set_textures(&mut self, delta: &mut TexturesDelta) -> Vec<TextureId> {
        for (id, d) in delta.set.iter().flat_map(|(id, list)| list.iter().map(move |d| (id, d))) {
            let egui::ImageData::Color(img) = &d.image;
            let linear = d.options.magnification == egui::TextureFilter::Linear;
            match d.pos {
                None => {
                    self.textures.insert(*id, Texture { size: img.size, pixels: img.pixels.clone(), linear });
                }
                Some([x0, y0]) => {
                    let Some(t) = self.textures.get_mut(id) else { continue };
                    for y in 0..img.size[1] {
                        for x in 0..img.size[0] {
                            let (tx, ty) = (x0 + x, y0 + y);
                            if tx < t.size[0] && ty < t.size[1] {
                                t.pixels[ty * t.size[0] + tx] = img.pixels[y * img.size[0] + x];
                            }
                        }
                    }
                }
            }
        }
        let free = std::mem::take(&mut delta.free);
        delta.clear();
        free.into_iter().collect()
    }

    pub fn free_textures(&mut self, ids: &[TextureId]) {
        for id in ids {
            self.textures.remove(id);
        }
    }

    /// Paints `prims` (points × `ppp` = pixels) over `clear`.
    pub fn paint(&mut self, width: usize, height: usize, ppp: f32, clear: Color32, prims: &[ClippedPrimitive]) {
        self.width = width;
        self.height = height;
        self.buf.clear();
        self.buf.resize(width * height, clear.to_array());
        for p in prims {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let clip = [
                (p.clip_rect.min.x * ppp).round().max(0.0) as i32,
                (p.clip_rect.min.y * ppp).round().max(0.0) as i32,
                ((p.clip_rect.max.x * ppp).round() as i32).min(width as i32),
                ((p.clip_rect.max.y * ppp).round() as i32).min(height as i32),
            ];
            if clip[0] >= clip[2] || clip[1] >= clip[3] {
                continue;
            }
            let tex = self.textures.get(&mesh.texture_id);
            for tri in mesh.indices.chunks_exact(3) {
                let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
                triangle(&mut self.buf, width, clip, ppp, v, tex);
            }
        }
    }
}

fn mul(a: u8, b: u8) -> u32 {
    (a as u32 * b as u32 + 127) / 255
}

fn sample(t: &Texture, u: f32, v: f32) -> [u8; 4] {
    let (w, h) = (t.size[0], t.size[1]);
    if w == 0 || h == 0 {
        return [255; 4];
    }
    if !t.linear {
        let x = ((u * w as f32) as isize).clamp(0, w as isize - 1) as usize;
        let y = ((v * h as f32) as isize).clamp(0, h as isize - 1) as usize;
        return t.pixels[y * w + x].to_array();
    }
    let fx = (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
    let fy = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
    let p = |x: usize, y: usize| t.pixels[y * w + x].to_array();
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = a[i] as f32 + (b[i] as f32 - a[i] as f32) * ax;
        let bottom = c[i] as f32 + (d[i] as f32 - c[i] as f32) * ax;
        out[i] = (top + (bottom - top) * ay).round() as u8;
    }
    out
}

/// Edge function: twice the signed area of (a, b, p).
fn edge(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

fn triangle(buf: &mut [[u8; 4]], width: usize, clip: [i32; 4], ppp: f32, v: [&Vertex; 3], tex: Option<&Texture>) {
    let p: Vec<(f32, f32)> = v.iter().map(|v| (v.pos.x * ppp, v.pos.y * ppp)).collect();
    let mut area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-6 {
        return;
    }
    // Wind counter-clockwise (positive area) so one test fits every edge.
    let (i1, i2) = if area > 0.0 { (1, 2) } else { (2, 1) };
    let (a, b, c) = (p[0], p[i1], p[i2]);
    let (va, vb, vc) = (v[0], v[i1], v[i2]);
    area = area.abs();
    let x0 = (a.0.min(b.0).min(c.0).floor() as i32).max(clip[0]);
    let x1 = (a.0.max(b.0).max(c.0).ceil() as i32).min(clip[2]);
    let y0 = (a.1.min(b.1).min(c.1).floor() as i32).max(clip[1]);
    let y1 = (a.1.max(b.1).max(c.1).ceil() as i32).min(clip[3]);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Top-left rule: pixels exactly on a shared edge belong to one triangle.
    let bias = |s: (f32, f32), e: (f32, f32)| {
        let top_left = (s.1 == e.1 && e.0 < s.0) || e.1 > s.1;
        if top_left {
            0.0
        } else {
            -1e-4
        }
    };
    let (b0, b1, b2) = (bias(b, c), bias(c, a), bias(a, b));
    let same_colour = va.color == vb.color && va.color == vc.color;
    let same_uv = va.uv == vb.uv && va.uv == vc.uv;
    let flat = if same_uv { tex.map_or([255; 4], |t| sample(t, va.uv.x, va.uv.y)) } else { [255; 4] };
    for y in y0..y1 {
        let py = y as f32 + 0.5;
        let row = y as usize * width;
        for x in x0..x1 {
            let pt = (x as f32 + 0.5, py);
            let w0 = edge(b, c, pt);
            let w1 = edge(c, a, pt);
            let w2 = edge(a, b, pt);
            if w0 + b0 < 0.0 || w1 + b1 < 0.0 || w2 + b2 < 0.0 {
                continue;
            }
            let (l0, l1, l2) = (w0 / area, w1 / area, w2 / area);
            let col = if same_colour {
                va.color.to_array()
            } else {
                let (ca, cb, cc) = (va.color.to_array(), vb.color.to_array(), vc.color.to_array());
                let mut o = [0u8; 4];
                for i in 0..4 {
                    o[i] = (ca[i] as f32 * l0 + cb[i] as f32 * l1 + cc[i] as f32 * l2).round().clamp(0.0, 255.0) as u8;
                }
                o
            };
            let texel = if same_uv {
                flat
            } else {
                match tex {
                    Some(t) => {
                        let u = va.uv.x * l0 + vb.uv.x * l1 + vc.uv.x * l2;
                        let v = va.uv.y * l0 + vb.uv.y * l1 + vc.uv.y * l2;
                        sample(t, u, v)
                    }
                    None => [255; 4],
                }
            };
            let src = [mul(col[0], texel[0]), mul(col[1], texel[1]), mul(col[2], texel[2]), mul(col[3], texel[3])];
            if src[3] == 0 && src[0] == 0 && src[1] == 0 && src[2] == 0 {
                continue;
            }
            let d = &mut buf[row + x as usize];
            let inv = 255 - src[3];
            for i in 0..4 {
                d[i] = (src[i] + mul(d[i], inv as u8)).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Rect};

    fn render(f: impl FnMut(&mut egui::Ui)) -> Raster {
        let ctx = egui::Context::default();
        let input =
            egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(120.0, 60.0))), ..Default::default() };
        let out = ctx.run_ui(input, f);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut r = Raster::new();
        let mut delta = out.textures_delta;
        r.set_textures(&mut delta);
        r.paint(120, 60, 1.0, Color32::BLACK, &prims);
        r
    }

    #[test]
    fn fills_shapes_and_text() {
        let r = render(|ui| {
            ui.painter().rect_filled(Rect::from_min_max(pos2(10.0, 10.0), pos2(30.0, 30.0)), 0.0, Color32::from_rgb(255, 0, 0));
            ui.painter().text(pos2(60.0, 30.0), egui::Align2::CENTER_CENTER, "Hi", egui::FontId::proportional(20.0), Color32::WHITE);
        });
        assert_eq!(r.buf[20 * 120 + 20], [255, 0, 0, 255]);
        assert_eq!(r.buf[50 * 120 + 5], [0, 0, 0, 255]);
        // Some text pixels were drawn around (60, 30).
        let lit = (20..40).flat_map(|y| (50..70).map(move |x| (x, y))).filter(|(x, y)| r.buf[y * 120 + x][0] > 128).count();
        assert!(lit > 10, "{lit}");
    }

    #[test]
    fn clip_rect_is_respected() {
        let r = render(|ui| {
            ui.set_clip_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(15.0, 60.0)));
            ui.painter().rect_filled(Rect::from_min_max(pos2(0.0, 0.0), pos2(120.0, 60.0)), 0.0, Color32::WHITE);
        });
        assert_eq!(r.buf[10 * 120 + 5], [255, 255, 255, 255]);
        assert_eq!(r.buf[10 * 120 + 40], [0, 0, 0, 255]);
    }
}
