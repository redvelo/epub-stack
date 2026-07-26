use super::{AnalysisIssue, InspectionKind, RasterImageFormat};
use crate::analysis::inspection::RasterImage;

pub(super) fn inspect(
    bytes: &[u8],
    format: RasterImageFormat,
    complete: bool,
) -> (InspectionKind, Option<AnalysisIssue>) {
    let parsed = match format {
        RasterImageFormat::Jpeg => inspect_jpeg(bytes, complete),
        RasterImageFormat::Png => inspect_png(bytes, complete),
        RasterImageFormat::Gif => inspect_gif(bytes, complete),
        RasterImageFormat::WebP => inspect_webp(bytes, complete),
        RasterImageFormat::Avif => inspect_avif(bytes, complete),
        RasterImageFormat::JpegXl => inspect_jpeg_xl(bytes, complete),
    };
    let issue = (parsed.malformed || (complete && !parsed.header_complete))
        .then_some(AnalysisIssue::Malformed);
    (
        InspectionKind::RasterImage(RasterImage::new(
            format,
            parsed.width,
            parsed.height,
            parsed.color_type,
            parsed.bit_depth,
            parsed.has_alpha,
            parsed.animated,
            parsed.has_icc_profile,
        )),
        issue,
    )
}

#[derive(Default)]
struct RasterParts {
    width: Option<u32>,
    height: Option<u32>,
    color_type: Option<String>,
    bit_depth: Option<u8>,
    has_alpha: Option<bool>,
    animated: Option<bool>,
    has_icc_profile: Option<bool>,
    header_complete: bool,
    malformed: bool,
}

fn inspect_jpeg(bytes: &[u8], complete: bool) -> RasterParts {
    let mut result = RasterParts {
        has_alpha: Some(false),
        animated: Some(false),
        has_icc_profile: complete.then_some(false),
        ..Default::default()
    };
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return result;
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            result.header_complete = result.width.is_some();
            break;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd7) {
            continue;
        }
        if offset + 2 > bytes.len() {
            break;
        }
        let length = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if length < 2 || offset + length > bytes.len() {
            result.malformed = complete;
            break;
        }
        let data = &bytes[offset + 2..offset + length];
        if marker == 0xe2 && data.starts_with(b"ICC_PROFILE\0") {
            result.has_icc_profile = Some(true);
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf)
            && data.len() >= 6
        {
            result.bit_depth = Some(data[0]);
            result.height = Some(u16::from_be_bytes([data[1], data[2]]) as u32);
            result.width = Some(u16::from_be_bytes([data[3], data[4]]) as u32);
            result.color_type = Some(
                match data[5] {
                    1 => "grayscale",
                    3 => "YCbCr",
                    4 => "CMYK",
                    _ => "unknown",
                }
                .to_string(),
            );
            result.header_complete = true;
        }
        offset += length;
    }
    result
}

fn inspect_png(bytes: &[u8], complete: bool) -> RasterParts {
    let mut result = RasterParts::default();
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return result;
    }
    if bytes.len() < 33 {
        return result;
    }
    let ihdr_length = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    if ihdr_length != 13 || &bytes[12..16] != b"IHDR" {
        result.malformed = true;
        return result;
    }
    let color = bytes[25];
    let bit_depth = bytes[24];
    let valid_color_depth = match color {
        0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        2 | 4 | 6 => matches!(bit_depth, 8 | 16),
        3 => matches!(bit_depth, 1 | 2 | 4 | 8),
        _ => false,
    };
    if !valid_color_depth || bytes[26] != 0 || bytes[27] != 0 || !matches!(bytes[28], 0 | 1) {
        result.malformed = true;
        return result;
    }
    result.width = Some(u32::from_be_bytes(bytes[16..20].try_into().unwrap()));
    result.height = Some(u32::from_be_bytes(bytes[20..24].try_into().unwrap()));
    if result.width == Some(0) || result.height == Some(0) {
        result.malformed = true;
        return result;
    }
    result.bit_depth = Some(bit_depth);
    result.color_type = Some(
        match color {
            0 => "grayscale",
            2 => "truecolor",
            3 => "indexed",
            4 => "grayscale-alpha",
            6 => "truecolor-alpha",
            _ => "unknown",
        }
        .to_string(),
    );
    result.has_alpha = matches!(color, 4 | 6)
        .then_some(true)
        .or_else(|| complete.then_some(false));
    result.animated = complete.then_some(false);
    result.has_icc_profile = complete.then_some(false);
    result.header_complete = true;
    let mut offset = 8usize;
    let mut terminated = false;
    while offset.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let Some(end) = offset
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
        else {
            result.malformed = complete;
            break;
        };
        if end > bytes.len() {
            result.malformed = complete;
            break;
        }
        match &bytes[offset + 4..offset + 8] {
            b"tRNS" => result.has_alpha = Some(true),
            b"acTL" => result.animated = Some(true),
            b"iCCP" => result.has_icc_profile = Some(true),
            b"IEND" if length == 0 => {
                terminated = true;
                break;
            }
            b"IEND" => result.malformed = true,
            _ => {}
        }
        offset = end;
    }
    if complete && !terminated {
        result.malformed = true;
    }
    result
}

