//! Resource inspection results for images, media, fonts, tracks, and text.
//!
//! [`InspectionData`] contains image dimensions, audio/video tracks, font names, WebVTT cue
//! counts, and text encodings decoded from bytes. Check [`super::AnalysisOutcome`] for availability.
//! Bytes that do not conform to their detected format produce a partial result with
//! [`super::AnalysisIssue::Malformed`].
//!
//! Read image formats and audio/video container types from resource bytes.
//!
//! ```
//! use epub_stack::{EpubZip, analysis::inspection::InspectionData};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//! let analysis = book.analyze();
//!
//! for resource in analysis.analyzed_resources() {
//!     if let Some(inspection) = resource.inspection().value() {
//!         let address = resource.resource().address();
//!
//!         match inspection.data() {
//!             InspectionData::RasterImage(image) => {
//!                 println!("{}: {:?}", address.display_value(), image.format());
//!             }
//!             InspectionData::Media(media) => {
//!                 println!("{}: {:?}", address.display_value(), media.container());
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Image dimensions are available through `width()` and `height()`; `media.tracks()` exposes
//! track details such as codecs, sample rates, and channel counts when available.

mod backend;

pub(crate) use backend::{DetectedFormat, Detection, detect, inspect};

pub use crate::media_type::MediaContainer;
use crate::media_type::MediaType;

/// A detected media type and the image, media, font, or text details decoded from bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceInspection {
    detected_media_type: Option<MediaType>,
    data: InspectionData,
}

impl ResourceInspection {
    /// Returns the media type detected from bytes, independent of package declarations.
    pub fn detected_media_type(&self) -> Option<&MediaType> {
        self.detected_media_type.as_ref()
    }

    /// Returns decoded details selected from the detected format, or from the declared media type
    /// when byte detection produced no format.
    pub fn data(&self) -> &InspectionData {
        &self.data
    }

    #[allow(dead_code)]
    pub(crate) fn new(detected_media_type: Option<MediaType>, data: InspectionData) -> Self {
        Self {
            detected_media_type,
            data,
        }
    }
}

/// The supported byte-level inspection result for a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectionData {
    /// Metadata decoded from a supported raster image encoding.
    RasterImage(RasterImage),
    /// Metadata recovered from an SVG root element.
    Svg(SvgImage),
    /// Container and track metadata from audio or video bytes.
    Media(Media),
    /// Naming and shape metadata from a supported font encoding.
    Font(Font),
    /// Bounded syntax facts from WebVTT text.
    WebVtt(WebVtt),
    /// A detected generic Unicode text encoding.
    Text(Text),
    /// Bytes with no more specific supported inspection.
    Binary,
}

/// Dimensions and encoding metadata recovered from a raster image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterImage {
    format: RasterImageFormat,
    width: Option<u32>,
    height: Option<u32>,
    color_type: Option<RasterColorModel>,
    bit_depth: Option<u8>,
    has_alpha: Option<bool>,
    animated: Option<bool>,
    has_icc_profile: Option<bool>,
}

impl RasterImage {
    /// Returns the detected raster encoding.
    pub fn format(&self) -> RasterImageFormat {
        self.format
    }

    /// Returns the intrinsic pixel width when available.
    pub fn width(&self) -> Option<u32> {
        self.width
    }

    /// Returns the intrinsic pixel height when available.
    pub fn height(&self) -> Option<u32> {
        self.height
    }

    /// Returns the encoded color model when available.
    pub fn color_model(&self) -> Option<RasterColorModel> {
        self.color_type
    }

    /// Returns bits per sample or channel when available.
    pub fn bit_depth(&self) -> Option<u8> {
        self.bit_depth
    }

    /// Returns whether an alpha channel is reported when known.
    pub fn has_alpha(&self) -> Option<bool> {
        self.has_alpha
    }

    /// Returns whether multiple animation frames were detected when known.
    pub fn animated(&self) -> Option<bool> {
        self.animated
    }

