// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
