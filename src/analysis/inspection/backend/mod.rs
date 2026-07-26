use super::{FontFormat, InspectionKind, RasterImageFormat, ResourceInspection, Svg};
use crate::analysis::AnalysisIssue;
use crate::content::extraction::svg::{SvgScan, scan as scan_svg};
use crate::media_type::{MediaContainer, MediaType, MediaTypeClassification};

mod detector;
mod font;
mod media;
mod raster;
mod text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectedFormat {
    Raster(RasterImageFormat),
    Svg,
    Font(FontFormat),
    WebVtt,
    Media(MediaContainer),
}

#[derive(Debug, Clone)]
pub(crate) struct Detection {
    format: DetectedFormat,
    svg: Option<SvgScan>,
}

impl Detection {
    pub(crate) fn format(&self) -> DetectedFormat {
        self.format
    }

    pub(crate) fn svg(scan: SvgScan) -> Self {
        Self {
            format: DetectedFormat::Svg,
            svg: Some(scan),
        }
    }

    fn plain(format: DetectedFormat) -> Self {
        Self { format, svg: None }
    }
}

pub(crate) struct InspectionResult {
    pub(crate) facts: ResourceInspection,
    pub(crate) issue: Option<AnalysisIssue>,
}

pub(crate) fn inspect(
    bytes: &[u8],
    declared: Option<MediaTypeClassification>,
    detected: Option<Detection>,
    complete: bool,
) -> InspectionResult {
    let format = detected
        .as_ref()
        .map(Detection::format)
        .or_else(|| declared.and_then(declared_format));
    let detected_media_type = detected
        .as_ref()
        .and_then(|value| detected_media_type(value.format));
    let (inspection, issue) = match format {
        Some(DetectedFormat::Raster(format)) => raster::inspect(bytes, format, complete),
        Some(DetectedFormat::Svg) => inspect_svg(
            detected
                .and_then(|value| value.svg)
                .unwrap_or_else(|| scan_svg(bytes)),
            complete,
        ),
        Some(DetectedFormat::Font(format)) => font::inspect(bytes, format, complete),
        Some(DetectedFormat::WebVtt) => text::inspect_webvtt(bytes, complete),
        Some(DetectedFormat::Media(container)) => media::inspect(bytes, container, complete),
        None if matches!(declared, Some(MediaTypeClassification::GenericText)) => {
            (InspectionKind::Text(text::inspect(bytes, complete)), None)
        }
        None => (InspectionKind::Binary, None),
    };
    InspectionResult {
        facts: ResourceInspection::new(detected_media_type, inspection),
        issue,
    }
}

fn declared_format(classification: MediaTypeClassification) -> Option<DetectedFormat> {
    Some(match classification {
        MediaTypeClassification::WebVtt => DetectedFormat::WebVtt,
        MediaTypeClassification::Svg => DetectedFormat::Svg,
        MediaTypeClassification::Raster(format) => DetectedFormat::Raster(format),
        MediaTypeClassification::Font(format) => DetectedFormat::Font(format),
        MediaTypeClassification::Media(container) => DetectedFormat::Media(container),
        MediaTypeClassification::GenericText => return None,
    })
}

pub(crate) fn detect(bytes: &[u8]) -> Option<Detection> {
    detector::detect(bytes)
}

fn detected_media_type(format: DetectedFormat) -> Option<MediaType> {
    let media_type = match format {
        DetectedFormat::Raster(RasterImageFormat::Jpeg) => "image/jpeg",
        DetectedFormat::Raster(RasterImageFormat::Png) => "image/png",
        DetectedFormat::Raster(RasterImageFormat::Gif) => "image/gif",
        DetectedFormat::Raster(RasterImageFormat::WebP) => "image/webp",
        DetectedFormat::Raster(RasterImageFormat::Avif) => "image/avif",
        DetectedFormat::Raster(RasterImageFormat::JpegXl) => "image/jxl",
        DetectedFormat::Svg => "image/svg+xml",
        DetectedFormat::Font(FontFormat::Ttf) => "font/ttf",
        DetectedFormat::Font(FontFormat::Otf) => "font/otf",
        DetectedFormat::Font(FontFormat::Ttc) => "font/collection",
        DetectedFormat::Font(FontFormat::Woff) => "font/woff",
        DetectedFormat::Font(FontFormat::Woff2) => "font/woff2",
        DetectedFormat::WebVtt => "text/vtt",
        DetectedFormat::Media(MediaContainer::Mp3) => "audio/mpeg",
        DetectedFormat::Media(MediaContainer::AacAdts) => "audio/aac",
        DetectedFormat::Media(MediaContainer::Ogg) => "application/ogg",
        DetectedFormat::Media(MediaContainer::Mp4) => "application/mp4",
        DetectedFormat::Media(MediaContainer::WebM) => return None,
    };
    MediaType::new(media_type)
}

fn inspect_svg(scan: SvgScan, complete: bool) -> (InspectionKind, Option<AnalysisIssue>) {
    let issue = (scan.is_malformed() || (complete && (!scan.is_svg() || !scan.root_closed())))
        .then_some(AnalysisIssue::Malformed);
    (
        InspectionKind::SvgImage(Svg::new(
            scan.width().map(str::to_owned),
            scan.height().map(str::to_owned),
            scan.view_box().map(str::to_owned),
            scan.title().map(str::to_owned),
            scan.description().map(str::to_owned),
        )),
        issue,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_scanner_handles_namespaced_empty_roots_and_attributes() {
        for bytes in [
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20" viewBox="0 0 10 20"/>"#.as_slice(),
            br#"<s:svg xmlns:s="http://www.w3.org/2000/svg" width="10" height="20" viewBox="0 0 10 20"/>"#.as_slice(),
        ] {
            let result = inspect(bytes, None, detect(bytes), true);
            assert_eq!(result.issue, None);
            let InspectionKind::SvgImage(svg) = result.facts.kind() else {
                panic!()
            };
            assert_eq!(svg.width(), Some("10"));
            assert_eq!(svg.height(), Some("20"));
            assert_eq!(svg.view_box(), Some("0 0 10 20"));
        }
        assert!(detect(br#"<svg width="10"/>"#).is_none());
        assert!(detect(br#"<s:svg xmlns:s="urn:not-svg"/>"#).is_none());
    }

    #[test]
    fn svg_scanner_accumulates_multiline_and_nested_text() {
        let bytes = br#"<svg xmlns="http://www.w3.org/2000/svg">
  <title>First
<tspan>middle</tspan>
last</title>
  <desc>Line one
Line two</desc>
</svg>"#;
        let result = inspect(bytes, None, detect(bytes), true);
        assert_eq!(result.issue, None);
        let InspectionKind::SvgImage(svg) = result.facts.kind() else {
            panic!()
        };
        assert_eq!(svg.title(), Some("First\nmiddle\nlast"));
        assert_eq!(svg.description(), Some("Line one\nLine two"));
    }

    #[test]
    fn malformed_svg_structure_is_surfaced() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><title>broken</svg>"#;
        let result = inspect(svg, Some(MediaTypeClassification::Svg), detect(svg), true);
        assert_eq!(result.issue, Some(AnalysisIssue::Malformed));
    }
}
