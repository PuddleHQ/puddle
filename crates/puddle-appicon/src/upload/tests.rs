// SPDX-License-Identifier: AGPL-3.0-or-later
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