fn inspect_gif(bytes: &[u8], complete: bool) -> RasterParts {
    if bytes.len() < 13 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return RasterParts::default();
    }
    let mut result = RasterParts {
        width: Some(u16::from_le_bytes([bytes[6], bytes[7]]) as u32),
        height: Some(u16::from_le_bytes([bytes[8], bytes[9]]) as u32),
        color_type: Some("indexed".to_string()),
        bit_depth: Some((bytes[10] & 0x07) + 1),
        has_alpha: None,
        animated: None,
        has_icc_profile: None,
        header_complete: true,
        malformed: false,
    };
    let global_table = if bytes[10] & 0x80 != 0 {
        3usize << ((bytes[10] & 0x07) + 1)
    } else {
        0
    };
    let mut offset = 13usize.saturating_add(global_table);
    let mut images = 0u8;
    let mut terminated = false;
    while offset < bytes.len() {
        match bytes[offset] {
            0x2c => {
                if offset + 10 > bytes.len() {
                    break;
                }
                images = images.saturating_add(1);
                if images >= 2 {
                    result.animated = Some(true);
                }
                let packed = bytes[offset + 9];
                offset += 10;
                if packed & 0x80 != 0 {
                    offset = offset.saturating_add(3usize << ((packed & 0x07) + 1));
                }
                if offset >= bytes.len() {
                    break;
                }
                offset += 1;
                if !skip_gif_sub_blocks(bytes, &mut offset) {
                    break;
                }
            }
            0x21 => {
                if offset + 2 >= bytes.len() {
                    break;
                }
                let label = bytes[offset + 1];
                offset += 2;
                if label == 0xf9 {
                    if offset + 6 > bytes.len() || bytes[offset] != 4 || bytes[offset + 5] != 0 {
                        result.malformed = complete;
                        break;
                    }
                    if bytes[offset + 1] & 1 != 0 {
                        result.has_alpha = Some(true);
                    }
                    offset += 6;
                } else {
                    if label == 0xff
                        && offset + 12 <= bytes.len()
                        && bytes[offset] == 11
                        && &bytes[offset + 1..offset + 12] == b"ICCRGBG1012"
                    {
                        result.has_icc_profile = Some(true);
                    }
                    if !skip_gif_sub_blocks(bytes, &mut offset) {
                        break;
                    }
                }
            }
            0x3b => {
                terminated = true;
                break;
            }
            _ => {
                result.malformed = complete;
                break;
            }
        }
    }
    if complete && terminated && !result.malformed {
        result.animated.get_or_insert(false);
        result.has_alpha.get_or_insert(false);
        result.has_icc_profile.get_or_insert(false);
    } else if complete && !terminated {
        result.malformed = true;
    }
    result
}

fn skip_gif_sub_blocks(bytes: &[u8], offset: &mut usize) -> bool {
    loop {
        let Some(&length) = bytes.get(*offset) else {
            return false;
        };
        *offset += 1;
        if length == 0 {
            return true;
        }
        *offset = offset.saturating_add(length as usize);
        if *offset > bytes.len() {
            return false;
        }
    }
}

