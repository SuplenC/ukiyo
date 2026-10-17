//! A small overlay for recordings: a caption saying what's happening and a
//! panel of the signals driving the scene, so a viewer can tell which
//! reaction is which.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{Rgba, RgbaImage};

pub struct Hud {
    font: FontRef<'static>,
    mono: FontRef<'static>,
}

pub struct Meter<'a> {
    pub label: &'a str,
    pub value: f32,
    pub text: Option<String>,
}

impl Hud {
    pub fn new() -> Hud {
        Hud {
            font: FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT)
                .expect("bundled font"),
            mono: FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR)
                .expect("bundled font"),
        }
    }

    /// Caption bottom left, fading in over 0.4 s.
    pub fn caption(&self, img: &mut RgbaImage, text: &str, age: f64) {
        let s = img.height() as f32 / 1080.0;
        let alpha = (age / 0.4).clamp(0.0, 1.0) as f32;
        let size = 34.0 * s;
        let w = self.width(&self.font, text, size);
        let (x, y) = (56.0 * s, img.height() as f32 - 72.0 * s);
        fill(
            img,
            x - 20.0 * s,
            y - size * 0.95,
            w + 40.0 * s,
            size * 1.45,
            [8, 8, 12],
            0.55 * alpha,
        );
        fill(
            img,
            x - 20.0 * s,
            y - size * 0.95,
            4.0 * s,
            size * 1.45,
            [255, 64, 72],
            0.9 * alpha,
        );
        self.text(img, &self.font, text, x, y, size, [240, 240, 245], alpha);
    }

    /// Signal panel, top right.
    pub fn meters(&self, img: &mut RgbaImage, meters: &[Meter]) {
        let s = img.height() as f32 / 1080.0;
        let row = 30.0 * s;
        let w = 300.0 * s;
        let x0 = img.width() as f32 - w - 40.0 * s;
        let y0 = 40.0 * s;
        fill(
            img,
            x0,
            y0,
            w,
            row * meters.len() as f32 + 20.0 * s,
            [8, 8, 12],
            0.5,
        );
        for (i, m) in meters.iter().enumerate() {
            let y = y0 + 10.0 * s + row * i as f32;
            let size = 17.0 * s;
            self.text(
                img,
                &self.mono,
                m.label,
                x0 + 14.0 * s,
                y + row * 0.68,
                size,
                [200, 200, 210],
                0.95,
            );
            let bx = x0 + 120.0 * s;
            let bw = w - 134.0 * s;
            match &m.text {
                Some(t) => self.text(
                    img,
                    &self.mono,
                    t,
                    bx,
                    y + row * 0.68,
                    size,
                    [240, 240, 245],
                    1.0,
                ),
                None => {
                    fill(img, bx, y + row * 0.38, bw, 6.0 * s, [255, 255, 255], 0.15);
                    fill(
                        img,
                        bx,
                        y + row * 0.38,
                        bw * m.value.clamp(0.0, 1.0),
                        6.0 * s,
                        [255, 80, 90],
                        0.95,
                    );
                }
            }
        }
    }

    fn width(&self, font: &FontRef, text: &str, size: f32) -> f32 {
        let f = font.as_scaled(PxScale::from(size));
        text.chars().map(|c| f.h_advance(f.glyph_id(c))).sum()
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        img: &mut RgbaImage,
        font: &FontRef,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        rgb: [u8; 3],
        alpha: f32,
    ) {
        let scale = PxScale::from(size);
        let f = font.as_scaled(scale);
        let mut cx = x;
        for c in text.chars() {
            let id = f.glyph_id(c);
            let g = id.with_scale_and_position(scale, point(cx, y));
            cx += f.h_advance(id);
            if let Some(o) = font.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    blend(img, px, py, rgb, cov * alpha);
                });
            }
        }
    }
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, rgb: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 || a <= 0.0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let a = a.min(1.0);
    for i in 0..3 {
        p[i] = (f32::from(p[i]) * (1.0 - a) + f32::from(rgb[i]) * a).round() as u8;
    }
    *p = Rgba([p[0], p[1], p[2], 255]);
}

fn fill(img: &mut RgbaImage, x: f32, y: f32, w: f32, h: f32, rgb: [u8; 3], a: f32) {
    let (x0, y0) = (x.max(0.0) as i32, y.max(0.0) as i32);
    let (x1, y1) = ((x + w) as i32, (y + h) as i32);
    for py in y0..y1 {
        for px in x0..x1 {
            blend(img, px, py, rgb, a);
        }
    }
}