    /// Returns whether an embedded ICC profile was detected when known.
    pub fn has_icc_profile(&self) -> Option<bool> {
        self.has_icc_profile
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    pub(crate) fn new(
        format: RasterImageFormat,
        width: Option<u32>,
        height: Option<u32>,
        color_type: Option<RasterColorModel>,
        bit_depth: Option<u8>,
        has_alpha: Option<bool>,
        animated: Option<bool>,
        has_icc_profile: Option<bool>,
    ) -> Self {
        Self {
            format,
            width,
            height,
            color_type,
            bit_depth,
            has_alpha,
            animated,
            has_icc_profile,
        }
    }
}

/// The color model declared by a raster image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RasterColorModel {
    /// Single-channel grayscale.
    Grayscale,
    /// Grayscale with alpha.
    GrayscaleAlpha,
    /// Red, green, and blue channels.
    Rgb,
    /// Red, green, and blue channels with alpha.
    Rgba,
    /// Palette indices.
    Indexed,
    /// Luma and chroma channels.
    YCbCr,
    /// Cyan, magenta, yellow, and key channels.
    Cmyk,
    /// Cyan, magenta, yellow, and key channels with alpha.
    CmykAlpha,
}

/// A recognized raster image encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RasterImageFormat {
    /// JPEG.
    Jpeg,
    /// Portable Network Graphics.
    Png,
    /// Graphics Interchange Format.
    Gif,
    /// WebP.
    WebP,
    /// AV1 Image File Format.
    Avif,
    /// JPEG XL.
    JpegXl,
}

/// Intrinsic size and accessible naming recovered from an SVG root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgImage {
    width: Option<String>,
    height: Option<String>,
    view_box: Option<String>,
    title: Option<String>,
    description: Option<String>,
}

impl SvgImage {
    /// Returns the authored root `width` value.
    pub fn width(&self) -> Option<&str> {
        self.width.as_deref()
    }

    /// Returns the authored root `height` value.
    pub fn height(&self) -> Option<&str> {
        self.height.as_deref()
    }

    /// Returns the authored root `viewBox` value.
    pub fn view_box(&self) -> Option<&str> {
        self.view_box.as_deref()
    }

    /// Returns extracted root title text.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Returns extracted root description text.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[allow(dead_code)]
    pub(crate) fn new(
        width: Option<String>,
        height: Option<String>,
        view_box: Option<String>,
        title: Option<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            width,
            height,
            view_box,
            title,
            description,
        }
    }
}

/// Container-level media metadata and its inspected tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    container: MediaContainer,
    duration: Option<MediaDuration>,
    tracks: Vec<MediaTrack>,
}

impl Media {
    /// Returns the detected container format.
    pub fn container(&self) -> MediaContainer {
        self.container
    }

    /// Returns container-level duration when available.
    pub fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    /// Returns tracks in container order.
    pub fn tracks(&self) -> &[MediaTrack] {
        &self.tracks
    }

    #[allow(dead_code)]
    pub(crate) fn new(
        container: MediaContainer,
        duration: Option<MediaDuration>,
        tracks: Vec<MediaTrack>,
    ) -> Self {
        Self {
            container,
            duration,
            tracks,
        }
    }
}

/// Metadata recovered for one media-container track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTrack {
    role: MediaTrackRole,
    codec: Option<String>,
    language: Option<String>,
    duration: Option<MediaDuration>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    width: Option<u32>,
    height: Option<u32>,
}

impl MediaTrack {
    /// Returns the track's broad media role.
    pub fn role(&self) -> MediaTrackRole {
        self.role
    }

    /// Returns the container-reported codec name when available.
    pub fn codec(&self) -> Option<&str> {
        self.codec.as_deref()
    }

    /// Returns the container-reported language when available.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Returns track duration when available.
    pub fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    /// Returns audio samples per second when available.
    pub fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    /// Returns the audio channel count when available.
    pub fn channels(&self) -> Option<u16> {
        self.channels
    }

    /// Returns video pixel width when available.
    pub fn width(&self) -> Option<u32> {
        self.width
    }