fn inspect_webp(bytes: &[u8], complete: bool) -> RasterParts {
    let mut result = RasterParts::default();
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return result;
    }
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let Some(riff_end) = riff_size.checked_add(8) else {
        result.malformed = complete;
        return result;
    };
    let chunk_size = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let Some(chunk_end) = 20usize
        .checked_add(chunk_size)
        .and_then(|end| end.checked_add(chunk_size & 1))
    else {
        result.malformed = complete;
        return result;
    };
    if complete && (riff_end != bytes.len() || chunk_end > riff_end) {
        result.malformed = true;
    }
    let chunk = &bytes[12..16];
    let data = &bytes[20..bytes.len().min(chunk_end)];
    match chunk {
        b"VP8X" if data.len() >= 10 => {
            result.width = Some(1 + u32::from_le_bytes([data[4], data[5], data[6], 0]));
            result.height = Some(1 + u32::from_le_bytes([data[7], data[8], data[9], 0]));
            result.has_icc_profile = Some(data[0] & 0x20 != 0);
            result.has_alpha = Some(data[0] & 0x10 != 0);
            result.animated = Some(data[0] & 0x02 != 0);
            result.header_complete = true;
        }
        b"VP8L" if data.len() >= 5 && data[0] == 0x2f => {
            let bits = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            result.width = Some((bits & 0x3fff) + 1);
            result.height = Some(((bits >> 14) & 0x3fff) + 1);
            result.has_alpha = Some(true);
            result.animated = Some(false);
            result.has_icc_profile = Some(false);
            result.header_complete = true;
        }
        b"VP8 " if data.len() >= 10 && data[3..6] == [0x9d, 0x01, 0x2a] => {
            result.width = Some(u16::from_le_bytes([data[6], data[7]]) as u32 & 0x3fff);
            result.height = Some(u16::from_le_bytes([data[8], data[9]]) as u32 & 0x3fff);
            result.has_alpha = Some(false);
            result.animated = Some(false);
            result.has_icc_profile = Some(false);
            result.header_complete = true;
        }
        _ => {}
    }
    result
}

fn inspect_avif(bytes: &[u8], complete: bool) -> RasterParts {
    let mut result = RasterParts::default();
    if !super::detector::is_avif(bytes) {
        return result;
    }
    let mut offset = 0usize;
    let mut boxes = 0usize;
    let mut malformed = false;
    while offset < bytes.len() {
        let Some(header_end) = offset.checked_add(8) else {
            malformed = true;
            break;
        };
        if header_end > bytes.len() {
            break;
        }
        let size32 = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let (header_len, size) = if size32 == 1 {
            let Some(extended_end) = offset.checked_add(16) else {
                malformed = true;
                break;
            };
            if extended_end > bytes.len() {
                break;
            }
            let size = u64::from_be_bytes(bytes[offset + 8..extended_end].try_into().unwrap());
            (16usize, usize::try_from(size).ok())
        } else if size32 == 0 {
            (8usize, Some(bytes.len() - offset))
        } else {
            (8usize, Some(size32 as usize))
        };
        let Some(size) = size else {
            malformed = true;
            break;
        };
        let Some(end) = offset.checked_add(size) else {
            malformed = true;
            break;
        };
        if size < header_len || end > bytes.len() {
            break;
        }
        boxes += 1;
        offset = end;
    }
    result.header_complete = boxes >= 2 && offset == bytes.len();
    result.malformed = malformed || (complete && !result.header_complete);
    result
}

