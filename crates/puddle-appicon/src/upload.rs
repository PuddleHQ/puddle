// SPDX-License-Identifier: GPL-3.0-or-later
//! Checking and resizing an image the user gives as the app icon.

use std::io::Cursor;

use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbaImage};

use crate::{Icon, SIZE};

/// The largest file accepted. A photo from a phone is a few megabytes; an icon is a few kilobytes.
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// The shortest side accepted: smaller than this would be blown up into a blur.
const MIN_SIDE: u32 = 32;
/// The longest side accepted, so a file that says it is a billion pixels wide is refused from its
/// header, before anything is allocated for it.
const MAX_SIDE: u32 = 8192;
/// The most memory decoding may ask for.
const MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// Why an image was refused as an icon. Every message is said to the user as it stands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IconError {
    /// Nothing was sent.
    #[error("the file is empty")]
    Empty,
    /// Over [`MAX_UPLOAD_BYTES`].
    #[error("the file is larger than 8 MB")]
    TooLarge,
    /// Not one of the formats Puddle reads.
    #[error("the file is not a PNG, JPEG or WebP image")]
    UnknownFormat,
    /// The format is right but the data is not (cut off, damaged, or too big to open safely).
    #[error("the image cannot be read: {0}")]
    Unreadable(String),
    /// A side is under 32 pixels.
    #[error("the image is {0} by {1} pixels; an icon needs at least 32 on each side")]
    TooSmall(u32, u32),
    /// A side is over 8192 pixels, or opening the image would take too much memory.
    #[error("the image is too big; the most is 8192 pixels on each side")]
    TooBig,
    /// Every pixel is see-through.
    #[error("the image is completely transparent")]
    Blank,
    /// A stored icon is not square (it was not made by Puddle).
    #[error("the stored icon is not square")]
    NotSquare,
    /// The PNG could not be written.
    #[error("the icon cannot be written: {0}")]
    Encode(String),
}

/// Checks `bytes` as an icon image (PNG, JPEG or WebP, at most 8 MB, 32 to 8192 pixels a side, not
/// blank) and returns it as a [`SIZE`] pixel square PNG. An image that is not square is scaled to
/// fit and centred on a transparent square, so nothing of it is cut off.
///
/// # Errors
///
/// An [`IconError`] naming what is wrong with the file.
pub fn normalize_upload(bytes: &[u8]) -> Result<Vec<u8>, IconError> {
    if bytes.is_empty() {
        return Err(IconError::Empty);
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(IconError::TooLarge);
    }
    let format = image::guess_format(bytes)
        .ok()
        .filter(|f| matches!(f, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP))
        .ok_or(IconError::UnknownFormat)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(unreadable)?;
    let (width, height) = decoder.dimensions();
    if width.min(height) < MIN_SIDE {
        return Err(IconError::TooSmall(width, height));
    }
    // The orientation a phone camera recorded, so a photo is not stored on its side.
    let orientation = decoder.orientation().map_err(unreadable)?;
    let mut picture = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    picture.apply_orientation(orientation);
    let source = picture.into_rgba8();
    if source.pixels().all(|p| p.0[3] == 0) {
        return Err(IconError::Blank);
    }
    square(&source).to_png()
}

fn unreadable(err: image::ImageError) -> IconError {
    match err {
        image::ImageError::Limits(_) => IconError::TooBig,
        other => IconError::Unreadable(other.to_string()),
    }
}

