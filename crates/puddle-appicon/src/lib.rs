// SPDX-License-Identifier: GPL-3.0-or-later
//! The icon of Puddle's window, tray and taskbar button.
//!
//! The user picks one of the built-in duck icons ([`built_in`]) or gives an image of their own
//! ([`normalize_upload`]). Both end as a square [`Icon`] of [`SIZE`] pixels, so the shell sets the
//! same kind of image whichever was chosen, and the Settings screen previews what the window will
//! really show.
#![forbid(unsafe_code)]

mod builtin;
mod upload;

pub use builtin::built_in;
pub use upload::{IconError, MAX_UPLOAD_BYTES, normalize_upload};

/// The side, in pixels, of a stored icon: large enough for any taskbar or tray scale, small enough
/// to keep in the settings folder and to send to the Settings screen.
pub const SIZE: u32 = 256;

/// A square image, four bytes a pixel (red, green, blue, alpha; not premultiplied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// The side in pixels.
    pub size: u32,
    /// `size * size * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Icon {
    /// The icon as a PNG file.
    ///
    /// # Errors
    ///
    /// [`IconError::Encode`] when the PNG cannot be made, which for a well-formed icon does not
    /// happen.
    pub fn to_png(&self) -> Result<Vec<u8>, IconError> {
        let mut out = Vec::new();
        image::write_buffer_with_format(
            &mut std::io::Cursor::new(&mut out),
            &self.rgba,
            self.size,
            self.size,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|e| IconError::Encode(e.to_string()))?;
        Ok(out)
    }

    /// The icon in a PNG file made by [`Icon::to_png`] (or any square PNG).
    ///
    /// # Errors
    ///
    /// [`IconError::Unreadable`] when the bytes are not a PNG, [`IconError::NotSquare`] when it is
    /// not square.
    pub fn from_png(png: &[u8]) -> Result<Self, IconError> {
        let decoded = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map_err(|e| IconError::Unreadable(e.to_string()))?
            .into_rgba8();
        if decoded.width() != decoded.height() {
            return Err(IconError::NotSquare);
        }
        Ok(Self {
            size: decoded.width(),
            rgba: decoded.into_raw(),
        })
    }
}
