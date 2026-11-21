//! Tray icon rendering (platform independent, produces straight RGBA).
//!
//! The artwork is the original 512×512 layer set; only the alpha channel is
//! used, so one set of images serves both the light and the dark taskbar.
//! The gauge is drawn procedurally: continuous fill level, colour coded.

use crate::model::DeviceKind;
use std::sync::OnceLock;

const SRC: usize = 512;

/// Inside of the battery outline, in source pixels (x0, y0, x1, y1).
const GAUGE: (usize, usize, usize, usize) = (306, 138, 452, 412);

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct IconSpec {
    pub kind: DeviceKind,
    /// `None` = unknown (question mark).
    pub percent: Option<f32>,
    pub charging: bool,
    /// Light foreground for a dark taskbar.
    pub light_foreground: bool,
    /// Device offline: drawn semi-transparent.
    pub dimmed: bool,
}

struct Layers {
    battery: Vec<u8>,
    mouse: Vec<u8>,
    keyboard: Vec<u8>,
    headset: Vec<u8>,
    missing: Vec<u8>,
    charging: Vec<u8>,
}

fn decode_alpha(data: &[u8]) -> Vec<u8> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().expect("embedded png");
    let mut buf = vec![0; reader.output_buffer_size().expect("embedded png")];
    let info = reader.next_frame(&mut buf).expect("embedded png");
    assert_eq!((info.width as usize, info.height as usize), (SRC, SRC));
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let channels = info.color_type.samples();
    buf[..info.buffer_size()].chunks_exact(channels).map(|px| px[channels - 1]).collect()
}

fn layers() -> &'static Layers {
    static LAYERS: OnceLock<Layers> = OnceLock::new();
    LAYERS.get_or_init(|| Layers {
        battery: decode_alpha(include_bytes!("../assets/battery.png")),
        mouse: decode_alpha(include_bytes!("../assets/mouse.png")),
        keyboard: decode_alpha(include_bytes!("../assets/keyboard.png")),
        headset: decode_alpha(include_bytes!("../assets/headset.png")),
        missing: decode_alpha(include_bytes!("../assets/missing.png")),
        charging: decode_alpha(include_bytes!("../assets/charging.png")),
    })
}

/// Gauge / text colour for a battery level.
pub fn level_color(percent: f32) -> [u8; 3] {
    if percent <= 15.0 {
        [0xE5, 0x48, 0x4D]
    } else if percent <= 30.0 {
        [0xF5, 0xA5, 0x24]
    } else {
        [0x3D, 0xC4, 0x6B]
    }
}

pub fn foreground(light: bool) -> [u8; 3] {
    if light {
        [0xF2, 0xF2, 0xF2]
    } else {
        [0x16, 0x16, 0x16]
    }
}

