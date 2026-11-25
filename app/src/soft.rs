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
    /// Premultiplied BGRA.
    pixels: Vec<[u8; 4]>,
    linear: bool,
}

pub struct Raster {
    pub width: usize,
    pub height: usize,
    /// Premultiplied BGRA (the order of Windows bitmaps), row by row.
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
                    self.textures
                        .insert(*id, Texture { size: img.size, pixels: img.pixels.iter().map(|p| bgra(p.to_array())).collect(), linear });
                }
                Some([x0, y0]) => {
                    let Some(t) = self.textures.get_mut(id) else { continue };
                    for y in 0..img.size[1] {
                        for x in 0..img.size[0] {
                            let (tx, ty) = (x0 + x, y0 + y);
                            if tx < t.size[0] && ty < t.size[1] {
                                t.pixels[ty * t.size[0] + tx] = bgra(img.pixels[y * img.size[0] + x].to_array());
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

    /// Paints `prims` (points × `ppp` = pixels) over `clear`. The rows are
    /// shared out between the processor cores.
    pub fn paint(&mut self, width: usize, height: usize, ppp: f32, clear: Color32, prims: &[ClippedPrimitive]) {
        self.width = width;
        self.height = height;
        self.buf.clear();
        self.buf.resize(width * height, bgra(clear.to_array()));
        if width == 0 || height == 0 {
            return;
        }
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
        let threads = if width * height < 150_000 { 1 } else { threads };
        let band = height.div_ceil(threads);
        let textures = &self.textures;
        let paint_band = |rows: &mut [[u8; 4]], y0: usize| {
            let y1 = y0 + rows.len() / width;
            for p in prims {
                let Primitive::Mesh(mesh) = &p.primitive else { continue };
                let clip = [
                    (p.clip_rect.min.x * ppp).round().max(0.0) as i32,
                    ((p.clip_rect.min.y * ppp).round() as i32).max(y0 as i32),
                    ((p.clip_rect.max.x * ppp).round() as i32).min(width as i32),
                    ((p.clip_rect.max.y * ppp).round() as i32).min(y1 as i32),
                ];
                if clip[0] >= clip[2] || clip[1] >= clip[3] {
                    continue;
                }
                let tex = textures.get(&mesh.texture_id);
                for tri in mesh.indices.chunks_exact(3) {
                    let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
                    triangle(rows, y0, width, clip, ppp, v, tex);
                }
            }
        };
        if threads == 1 {
            paint_band(&mut self.buf, 0);
        } else {
            std::thread::scope(|s| {
                for (i, rows) in self.buf.chunks_mut(band * width).enumerate() {
                    let paint_band = &paint_band;
                    s.spawn(move || paint_band(rows, i * band));
                }
            });
        }
    }
}

/// RGBA → BGRA (the order of Windows bitmaps).
fn bgra(c: [u8; 4]) -> [u8; 4] {
    [c[2], c[1], c[0], c[3]]
}

/// a × b / 255, rounded.
fn mul(a: u32, b: u32) -> u32 {
    let x = a * b + 128;
    (x + (x >> 8)) >> 8
}

/// Texel at (u, v), BGRA.
fn sample(t: &Texture, u: f32, v: f32) -> [u8; 4] {
    let (w, h) = (t.size[0], t.size[1]);
    if w == 0 || h == 0 {
        return [255; 4];
    }
    if !t.linear {
        let x = ((u * w as f32) as isize).clamp(0, w as isize - 1) as usize;
        let y = ((v * h as f32) as isize).clamp(0, h as isize - 1) as usize;
        return t.pixels[y * w + x];
    }
    let fx = (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
    let fy = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (ax, ay) = (((fx - x0 as f32) * 256.0) as u32, ((fy - y0 as f32) * 256.0) as u32);
    let (a, b, c, d) = (t.pixels[y0 * w + x0], t.pixels[y0 * w + x1], t.pixels[y1 * w + x0], t.pixels[y1 * w + x1]);
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = a[i] as u32 * (256 - ax) + b[i] as u32 * ax;
        let bottom = c[i] as u32 * (256 - ax) + d[i] as u32 * ax;
        out[i] = ((top * (256 - ay) + bottom * ay) >> 16) as u8;
    }
    out
}

/// Edge function: twice the signed area of (a, b, p).
fn edge(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Source over destination, premultiplied.
#[inline]
fn blend(d: &mut [u8; 4], s: [u32; 4]) {
    if s[3] == 255 {
        *d = [s[0] as u8, s[1] as u8, s[2] as u8, 255];
        return;
    }
    let inv = 255 - s[3];
    for i in 0..4 {
        d[i] = (s[i] + mul(d[i] as u32, inv)).min(255) as u8;
    }
}

/// Rasterises one triangle into `rows` (the image rows from `y0`).
fn triangle(rows: &mut [[u8; 4]], y0: usize, width: usize, clip: [i32; 4], ppp: f32, v: [&Vertex; 3], tex: Option<&Texture>) {
    let p: [(f32, f32); 3] =
        [(v[0].pos.x * ppp, v[0].pos.y * ppp), (v[1].pos.x * ppp, v[1].pos.y * ppp), (v[2].pos.x * ppp, v[2].pos.y * ppp)];
    let mut area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-6 {
        return;
    }
    // Wind counter-clockwise (positive area) so one test fits every edge.
    let (i1, i2) = if area > 0.0 { (1, 2) } else { (2, 1) };
    let (a, b, c) = (p[0], p[i1], p[i2]);
    let (va, vb, vc) = (v[0], v[i1], v[i2]);
    area = area.abs();
    let x_lo = (a.0.min(b.0).min(c.0).floor() as i32).max(clip[0]);
    let x_hi = (a.0.max(b.0).max(c.0).ceil() as i32).min(clip[2]);
    let y_lo = (a.1.min(b.1).min(c.1).floor() as i32).max(clip[1]);
    let y_hi = (a.1.max(b.1).max(c.1).ceil() as i32).min(clip[3]);
    if x_lo >= x_hi || y_lo >= y_hi {
        return;
    }
    // Top-left rule: pixels exactly on a shared edge belong to one triangle.
    let bias = |s: (f32, f32), e: (f32, f32)| if (s.1 == e.1 && e.0 < s.0) || e.1 > s.1 { 0.0 } else { -1e-4 };
    // Each edge function is linear in x: w = k·x + m(y).
    let edges = [(b, c, bias(b, c)), (c, a, bias(c, a)), (a, b, bias(a, b))];
    let ks = [-(c.1 - b.1), -(a.1 - c.1), -(b.1 - a.1)];

    let same_colour = va.color == vb.color && va.color == vc.color;
    let same_uv = va.uv == vb.uv && va.uv == vc.uv;
    let flat_texel = if same_uv { tex.map_or([255; 4], |t| sample(t, va.uv.x, va.uv.y)) } else { [255; 4] };
    let ca = bgra(va.color.to_array());
    let (cb, cc) = (bgra(vb.color.to_array()), bgra(vc.color.to_array()));
    let solid = same_colour && same_uv;
    let solid_src = [
        mul(ca[0] as u32, flat_texel[0] as u32),
        mul(ca[1] as u32, flat_texel[1] as u32),
        mul(ca[2] as u32, flat_texel[2] as u32),
        mul(ca[3] as u32, flat_texel[3] as u32),
    ];
    if solid && solid_src == [0; 4] {
        return;
    }

    for y in y_lo..y_hi {
        let py = y as f32 + 0.5;
        // Span of x where all three edge functions are >= 0.
        let (mut xs, mut xe) = (x_lo as f32, x_hi as f32 - 1.0);
        let mut empty = false;
        let mut w_at0 = [0f32; 3];
        for (i, &(s, e, bi)) in edges.iter().enumerate() {
            // w(px) = edge(s, e, (px, py)), px = x + 0.5.
            let w0 = edge(s, e, (0.5, py)) + bi;
            w_at0[i] = w0 - bi;
            let k = ks[i];
            if k.abs() < 1e-12 {
                if w0 < 0.0 {
                    empty = true;
                }
            } else if k > 0.0 {
                xs = xs.max((-w0 / k).ceil());
            } else {
                xe = xe.min((-w0 / k).floor());
            }
        }
        if empty || xs > xe {
            continue;
        }
        let (xs, xe) = (xs.max(x_lo as f32) as i32, xe.min(x_hi as f32 - 1.0) as i32);
        let row = (y as usize - y0) * width;
        if solid {
            let span = &mut rows[row + xs as usize..=row + xe as usize];
            if solid_src[3] == 255 {
                span.fill([solid_src[0] as u8, solid_src[1] as u8, solid_src[2] as u8, 255]);
            } else {
                for d in span {
                    blend(d, solid_src);
                }
            }
            continue;
        }
        for x in xs..=xe {
            let fx = x as f32;
            let w = [w_at0[0] + ks[0] * fx, w_at0[1] + ks[1] * fx, w_at0[2] + ks[2] * fx];
            let d = &mut rows[row + x as usize];
            let (l0, l1, l2) = (w[0] / area, w[1] / area, w[2] / area);
            let col = if same_colour {
                ca
            } else {
                let mut o = [0u8; 4];
                for i in 0..4 {
                    o[i] = (ca[i] as f32 * l0 + cb[i] as f32 * l1 + cc[i] as f32 * l2).round().clamp(0.0, 255.0) as u8;
                }
                o
            };
            let texel = if same_uv {
                flat_texel
            } else {
                match tex {
                    Some(t) => sample(t, va.uv.x * l0 + vb.uv.x * l1 + vc.uv.x * l2, va.uv.y * l0 + vb.uv.y * l1 + vc.uv.y * l2),
                    None => [255; 4],
                }
            };
            let s = [
                mul(col[0] as u32, texel[0] as u32),
                mul(col[1] as u32, texel[1] as u32),
                mul(col[2] as u32, texel[2] as u32),
                mul(col[3] as u32, texel[3] as u32),
            ];
            if s[3] != 0 || s[0] != 0 || s[1] != 0 || s[2] != 0 {
                blend(d, s);
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
        assert_eq!(r.buf[20 * 120 + 20], [0, 0, 255, 255]);
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

#[cfg(test)]
mod bench {
    use super::*;
    use egui::{pos2, Rect};
    #[test]
    #[ignore]
    fn frame_time() {
        let ctx = egui::Context::default();
        let size = egui::vec2(680.0, 760.0);
        let ppp = 2.0;
        let mut r = Raster::new();
        let mut on = true;
        let mut text = String::from("Ctrl+Alt+B");
        let mut total = std::time::Duration::ZERO;
        for frame in 0..20 {
            let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)), ..Default::default() };
            input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
            let out = ctx.run_ui(input, |ui| {
                ui.heading("Réglages");
                for i in 0..12 {
                    egui::Frame::new().fill(Color32::from_gray(30)).corner_radius(12).inner_margin(12).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(format!("Une option assez longue numéro {i}"));
                            ui.checkbox(&mut on, "");
                            ui.text_edit_singleline(&mut text);
                        });
                    });
                }
            });
            let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
            let mut delta = out.textures_delta;
            let free = r.set_textures(&mut delta);
            let t = std::time::Instant::now();
            r.paint((size.x * ppp) as usize, (size.y * ppp) as usize, ppp, Color32::BLACK, &prims);
            if frame >= 5 {
                total += t.elapsed();
            }
            r.free_textures(&free);
        }
        eprintln!("raster: {:?} per frame", total / 15);
    }
}