fn inspect_jpeg_xl(bytes: &[u8], _complete: bool) -> RasterParts {
    let mut uninitialized = jxl_oxide::JxlImage::builder().build_uninit();
    if uninitialized.feed_bytes(bytes).is_err() {
        return RasterParts::default();
    }
    let Ok(jxl_oxide::InitializeResult::Initialized(image)) = uninitialized.try_init() else {
        return RasterParts::default();
    };
    let pixel_format = format!("{:?}", image.pixel_format()).to_ascii_lowercase();
    let bit_depth = u8::try_from(image.image_header().metadata.bit_depth.bits_per_sample()).ok();
    RasterParts {
        width: Some(image.width()),
        height: Some(image.height()),
        color_type: Some(pixel_format.clone()),
        bit_depth,
        has_alpha: Some(pixel_format.contains('a')),
        animated: Some(image.image_header().metadata.animation.is_some()),
        has_icc_profile: Some(image.original_icc().is_some()),
        header_complete: true,
        malformed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_inspects_png_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&200u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png.extend_from_slice(b"\0\0\0\0IEND\0\0\0\0");
        let (kind, issue) = inspect(&png, RasterImageFormat::Png, true);
        assert_eq!(issue, None);
        let InspectionKind::RasterImage(image) = kind else {
            panic!()
        };
        assert_eq!((image.width(), image.height()), (Some(320), Some(200)));

        let result = super::super::inspect(&png, None, super::super::detect(&png), true);
        assert_eq!(
            result.facts.detected_media_type().unwrap().essence(),
            Some("image/png")
        );
    }

    #[test]
    fn malformed_png_structures_are_surfaced() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0cIHDR".to_vec();
        png.resize(33, 0);
        let (kind, issue) = inspect(&png, RasterImageFormat::Png, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));
        let InspectionKind::RasterImage(image) = kind else {
            panic!()
        };
        assert_eq!((image.width(), image.height()), (None, None));

        let mut huge_chunk = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        huge_chunk.extend_from_slice(&1u32.to_be_bytes());
        huge_chunk.extend_from_slice(&1u32.to_be_bytes());
        huge_chunk.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        huge_chunk.extend_from_slice(&u32::MAX.to_be_bytes());
        huge_chunk.extend_from_slice(b"data");
        let (_, issue) = inspect(&huge_chunk, RasterImageFormat::Png, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));
    }

    #[test]
    fn malformed_webp_and_avif_wrappers_are_partial() {
        let webp = b"RIFF\xff\xff\xff\xffWEBPVP8X\xff\xff\xff\xff";
        let (_, issue) = inspect(webp, RasterImageFormat::WebP, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));

        let avif = b"\0\0\0\x14ftypavif\0\0\0\0avif";
        let (_, issue) = inspect(avif, RasterImageFormat::Avif, true);
        assert_eq!(issue, Some(AnalysisIssue::Malformed));
    }

    #[test]
    fn gif_animation_scan_ignores_commas_inside_sub_blocks() {
        let mut gif = b"GIF89a\x01\0\x01\0\0\0\0".to_vec();
        gif.extend_from_slice(b"\x21\xfe\x01\x2c\0");
        gif.extend_from_slice(b"\x2c\0\0\0\0\x01\0\x01\0\0\x02\x01\0\0\x3b");
        let (kind, issue) = inspect(&gif, RasterImageFormat::Gif, true);
        assert_eq!(issue, None);
        let InspectionKind::RasterImage(image) = kind else {
            panic!()
        };
        assert_eq!(image.animated(), Some(false));
    }

    #[test]
    fn jpeg_xl_initialization_extracts_header_without_rendering() {
        let bytes = [
            0xff, 0x0a, 0x30, 0x54, 0x10, 0x09, 0x08, 0x06, 0x01, 0x00, 0x78, 0x00, 0x4b, 0x38,
            0x41, 0x3c, 0xb6, 0x3a, 0x51, 0xfe, 0x00, 0x47, 0x1e, 0xa0, 0x85, 0xb8, 0x27, 0x1a,
            0x48, 0x45, 0x84, 0x1b, 0x71, 0x4f, 0xa8, 0x3e, 0x8e, 0x30, 0x03, 0x92, 0x84, 0x01,
        ];
        let (kind, issue) = inspect(&bytes, RasterImageFormat::JpegXl, true);
        let InspectionKind::RasterImage(image) = kind else {
            panic!()
        };
        assert_eq!(image.format(), RasterImageFormat::JpegXl);
        assert!(image.width().is_some());
        assert!(issue.is_none());
    }
}
