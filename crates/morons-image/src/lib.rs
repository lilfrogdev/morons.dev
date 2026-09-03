use std::io::{self, Cursor};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{
    AnimationDecoder as _, DynamicImage, ImageDecoder as _, ImageEncoder as _, ImageFormat,
    ImageReader, Limits,
    codecs::{
        gif::{GifDecoder, GifEncoder},
        jpeg::JpegEncoder,
    },
};

pub const MAX_INPUT_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NORMALIZED_IMAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 2_048;
pub const MAX_IMAGE_PIXELS: u64 = 4_194_304;
pub const MAX_GIF_FRAMES: usize = 100;
pub const MAX_GIF_AGGREGATE_PIXELS: u64 = 25_000_000;
const MAX_DECODE_ALLOCATION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BASE64_INPUT_BYTES: usize = MAX_INPUT_IMAGE_BYTES * 4 / 3 + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageMediaType {
    Png,
    Jpeg,
    Gif,
}

impl ImageMediaType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    pub media_type: ImageMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for NormalizedImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NormalizedImage")
            .field("media_type", &self.media_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    InputLimit,
    InvalidImage,
    UnsupportedFormat,
    DimensionLimit,
    OutputLimit,
    InvalidBase64,
}

pub fn normalize_image(bytes: &[u8]) -> Result<NormalizedImage, ImageError> {
    if bytes.is_empty() || bytes.len() > MAX_INPUT_IMAGE_BYTES {
        return Err(ImageError::InputLimit);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ImageError::InvalidImage)?;
    let format = reader.format().ok_or(ImageError::UnsupportedFormat)?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(ImageError::UnsupportedFormat);
    }
    if format == ImageFormat::Gif {
        return normalize_gif(bytes);
    }
    let limits = decode_limits();
    reader.limits(limits.clone());
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| ImageError::InvalidImage)?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    let orientation = decoder
        .orientation()
        .map_err(|_| ImageError::InvalidImage)?;
    decoder
        .set_limits(limits)
        .map_err(|_| ImageError::DimensionLimit)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| ImageError::InvalidImage)?;
    image.apply_orientation(orientation);
    let (width, height) = (image.width(), image.height());
    validate_dimensions(width, height)?;
    encode_static_image(&image, width, height)
}

pub fn validate_normalized_image(
    bytes: &[u8],
    media_type: ImageMediaType,
    width: u32,
    height: u32,
) -> bool {
    if bytes.is_empty() || bytes.len() > MAX_NORMALIZED_IMAGE_BYTES {
        return false;
    }
    if media_type == ImageMediaType::Gif {
        return normalize_gif(bytes).is_ok_and(|image| {
            image.width == width && image.height == height && image.bytes == bytes
        });
    }
    let mut reader = match ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
        Ok(reader) => reader,
        Err(_) => return false,
    };
    let expected_format = match media_type {
        ImageMediaType::Png => ImageFormat::Png,
        ImageMediaType::Jpeg => ImageFormat::Jpeg,
        ImageMediaType::Gif => return false,
    };
    if reader.format() != Some(expected_format) {
        return false;
    }
    reader.limits(decode_limits());
    reader.decode().is_ok_and(|image| {
        image.width() == width
            && image.height() == height
            && validate_dimensions(width, height).is_ok()
    })
}

pub fn normalize_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<NormalizedImage, ImageError> {
    validate_dimensions(width, height)?;
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(ImageError::DimensionLimit)?;
    if rgba.len() != expected {
        return Err(ImageError::InvalidImage);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba).ok_or(ImageError::InvalidImage)?;
    encode_static_image(&DynamicImage::ImageRgba8(buffer), width, height)
}

#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode_base64(value: &str) -> Result<Vec<u8>, ImageError> {
    if value.is_empty() || value.len() > MAX_BASE64_INPUT_BYTES || !value.is_ascii() {
        return Err(ImageError::InvalidBase64);
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| ImageError::InvalidBase64)?;
    if bytes.is_empty() || bytes.len() > MAX_INPUT_IMAGE_BYTES {
        return Err(ImageError::InputLimit);
    }
    Ok(bytes)
}

fn normalize_gif(bytes: &[u8]) -> Result<NormalizedImage, ImageError> {
    if bytes.len() > MAX_NORMALIZED_IMAGE_BYTES {
        return Err(ImageError::OutputLimit);
    }
    let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(|_| ImageError::InvalidImage)?;
    decoder
        .set_limits(decode_limits())
        .map_err(|_| ImageError::DimensionLimit)?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    let frame_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ImageError::DimensionLimit)?;
    let mut frames = 0_usize;
    let mut aggregate_pixels = 0_u64;
    let mut writer = LimitedWriter::default();
    let mut encoder = GifEncoder::new_with_speed(&mut writer, 10);
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|_| ImageError::InvalidImage)?;
        frames = frames.checked_add(1).ok_or(ImageError::DimensionLimit)?;
        aggregate_pixels = aggregate_pixels
            .checked_add(frame_pixels)
            .ok_or(ImageError::DimensionLimit)?;
        if frames > MAX_GIF_FRAMES || aggregate_pixels > MAX_GIF_AGGREGATE_PIXELS {
            return Err(ImageError::DimensionLimit);
        }
        if encoder.encode_frame(frame).is_err() {
            drop(encoder);
            return Err(if writer.exceeded {
                ImageError::OutputLimit
            } else {
                ImageError::InvalidImage
            });
        }
    }
    drop(encoder);
    let normalized = writer.bytes;
    if frames == 0 {
        return Err(ImageError::InvalidImage);
    }
    Ok(NormalizedImage {
        media_type: ImageMediaType::Gif,
        width,
        height,
        bytes: normalized,
    })
}

