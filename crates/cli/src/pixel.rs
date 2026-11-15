//! Screenshot comparison.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Cursor;

#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    /// RGBA, 8 bits per channel.
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Diff {
    pub different: u64,
    pub total: u64,
    /// Largest per-channel difference seen.
    pub max_delta: u8,
    pub size_mismatch: bool,
}

impl Diff {
    pub fn exact(&self) -> bool {
        self.different == 0 && !self.size_mismatch
    }
}

pub fn decode(png_bytes: &[u8]) -> Result<Image> {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().context("screenshot is not a readable PNG")?;
    let size = reader.output_buffer_size().ok_or_else(|| anyhow!("screenshot is too large to decode"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).context("screenshot is not a readable PNG")?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let n = (w as usize) * (h as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(n * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        other => bail!("unexpected screenshot colour type {other:?}"),
    };
    if rgba.len() != n * 4 {
        bail!("screenshot has an unexpected size");
    }
    Ok(Image { w, h, rgba })
}

/// The part of `img` inside the given rectangle (clamped to the image).
pub fn crop(img: &Image, x: u32, y: u32, w: u32, h: u32) -> Image {
    let x = x.min(img.w);
    let y = y.min(img.h);
    let w = w.min(img.w - x);
    let h = h.min(img.h - y);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = ((row * img.w + x) * 4) as usize;
        rgba.extend_from_slice(&img.rgba[start..start + (w * 4) as usize]);
    }
    Image { w, h, rgba }
}

/// Count pixels where any channel differs by more than `tolerance`.
pub fn diff(a: &Image, b: &Image, tolerance: u8) -> Diff {
    let (w, h) = (a.w.min(b.w) as usize, a.h.min(b.h) as usize);
    let mut d = Diff { total: (a.w.max(b.w) as u64) * (a.h.max(b.h) as u64), size_mismatch: a.w != b.w || a.h != b.h, ..Default::default() };
    for y in 0..h {
        let ra = &a.rgba[y * a.w as usize * 4..][..w * 4];
        let rb = &b.rgba[y * b.w as usize * 4..][..w * 4];
        if ra == rb {
            continue;
        }
        for (pa, pb) in ra.chunks_exact(4).zip(rb.chunks_exact(4)) {
            let delta = (0..4).map(|i| pa[i].abs_diff(pb[i])).max().unwrap_or(0);
            if delta > tolerance {
                d.different += 1;
            }
            d.max_delta = d.max_delta.max(delta);
        }
    }
    d.different += d.total - (w as u64) * (h as u64);
    d
}

pub fn encode(img: &Image) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, img.w, img.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&img.rgba)?;
    }
    Ok(bytes)
}

/// The original dimmed, with every differing pixel marked red.
pub fn diff_png(a: &Image, b: &Image, tolerance: u8) -> Result<Vec<u8>> {
    encode(&diff_image(a, b, tolerance))
}

pub fn diff_image(a: &Image, b: &Image, tolerance: u8) -> Image {
    let (w, h) = (a.w.max(b.w) as usize, a.h.max(b.h) as usize);
    let mut out = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let pa = (x < a.w as usize && y < a.h as usize).then(|| &a.rgba[(y * a.w as usize + x) * 4..][..4]);
            let pb = (x < b.w as usize && y < b.h as usize).then(|| &b.rgba[(y * b.w as usize + x) * 4..][..4]);
            let o = &mut out[(y * w + x) * 4..][..4];
            let same = match (pa, pb) {
                (Some(p), Some(q)) => (0..4).all(|i| p[i].abs_diff(q[i]) <= tolerance),
                _ => false,
            };
            if same {
                let p = pa.unwrap();
                for i in 0..3 {
                    o[i] = 255 - (255 - p[i]) / 4;
                }
            } else {
                o.copy_from_slice(&[230, 0, 40, 255]);
            }
        }
    }
    Image { w: w as u32, h: h as u32, rgba: out }
}

/// 5x7 bitmap glyphs, one row per entry, high bit on the left.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 12, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '%' => [24, 25, 2, 4, 8, 19, 3],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        _ => [0; 7],
    }
}