/// `source` scaled to fit [`SIZE`] by [`SIZE`] and centred on a transparent square.
fn square(source: &RgbaImage) -> Icon {
    let (w, h) = source.dimensions();
    let longest = w.max(h);
    // Rounded to the nearest pixel, never zero.
    let fit = |side: u32| {
        ((u64::from(side) * u64::from(SIZE) + u64::from(longest) / 2) / u64::from(longest)).max(1)
    };
    let (new_w, new_h) = (
        u32::try_from(fit(w)).unwrap_or(SIZE),
        u32::try_from(fit(h)).unwrap_or(SIZE),
    );
    let scaled = imageops::resize(source, new_w, new_h, FilterType::Lanczos3);
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    imageops::replace(
        &mut canvas,
        &scaled,
        i64::from((SIZE - new_w) / 2),
        i64::from((SIZE - new_h) / 2),
    );
    Icon {
        size: SIZE,
        rgba: canvas.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use image::{ExtendedColorType, Rgba};

    use super::*;

    fn encoded(width: u32, height: u32, format: ImageFormat, pixel: [u8; 4]) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba(pixel));
        let mut out = Cursor::new(Vec::new());
        match format {
            // JPEG has no alpha.
            ImageFormat::Jpeg => {
                let rgb = DynamicImage::ImageRgba8(image).into_rgb8();
                image::write_buffer_with_format(
                    &mut out,
                    rgb.as_raw(),
                    width,
                    height,
                    ExtendedColorType::Rgb8,
                    format,
                )
                .unwrap();
            }
            _ => image::write_buffer_with_format(
                &mut out,
                image.as_raw(),
                width,
                height,
                ExtendedColorType::Rgba8,
                format,
            )
            .unwrap(),
        }
        out.into_inner()
    }

    fn pixel(icon: &Icon, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * icon.size + x) * 4) as usize;
        icon.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn a_wide_png_is_fitted_and_centred_on_a_clear_square() {
        let png = encoded(400, 100, ImageFormat::Png, [10, 200, 30, 255]);
        let icon = Icon::from_png(&normalize_upload(&png).unwrap()).unwrap();
        assert_eq!(icon.size, SIZE);
        assert_eq!(
            pixel(&icon, 128, 128),
            [10, 200, 30, 255],
            "the image fills the middle"
        );
        assert_eq!(pixel(&icon, 128, 10)[3], 0, "the padding above is clear");
        assert_eq!(pixel(&icon, 128, 245)[3], 0, "and below");
        assert_eq!(
            pixel(&icon, 2, 128),
            [10, 200, 30, 255],
            "full width is used"
        );
    }

    #[test]
    fn a_tall_image_is_fitted_the_other_way() {
        let png = encoded(60, 240, ImageFormat::Png, [1, 2, 3, 255]);
        let icon = Icon::from_png(&normalize_upload(&png).unwrap()).unwrap();
        assert_eq!(pixel(&icon, 128, 2), [1, 2, 3, 255]);
        assert_eq!(pixel(&icon, 5, 128)[3], 0);
    }

    #[test]
    fn jpeg_and_webp_are_read_too() {
        for format in [ImageFormat::Jpeg, ImageFormat::WebP] {
            let file = encoded(64, 64, format, [200, 100, 50, 255]);
            let icon = Icon::from_png(&normalize_upload(&file).unwrap()).unwrap();
            let [r, g, b, a] = pixel(&icon, 128, 128);
            assert!(
                r.abs_diff(200) < 8 && g.abs_diff(100) < 8 && b.abs_diff(50) < 8,
                "{format:?}"
            );
            assert_eq!(a, 255);
        }
    }

    #[test]
    fn an_image_already_the_right_size_keeps_its_pixels() {
        let png = encoded(SIZE, SIZE, ImageFormat::Png, [9, 9, 9, 128]);
        let icon = Icon::from_png(&normalize_upload(&png).unwrap()).unwrap();
        assert_eq!(pixel(&icon, 0, 0), [9, 9, 9, 128]);
        assert_eq!(pixel(&icon, 255, 255), [9, 9, 9, 128]);
    }

    #[test]
    fn what_is_not_an_icon_image_is_refused_in_words() {
        assert_eq!(normalize_upload(&[]), Err(IconError::Empty));
        assert_eq!(
            normalize_upload(b"hello, not an image"),
            Err(IconError::UnknownFormat)
        );
        assert_eq!(
            normalize_upload(&vec![0u8; MAX_UPLOAD_BYTES + 1]),
            Err(IconError::TooLarge)
        );
        // GIF is a real image format, just not one Puddle reads.
        assert_eq!(
            normalize_upload(b"GIF89a\x20\x00\x20\x00"),
            Err(IconError::UnknownFormat)
        );
        assert_eq!(
            normalize_upload(&encoded(31, 100, ImageFormat::Png, [0, 0, 0, 255])),
            Err(IconError::TooSmall(31, 100))
        );
        assert_eq!(
            normalize_upload(&encoded(8193, 40, ImageFormat::Png, [0, 0, 0, 255])),
            Err(IconError::TooBig),
            "refused from the header, with no allocation"
        );
        assert_eq!(
            normalize_upload(&encoded(64, 64, ImageFormat::Png, [5, 5, 5, 0])),
            Err(IconError::Blank)
        );
        let mut cut = encoded(64, 64, ImageFormat::Png, [5, 5, 5, 255]);
        cut.truncate(60);
        assert!(matches!(
            normalize_upload(&cut),
            Err(IconError::Unreadable(_))
        ));
    }

    #[test]
    fn a_stored_icon_that_is_not_square_or_not_a_png_is_refused() {
        let wide = encoded(40, 20, ImageFormat::Png, [0, 0, 0, 255]);
        assert_eq!(Icon::from_png(&wide), Err(IconError::NotSquare));
        assert!(matches!(
            Icon::from_png(b"nope"),
            Err(IconError::Unreadable(_))
        ));
    }
}