#[derive(Default)]
struct LimitedWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl io::Write for LimitedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(buffer.len())
            .is_none_or(|bytes| bytes > MAX_NORMALIZED_IMAGE_BYTES)
        {
            self.exceeded = true;
            return Err(io::Error::other("normalized image output limit reached"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_static_image(
    image: &DynamicImage,
    width: u32,
    height: u32,
) -> Result<NormalizedImage, ImageError> {
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| ImageError::InvalidImage)?;
    if png.get_ref().len() <= MAX_NORMALIZED_IMAGE_BYTES {
        return Ok(NormalizedImage {
            media_type: ImageMediaType::Png,
            width,
            height,
            bytes: png.into_inner(),
        });
    }
    let rgb = if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        if rgba.pixels().any(|pixel| pixel.0[3] != 255) {
            return Err(ImageError::OutputLimit);
        }
        DynamicImage::ImageRgba8(rgba).to_rgb8()
    } else {
        image.to_rgb8()
    };
    for quality in [85, 75, 65] {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .map_err(|_| ImageError::InvalidImage)?;
        if jpeg.len() <= MAX_NORMALIZED_IMAGE_BYTES {
            return Ok(NormalizedImage {
                media_type: ImageMediaType::Jpeg,
                width,
                height,
                bytes: jpeg,
            });
        }
    }
    Err(ImageError::OutputLimit)
}

fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOCATION_BYTES);
    limits
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|pixels| pixels > MAX_IMAGE_PIXELS)
    {
        return Err(ImageError::DimensionLimit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    use super::*;

    #[test]
    fn png_and_clipboard_rgba_normalize_with_bounded_dimensions() {
        let source = RgbaImage::from_pixel(3, 2, Rgba([12, 34, 56, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(source.clone())
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("fixture should encode");
        let normalized = normalize_image(encoded.get_ref()).expect("PNG should normalize");
        assert_eq!((normalized.width, normalized.height), (3, 2));
        assert_eq!(normalized.media_type, ImageMediaType::Png);
        assert!(normalized.bytes.len() <= MAX_NORMALIZED_IMAGE_BYTES);

        let clipboard = normalize_rgba(3, 2, source.into_raw()).expect("RGBA should normalize");
        assert_eq!((clipboard.width, clipboard.height), (3, 2));
        assert!(normalize_rgba(MAX_IMAGE_DIMENSION + 1, 1, Vec::new()).is_err());
    }

    #[test]
    fn jpeg_exif_orientation_is_applied_before_normalization() {
        let source =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 3, Rgba([120, 30, 220, 255])));
        let mut encoded = Cursor::new(Vec::new());
        source
            .write_to(&mut encoded, ImageFormat::Jpeg)
            .expect("JPEG fixture should encode");
        let mut oriented = encoded.into_inner();
        let exif = [
            0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 0x2a, 0, 8, 0, 0, 0,
            1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0,
        ];
        oriented.splice(2..2, exif);
        let normalized = normalize_image(&oriented).expect("oriented JPEG should normalize");
        assert_eq!((normalized.width, normalized.height), (3, 2));
    }

    #[test]
    fn jpeg_webp_and_gif_are_detected_from_content() {
        let source =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([120, 30, 220, 255])));
        for format in [ImageFormat::Jpeg, ImageFormat::WebP, ImageFormat::Gif] {
            let mut encoded = Cursor::new(Vec::new());
            source
                .write_to(&mut encoded, format)
                .expect("fixture should encode");
            let normalized = normalize_image(encoded.get_ref()).expect("image should normalize");
            assert_eq!((normalized.width, normalized.height), (2, 2));
            assert!(normalized.bytes.len() <= MAX_NORMALIZED_IMAGE_BYTES);
            assert!(validate_normalized_image(
                &normalized.bytes,
                normalized.media_type,
                normalized.width,
                normalized.height
            ));
            if format == ImageFormat::Gif {
                assert_eq!(normalized.media_type, ImageMediaType::Gif);
            }
        }
    }

    #[test]
    fn base64_is_strict_and_bounded() {
        let value = encode_base64(b"image bytes");
        assert_eq!(decode_base64(&value).unwrap(), b"image bytes");
        assert!(decode_base64("not base64!").is_err());
        assert!(decode_base64(&"A".repeat(MAX_BASE64_INPUT_BYTES + 1)).is_err());
    }

    #[test]
    fn unsupported_or_malformed_input_is_rejected() {
        assert_eq!(
            normalize_image(b"not an image"),
            Err(ImageError::UnsupportedFormat)
        );
        assert_eq!(normalize_image(&[]), Err(ImageError::InputLimit));
    }
}
