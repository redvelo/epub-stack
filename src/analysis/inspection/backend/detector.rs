use super::{DetectedFormat, Detection, FontFormat, RasterImageFormat};
use crate::content::extraction::svg::has_svg_root;
use crate::media_type::MediaContainer;

pub(super) fn detect(bytes: &[u8]) -> Option<Detection> {
    let format = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        DetectedFormat::Raster(RasterImageFormat::Jpeg)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        DetectedFormat::Raster(RasterImageFormat::Png)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        DetectedFormat::Raster(RasterImageFormat::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        DetectedFormat::Raster(RasterImageFormat::WebP)
    } else if is_avif(bytes) {
        DetectedFormat::Raster(RasterImageFormat::Avif)
    } else if bytes.starts_with(&[0xff, 0x0a]) || bytes.starts_with(b"\0\0\0\x0cJXL \r\n\x87\n") {
        DetectedFormat::Raster(RasterImageFormat::JpegXl)
    } else if bytes.starts_with(b"ttcf") {
        DetectedFormat::Font(FontFormat::Ttc)
    } else if bytes.starts_with(b"wOFF") {
        DetectedFormat::Font(FontFormat::Woff)
    } else if bytes.starts_with(b"wOF2") {
        DetectedFormat::Font(FontFormat::Woff2)
    } else if bytes.starts_with(b"OTTO") {
        DetectedFormat::Font(FontFormat::Otf)
    } else if bytes.starts_with(&[0, 1, 0, 0]) || bytes.starts_with(b"true") {
        DetectedFormat::Font(FontFormat::Ttf)
    } else if strip_utf8_bom(bytes).starts_with(b"WEBVTT") {
        DetectedFormat::WebVtt
    } else if bytes.starts_with(b"OggS") {
        DetectedFormat::Media(MediaContainer::Ogg)
    } else if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        DetectedFormat::Media(MediaContainer::WebM)
    } else if is_mp4(bytes) {
        DetectedFormat::Media(MediaContainer::Mp4)
    } else if is_adts(bytes) {
        DetectedFormat::Media(MediaContainer::AacAdts)
    } else if is_mp3(bytes) {
        DetectedFormat::Media(MediaContainer::Mp3)
    } else {
        return has_svg_root(bytes).then_some(Detection {
            format: DetectedFormat::Svg,
            svg: None,
        });
    };
    Some(Detection::plain(format))
}

pub(super) fn is_avif(bytes: &[u8]) -> bool {
    ftyp_brands(bytes).is_some_and(|brands| {
        brands
            .as_chunks::<4>()
            .0
            .iter()
            .any(|brand| matches!(brand, b"avif" | b"avis"))
    })
}

fn is_mp4(bytes: &[u8]) -> bool {
    ftyp_brands(bytes).is_some() && !is_avif(bytes)
}

fn ftyp_brands(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let box_size = u32::from_be_bytes(bytes[..4].try_into().ok()?) as usize;
    if box_size < 16 || box_size > bytes.len() || !(box_size - 16).is_multiple_of(4) {
        return None;
    }
    let mut brands = Vec::with_capacity(box_size - 12);
    brands.extend_from_slice(&bytes[8..12]);
    brands.extend_from_slice(&bytes[16..box_size]);
    Some(brands)
}

fn is_adts(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0
}

fn is_mp3(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0 && !is_adts(bytes))
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avif_brand_detection_stays_inside_a_well_formed_ftyp_box() {
        let bytes = b"\0\0\0\x10ftypisom\0\0\0\0junkavif";
        assert_eq!(
            detect(bytes).map(|value| value.format()),
            Some(DetectedFormat::Media(MediaContainer::Mp4))
        );

        let truncated = b"\0\0\0\x20ftypavif\0\0\0\0";
        assert!(detect(truncated).is_none());
    }

    #[test]
    fn conservative_signatures_cover_phase_seven_formats() {
        let cases: &[(&[u8], DetectedFormat)] = &[
            (
                b"\xff\xd8\xff\xe0",
                DetectedFormat::Raster(RasterImageFormat::Jpeg),
            ),
            (
                b"\x89PNG\r\n\x1a\n",
                DetectedFormat::Raster(RasterImageFormat::Png),
            ),
            (b"GIF89a", DetectedFormat::Raster(RasterImageFormat::Gif)),
            (
                b"RIFF\0\0\0\0WEBP",
                DetectedFormat::Raster(RasterImageFormat::WebP),
            ),
            (
                b"\0\0\0\x14ftypavif\0\0\0\0avif",
                DetectedFormat::Raster(RasterImageFormat::Avif),
            ),
            (
                b"\xff\x0a",
                DetectedFormat::Raster(RasterImageFormat::JpegXl),
            ),
            (b"\0\x01\0\0", DetectedFormat::Font(FontFormat::Ttf)),
            (b"OTTO", DetectedFormat::Font(FontFormat::Otf)),
            (b"ttcf", DetectedFormat::Font(FontFormat::Ttc)),
            (b"wOFF", DetectedFormat::Font(FontFormat::Woff)),
            (b"wOF2", DetectedFormat::Font(FontFormat::Woff2)),
            (b"WEBVTT\n", DetectedFormat::WebVtt),
            (
                b"ID3\x04\0\0\0\0\0\0",
                DetectedFormat::Media(MediaContainer::Mp3),
            ),
            (
                b"\xff\xf1\x50\x80\0\x1f\xfc",
                DetectedFormat::Media(MediaContainer::AacAdts),
            ),
            (b"OggS", DetectedFormat::Media(MediaContainer::Ogg)),
            (
                b"\0\0\0\x10ftypisom\0\0\0\0",
                DetectedFormat::Media(MediaContainer::Mp4),
            ),
            (
                b"\x1a\x45\xdf\xa3",
                DetectedFormat::Media(MediaContainer::WebM),
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(
                detect(bytes).map(|value| value.format()),
                Some(*expected),
                "signature {bytes:?}"
            );
        }
    }
}
