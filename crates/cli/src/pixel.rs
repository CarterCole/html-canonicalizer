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
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&out)?;
    }
    Ok(bytes)
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
    fn round_trips_through_png() {
        let a = img(5, 2, |x, y| [x as u8 * 40, y as u8 * 90, 7, 255]);
        let png = diff_png(&a, &a, 0).unwrap();
        let back = decode(&png).unwrap();
        assert_eq!((back.w, back.h), (5, 2));
    }
}
