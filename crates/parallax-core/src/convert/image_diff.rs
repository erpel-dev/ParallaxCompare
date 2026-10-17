//! Pixel comparison of raster images; SVG is passed through for rendering.

use std::io::Cursor;

use image::{ImageFormat, RgbaImage};
use rayon::prelude::*;
use serde::Serialize;

use crate::{Error, Result};

/// Mask row, number of differing pixels and the x range they span.
type RowDiff = (Vec<u8>, u64, Option<(u32, u32)>);

/// Images above this many pixels are shown without a difference mask.
const MAX_MASK_PIXELS: u64 = 120_000_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSide {
    pub mime: String,
    /// Bytes the web view can display directly (the original file, or a PNG re-encoding).
    #[serde(skip)]
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageDiff {
    pub left: Option<ImageSide>,
    pub right: Option<ImageSide>,
    /// PNG of the same size as the images: differing pixels opaque, the rest transparent.
    #[serde(skip)]
    pub mask: Option<Vec<u8>>,
    pub different_pixels: u64,
    pub total_pixels: u64,
    /// x, y, width, height of the area that contains all differences.
    pub bounds: Option<[u32; 4]>,
    pub note: Option<String>,
}

fn browser_mime(format: ImageFormat) -> Option<&'static str> {
    Some(match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Ico => "image/x-icon",
        _ => return None,
    })
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    img.write_with_encoder(encoder).map_err(|e| Error::Unsupported(format!("PNG: {e}")))?;
    Ok(out.into_inner())
}

fn load(data: &[u8]) -> Result<(ImageSide, RgbaImage)> {
    let format = image::guess_format(data).map_err(|e| Error::Unsupported(format!("image: {e}")))?;
    let img =
        image::load_from_memory_with_format(data, format).map_err(|e| Error::Unsupported(format!("image: {e}")))?;
    let rgba = img.to_rgba8();
    let (mime, bytes) = match browser_mime(format) {
        Some(mime) => (mime.to_string(), data.to_vec()),
        None => ("image/png".to_string(), encode_png(&rgba)?),
    };
    let side = ImageSide { mime, bytes, width: rgba.width(), height: rgba.height(), format: format!("{format:?}") };
    Ok((side, rgba))
}

fn svg_side(data: &[u8]) -> ImageSide {
    ImageSide { mime: "image/svg+xml".into(), bytes: data.to_vec(), width: 0, height: 0, format: "SVG".into() }
}

pub fn image_diff(name: &str, left: Option<&[u8]>, right: Option<&[u8]>) -> Result<ImageDiff> {
    if name.to_ascii_lowercase().ends_with(".svg") {
        return Ok(ImageDiff {
            left: left.map(svg_side),
            right: right.map(svg_side),
            note: Some("SVG is shown rendered; pixels are not compared".into()),
            ..Default::default()
        });
    }
    let left = left.map(load).transpose()?;
    let right = right.map(load).transpose()?;
    let mut diff = ImageDiff::default();
    if let (Some((_, a)), Some((_, b))) = (&left, &right) {
        if a.dimensions() != b.dimensions() {
            diff.note = Some(format!("Sizes differ: {}×{} vs {}×{}", a.width(), a.height(), b.width(), b.height()));
        } else if (a.width() as u64 * a.height() as u64) > MAX_MASK_PIXELS {
            diff.note = Some("The image is too large for a pixel comparison".into());
        } else {
            let (w, h) = a.dimensions();
            diff.total_pixels = w as u64 * h as u64;
            // Rows are compared in parallel; each yields its mask row and differing span.
            let rows: Vec<RowDiff> = (0..h)
                .into_par_iter()
                .map(|y| {
                    let start = (y * w * 4) as usize;
                    let (ra, rb) =
                        (&a.as_raw()[start..start + w as usize * 4], &b.as_raw()[start..start + w as usize * 4]);
                    let mut mask = vec![0u8; w as usize * 4];
                    let (mut count, mut span) = (0u64, None::<(u32, u32)>);
                    for x in 0..w as usize {
                        if ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4] {
                            mask[x * 4..x * 4 + 4].copy_from_slice(&[255, 0, 90, 255]);
                            count += 1;
                            let x = x as u32;
                            span = Some(span.map_or((x, x), |(lo, hi)| (lo.min(x), hi.max(x))));
                        }
                    }
                    (mask, count, span)
                })
                .collect();
            let mut bounds: Option<[u32; 4]> = None;
            let mut mask = Vec::with_capacity(diff.total_pixels as usize * 4);
            for (y, (row, count, span)) in rows.into_iter().enumerate() {
                mask.extend_from_slice(&row);
                diff.different_pixels += count;
                if let Some((lo, hi)) = span {
                    let y = y as u32;
                    bounds = Some(match bounds {
                        None => [lo, y, hi, y],
                        Some([x0, y0, x1, _]) => [x0.min(lo), y0, x1.max(hi), y],
                    });
                }
            }
            diff.bounds = bounds.map(|[x0, y0, x1, y1]| [x0, y0, x1 - x0 + 1, y1 - y0 + 1]);
            if diff.different_pixels > 0 {
                let img = RgbaImage::from_raw(w, h, mask).expect("mask has the image size");
                diff.mask = Some(encode_png(&img)?);
            }
        }
    }
    diff.left = left.map(|(s, _)| s);
    diff.right = right.map(|(s, _)| s);
    Ok(diff)
}

/// Whether two encoded images have identical pixels.
pub fn pixels_equal(left: &[u8], right: &[u8]) -> Option<bool> {
    let a = image::load_from_memory(left).ok()?.to_rgba8();
    let b = image::load_from_memory(right).ok()?.to_rgba8();
    Some(a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw())
}
