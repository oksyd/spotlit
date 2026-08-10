use std::path::Path;

use image::{DynamicImage, ImageReader, RgbaImage, imageops::FilterType};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

const MAX_PREVIEW_WIDTH: u32 = 1280;
const MAX_PREVIEW_HEIGHT: u32 = 720;
const MAX_THUMBNAIL_WIDTH: u32 = 176;
const MAX_THUMBNAIL_HEIGHT: u32 = 104;

#[derive(Clone)]
pub(crate) struct DecodedPreviewImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl DecodedPreviewImage {
    pub(crate) fn into_slint_image(self) -> Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(self.width, self.height);
        buffer.make_mut_bytes().copy_from_slice(&self.rgba);
        Image::from_rgba8(buffer)
    }

    pub(crate) fn thumbnail_copy(&self) -> Option<Self> {
        resize_rgba_image(
            self.width,
            self.height,
            self.rgba.clone(),
            MAX_THUMBNAIL_WIDTH,
            MAX_THUMBNAIL_HEIGHT,
            FilterType::Triangle,
        )
    }
}

pub(crate) fn decode_display_image(path: &Path) -> Option<DecodedPreviewImage> {
    decode_image(
        path,
        MAX_PREVIEW_WIDTH,
        MAX_PREVIEW_HEIGHT,
        FilterType::Triangle,
    )
}

pub(crate) fn decode_thumbnail_image(path: &Path) -> Option<DecodedPreviewImage> {
    decode_image(
        path,
        MAX_THUMBNAIL_WIDTH,
        MAX_THUMBNAIL_HEIGHT,
        FilterType::Triangle,
    )
}

fn decode_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
    filter: FilterType,
) -> Option<DecodedPreviewImage> {
    let image = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let rgba = resize_dynamic_image(image, max_width, max_height, filter).into_rgba8();
    let (width, height) = rgba.dimensions();

    Some(DecodedPreviewImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn resize_rgba_image(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    max_width: u32,
    max_height: u32,
    filter: FilterType,
) -> Option<DecodedPreviewImage> {
    let image = DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, rgba)?);
    let rgba = resize_dynamic_image(image, max_width, max_height, filter).into_rgba8();
    let (width, height) = rgba.dimensions();

    Some(DecodedPreviewImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn resize_dynamic_image(
    image: DynamicImage,
    max_width: u32,
    max_height: u32,
    filter: FilterType,
) -> DynamicImage {
    if image.width() > max_width || image.height() > max_height {
        image.resize(max_width, max_height, filter)
    } else {
        image
    }
}