    /// Returns video pixel height when available.
    pub fn height(&self) -> Option<u32> {
        self.height
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    pub(crate) fn new(
        role: MediaTrackRole,
        codec: Option<String>,
        language: Option<String>,
        duration: Option<MediaDuration>,
        sample_rate: Option<u32>,
        channels: Option<u16>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            role,
            codec,
            language,
            duration,
            sample_rate,
            channels,
            width,
            height,
        }
    }
}

/// The role of a track in its media container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaTrackRole {
    /// An audio track.
    Audio,
    /// A video track.
    Video,
    /// A subtitle or timed-text track.
    Subtitle,
    /// A track whose role was not recognized.
    Unknown,
}

/// A media duration represented as whole milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaDuration(u64);

impl MediaDuration {
    /// Returns the whole-millisecond duration.
    pub fn milliseconds(self) -> u64 {
        self.0
    }

    #[allow(dead_code)]
    pub(crate) fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }
}

/// Format and naming metadata recovered from a font resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Font {
    format: FontFormat,
    family_names: Vec<String>,
    subfamily_names: Vec<String>,
    glyph_count: Option<u32>,
    collection_count: Option<u32>,
    variable: Option<bool>,
}

impl Font {
    /// Returns the detected font encoding or container.
    pub fn format(&self) -> FontFormat {
        self.format
    }

    /// Returns deduplicated family names recovered from naming records.
    pub fn family_names(&self) -> &[String] {
        &self.family_names
    }

    /// Returns deduplicated subfamily names recovered from naming records.
    pub fn subfamily_names(&self) -> &[String] {
        &self.subfamily_names
    }

    /// Returns the glyph count when available.
    pub fn glyph_count(&self) -> Option<u32> {
        self.glyph_count
    }

    /// Returns the number of fonts in a collection, when applicable.
    pub fn collection_count(&self) -> Option<u32> {
        self.collection_count
    }

    /// Returns whether variation axes were detected when known.
    pub fn variable(&self) -> Option<bool> {
        self.variable
    }

    #[allow(dead_code)]
    pub(crate) fn new(
        format: FontFormat,
        family_names: Vec<String>,
        subfamily_names: Vec<String>,
        glyph_count: Option<u32>,
        collection_count: Option<u32>,
        variable: Option<bool>,
    ) -> Self {
        Self {
            format,
            family_names,
            subfamily_names,
            glyph_count,
            collection_count,
            variable,
        }
    }
}

/// A recognized font container or encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontFormat {
    /// A TrueType font.
    Ttf,
    /// An OpenType font.
    Otf,
    /// A TrueType Collection.
    Ttc,
    /// Web Open Font Format 1.
    Woff,
    /// Web Open Font Format 2.
    Woff2,
}

/// Cue-count facts for a completely read WebVTT resource.
///
/// Invalid WebVTT syntax is reported as a partial result with
/// [`AnalysisIssue::Malformed`](crate::analysis::AnalysisIssue::Malformed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebVtt {
    cue_count: u64,
}

impl WebVtt {
    /// Returns the number of parsed cues.
    pub fn cue_count(&self) -> u64 {
        self.cue_count
    }

    pub(crate) fn new(cue_count: u64) -> Self {
        Self { cue_count }
    }
}

/// Encoding facts recovered from a generic text resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    encoding: Option<TextEncoding>,
    has_byte_order_mark: bool,
}

impl Text {
    /// Returns the detected Unicode encoding.
    pub fn encoding(&self) -> Option<TextEncoding> {
        self.encoding
    }

    /// Returns whether the bytes began with a recognized byte-order mark.
    pub fn has_byte_order_mark(&self) -> bool {
        self.has_byte_order_mark
    }

    #[allow(dead_code)]
    pub(crate) fn new(encoding: Option<TextEncoding>, has_byte_order_mark: bool) -> Self {
        Self {
            encoding,
            has_byte_order_mark,
        }
    }
}

/// A Unicode encoding recognized by text inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextEncoding {
    /// UTF-8.
    Utf8,
    /// Little-endian UTF-16.
    Utf16Le,
    /// Big-endian UTF-16.
    Utf16Be,
}