/// Renders a `size`×`size` icon, rows top to bottom, straight alpha RGBA.
pub fn render(spec: &IconSpec, size: usize) -> Vec<u8> {
    let size = size.clamp(8, SRC);
    let l = layers();
    let fg = foreground(spec.light_foreground);
    let device = match spec.kind {
        DeviceKind::Keyboard => &l.keyboard,
        DeviceKind::Headset => &l.headset,
        DeviceKind::Mouse | DeviceKind::Other => &l.mouse,
    };
    let overlay = match spec.percent {
        None => Some(&l.missing),
        Some(_) if spec.charging => Some(&l.charging),
        Some(_) => None,
    };
    let fill = spec.percent.map(|p| {
        let p = p.clamp(0.0, 100.0);
        let height = (GAUGE.3 - GAUGE.1) as f32;
        // Keep a sliver visible even when nearly empty.
        let top = GAUGE.3 as f32 - (height * p / 100.0).max(height * 0.06);
        (top.round() as usize, level_color(p))
    });

    // Accumulate premultiplied colour into the destination bins (box filter).
    let mut acc = vec![[0f32; 4]; size * size];
    let mut count = vec![0u32; size * size];
    for y in 0..SRC {
        let dy = y * size / SRC;
        for x in 0..SRC {
            let i = y * SRC + x;
            let bin = dy * size + x * size / SRC;
            count[bin] += 1;

            // Source-over compositing of the layers for this pixel.
            let mut px = [0f32; 4]; // premultiplied
            if let Some((top, color)) = fill {
                if x >= GAUGE.0 && x < GAUGE.2 && y >= top && y < GAUGE.3 {
                    px = [color[0] as f32, color[1] as f32, color[2] as f32, 1.0];
                }
            }
            let mut ink = (l.battery[i] as f32 / 255.0).max(device[i] as f32 / 255.0);
            if let Some(o) = overlay {
                ink = ink.max(o[i] as f32 / 255.0);
            }
            if ink > 0.0 {
                for c in 0..3 {
                    px[c] = fg[c] as f32 * ink + px[c] * (1.0 - ink);
                }
                px[3] = ink + px[3] * (1.0 - ink);
            }
            if px[3] > 0.0 {
                let a = &mut acc[bin];
                for c in 0..4 {
                    a[c] += px[c];
                }
            }
        }
    }

    let opacity = if spec.dimmed { 0.45 } else { 1.0 };
    let mut out = vec![0u8; size * size * 4];
    for (bin, a) in acc.iter().enumerate() {
        if a[3] <= 0.0 {
            continue;
        }
        let n = count[bin] as f32;
        let o = &mut out[bin * 4..bin * 4 + 4];
        for c in 0..3 {
            o[c] = (a[c] / a[3]).round().clamp(0.0, 255.0) as u8;
        }
        o[3] = (a[3] / n * 255.0 * opacity).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// PNG encoding, used by the tests to produce previews.
#[cfg(test)]
pub fn to_png(rgba: &[u8], size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, size as u32, size as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(percent: Option<f32>) -> IconSpec {
        IconSpec { kind: DeviceKind::Mouse, percent, charging: false, light_foreground: true, dimmed: false }
    }

    fn pixel(img: &[u8], size: usize, sx: usize, sy: usize) -> [u8; 4] {
        let i = (sy * size / SRC * size + sx * size / SRC) * 4;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn gauge_follows_level() {
        let size = 32;
        let full = render(&spec(Some(100.0)), size);
        assert_eq!(full.len(), size * size * 4);
        // Centre top and bottom of the gauge are green when full.
        assert_eq!(pixel(&full, size, 380, 170), [0x3D, 0xC4, 0x6B, 255]);
        assert_eq!(pixel(&full, size, 380, 390), [0x3D, 0xC4, 0x6B, 255]);

        let half = render(&spec(Some(50.0)), size);
        assert_eq!(pixel(&half, size, 380, 170)[3], 0, "upper half is empty");
        assert_eq!(pixel(&half, size, 380, 390), [0x3D, 0xC4, 0x6B, 255]);

        let low = render(&spec(Some(10.0)), size);
        assert_eq!(&pixel(&low, size, 380, 400)[..3], &[0xE5, 0x48, 0x4D]);

        let unknown = render(&spec(None), size);
        assert_eq!(pixel(&unknown, size, 380, 400)[3], 0);
        // The device glyph (left) and battery outline (right) are drawn.
        assert!(pixel(&full, size, 20, 256)[3] > 100);
        assert!(pixel(&full, size, 272, 256)[3] > 100);
        // Fully transparent corner.
        assert_eq!(pixel(&full, size, 500, 10)[3], 0);
    }

    #[test]
    fn variants_differ() {
        let base = render(&spec(Some(60.0)), 32);
        let mut s = spec(Some(60.0));
        s.charging = true;
        assert_ne!(render(&s, 32), base);
        let mut s = spec(Some(60.0));
        s.kind = DeviceKind::Headset;
        assert_ne!(render(&s, 32), base);
        let mut s = spec(Some(60.0));
        s.dimmed = true;
        let dim = render(&s, 32);
        assert!(dim.chunks(4).all(|p| p[3] <= 115));
        for size in [16, 20, 24, 40, 64] {
            assert_eq!(render(&spec(Some(60.0)), size).len(), size * size * 4);
        }
    }

    /// `PREVIEW_DIR=/some/dir cargo test previews` writes sample icons.
    #[test]
    fn previews() {
        let Ok(dir) = std::env::var("PREVIEW_DIR") else { return };
        let sheet = 64;
        let cases = [
            (DeviceKind::Mouse, Some(100.0), false, true, false),
            (DeviceKind::Mouse, Some(55.0), false, true, false),
            (DeviceKind::Keyboard, Some(25.0), false, true, false),
            (DeviceKind::Headset, Some(8.0), false, true, false),
            (DeviceKind::Mouse, Some(70.0), true, true, false),
            (DeviceKind::Mouse, None, false, true, false),
            (DeviceKind::Keyboard, Some(80.0), false, false, false),
            (DeviceKind::Headset, Some(40.0), true, false, true),
        ];
        let mut img = vec![0u8; sheet * cases.len() * sheet * 4];
        for (n, &(kind, percent, charging, light, dimmed)) in cases.iter().enumerate() {
            let bg = if light { [32, 32, 32] } else { [235, 235, 235] };
            let icon = render(&IconSpec { kind, percent, charging, light_foreground: light, dimmed }, sheet);
            for y in 0..sheet {
                for x in 0..sheet {
                    let s = &icon[(y * sheet + x) * 4..][..4];
                    let d = &mut img[(y * sheet * cases.len() + n * sheet + x) * 4..][..4];
                    let a = s[3] as f32 / 255.0;
                    for c in 0..3 {
                        d[c] = (s[c] as f32 * a + bg[c] as f32 * (1.0 - a)) as u8;
                    }
                    d[3] = 255;
                }
            }
        }
        // Non-square sheet: encode manually.
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, (sheet * cases.len()) as u32, sheet as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&img).unwrap();
        std::fs::write(format!("{dir}/icons.png"), out).unwrap();
        let _ = to_png(&render(&spec(Some(50.0)), 16), 16);
    }
}