/// Draw `text` with its top-left at (x, y), each font pixel `scale` px square.
fn draw_text(img: &mut Image, text: &str, x: u32, y: u32, scale: u32, rgb: [u8; 3]) {
    let mut cx = x;
    for ch in text.chars() {
        let g = glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5u32 {
                if bits & (16 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (px, py) = (cx + col * scale + dx, y + row as u32 * scale + dy);
                        if px < img.w && py < img.h {
                            let o = ((py * img.w + px) * 4) as usize;
                            img.rgba[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                        }
                    }
                }
            }
        }
        cx += 6 * scale;
    }
}

/// Panels side by side on a light grey sheet, each with a caption above it.
pub fn side_by_side(panels: &[(&Image, &str)]) -> Image {
    let scale = 2;
    let (gap, head) = (16u32, 7 * scale + 16);
    let w = gap + panels.iter().map(|(i, _)| i.w + gap).sum::<u32>();
    let h = head + panels.iter().map(|(i, _)| i.h).max().unwrap_or(0) + gap;
    let mut out = Image { w, h, rgba: [238u8, 238, 238, 255].repeat((w * h) as usize) };
    let mut x = gap;
    for (img, caption) in panels {
        draw_text(&mut out, caption, x, 8, scale, [40, 40, 40]);
        for row in 0..img.h {
            let src = &img.rgba[(row * img.w * 4) as usize..((row + 1) * img.w * 4) as usize];
            let o = (((head + row) * w + x) * 4) as usize;
            out.rgba[o..o + src.len()].copy_from_slice(src);
        }
        x += img.w + gap;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
            }
        }
        Image { w, h, rgba }
    }

    #[test]
    fn identical_images_are_exact() {
        let a = img(4, 3, |x, y| [x as u8, y as u8, 0, 255]);
        let b = img(4, 3, |x, y| [x as u8, y as u8, 0, 255]);
        assert!(diff(&a, &b, 0).exact());
    }

    #[test]
    fn counts_differences_and_respects_tolerance() {
        let a = img(4, 3, |_, _| [10, 10, 10, 255]);
        let b = img(4, 3, |x, _| if x == 0 { [13, 10, 10, 255] } else { [10, 10, 10, 255] });
        let d = diff(&a, &b, 0);
        assert_eq!((d.different, d.total, d.max_delta), (3, 12, 3));
        assert_eq!(diff(&a, &b, 3).different, 0);
    }

    #[test]
    fn size_mismatch_is_never_exact() {
        let a = img(4, 3, |_, _| [0, 0, 0, 255]);
        let b = img(4, 4, |_, _| [0, 0, 0, 255]);
        let d = diff(&a, &b, 0);
        assert!(d.size_mismatch && !d.exact());
        assert_eq!(d.different, 4);
    }

    #[test]
    fn crops() {
        let a = img(4, 3, |x, y| [x as u8, y as u8, 0, 255]);
        let c = crop(&a, 1, 1, 2, 5);
        assert_eq!((c.w, c.h), (2, 2));
        assert_eq!(&c.rgba[..4], &[1, 1, 0, 255]);
        assert_eq!(&c.rgba[12..16], &[2, 2, 0, 255]);
    }

    #[test]
    fn side_by_side_places_panels_and_captions() {
        let a = img(10, 6, |_, _| [255, 0, 0, 255]);
        let b = img(8, 9, |_, _| [0, 0, 255, 255]);
        let s = side_by_side(&[(&a, "A"), (&b, "B")]);
        assert_eq!((s.w, s.h), (16 + 10 + 16 + 8 + 16, 30 + 9 + 16));
        let at = |x: u32, y: u32| s.rgba[((y * s.w + x) * 4) as usize..][..3].to_vec();
        assert_eq!(at(16, 30), vec![255, 0, 0]);
        assert_eq!(at(42, 38), vec![0, 0, 255]);
        assert_eq!(at(16 + 2 * 2, 8), vec![40, 40, 40]); // top bar of the "A"
    }

    #[test]
    fn round_trips_through_png() {
        let a = img(5, 2, |x, y| [x as u8 * 40, y as u8 * 90, 7, 255]);
        let png = diff_png(&a, &a, 0).unwrap();
        let back = decode(&png).unwrap();
        assert_eq!((back.w, back.h), (5, 2));
    }
}
