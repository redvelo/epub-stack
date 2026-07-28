//! Media type declarations for inspecting manifest resources without losing malformed input.
//!
//! [`MediaType`] trims surrounding whitespace and retains the remaining non-empty text.
//! Malformed MIME syntax remains available through [`MediaType::raw`] and is reported by
//! [`MediaType::is_valid`] instead of preventing the publication from loading.

use crate::analysis::inspection::{FontFormat, RasterImageFormat};
use crate::string::EpubString;
use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaTypeClassification {
    GenericText,
    WebVtt,
    Svg,
    Raster(RasterImageFormat),
    Font(FontFormat),
    Media(MediaContainer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaContainer {
    Mp3,
    AacAdts,
    Ogg,
    Mp4,
    WebM,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// A manifest media type that supports both source inspection and MIME queries.
///
/// The stored text has surrounding whitespace removed but otherwise preserves spelling and
/// parameters. MIME-based queries return `None` or `false` for malformed declarations. Equality
/// and hashing use MIME semantics when both values are valid; otherwise they compare stored text.
/// Direct comparison with `str` always compares the stored text.
pub struct MediaType {
    raw: EpubString,
    #[cfg_attr(feature = "specta", specta(skip))]
    parsed: Option<mime::Mime>,
}

#[cfg(feature = "serde")]
impl serde::Serialize for MediaType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.raw())
    }
}

impl MediaType {
    /// Creates a media type for a manifest declaration or application-supplied resource.
    ///
    /// Surrounding whitespace is trimmed. Returns `None` when nothing remains; malformed MIME
    /// syntax is retained and can be detected with [`Self::is_valid`].
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let raw = EpubString::new(value)?;
        Some(Self::from_epub_string(raw))
    }

    pub(crate) fn from_epub_string(raw: EpubString) -> Self {
        let parsed = raw.as_str().parse().ok();
        Self { raw, parsed }
    }

    /// Returns the stored declaration, with surrounding whitespace removed at construction.
    pub fn raw(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the stored declaration, with surrounding whitespace removed at construction.
    ///
    /// This is an alias for [`Self::raw`], not a normalized MIME serialization.
    pub fn as_str(&self) -> &str {
        self.raw()
    }

    /// Reports whether the authored value parses as MIME syntax.
    pub fn is_valid(&self) -> bool {
        self.parsed.is_some()
    }

    /// Returns the normalized MIME essence, excluding parameters, when parsing succeeded.
    pub fn essence(&self) -> Option<&str> {
        self.parsed.as_ref().map(mime::Mime::essence_str)
    }

    /// Reports whether the parsed essence is `application/xhtml+xml`.
    pub fn is_xhtml(&self) -> bool {
        self.has_essence("application/xhtml+xml")
    }

    /// Reports whether the parsed essence is `image/svg+xml`.
    pub fn is_svg(&self) -> bool {
        self.has_essence("image/svg+xml")
    }

    /// Reports whether the parsed essence is `text/css`.
    pub fn is_css(&self) -> bool {
        self.has_essence("text/css")
    }

    /// Reports whether the parsed essence is `application/smil+xml`.
    pub fn is_smil(&self) -> bool {
        self.has_essence("application/smil+xml")
    }

    /// Reports whether the parsed essence is `application/x-dtbncx+xml`.
    pub fn is_ncx(&self) -> bool {
        self.has_essence("application/x-dtbncx+xml")
    }

    /// Reports whether a valid MIME value has the requested top-level type.
    ///
    /// Returns `false` for malformed MIME declarations.
    pub fn has_top_level_type(&self, type_: mime::Name<'static>) -> bool {
        self.parsed
            .as_ref()
            .is_some_and(|media_type| media_type.type_() == type_)
    }

    pub(crate) fn classification(&self) -> Option<MediaTypeClassification> {
        use FontFormat as Font;
        use MediaContainer as Media;
        use MediaTypeClassification as Classification;
        use RasterImageFormat as Raster;

        let essence = self.essence()?;
        let classification = match essence {
            "text/vtt" => Classification::WebVtt,
            "image/svg+xml" => Classification::Svg,
            "image/jpeg" | "image/jpg" | "image/pjpeg" => Classification::Raster(Raster::Jpeg),
            "image/png" | "image/x-png" => Classification::Raster(Raster::Png),
            "image/gif" => Classification::Raster(Raster::Gif),
            "image/webp" => Classification::Raster(Raster::WebP),
            "image/avif" => Classification::Raster(Raster::Avif),
            "image/jxl" => Classification::Raster(Raster::JpegXl),
            "font/ttf" | "application/x-font-ttf" => Classification::Font(Font::Ttf),
            "font/otf" | "application/x-font-opentype" | "application/vnd.ms-opentype" => {
                Classification::Font(Font::Otf)
            }
            "font/collection" | "application/x-font-ttc" => Classification::Font(Font::Ttc),
            "font/woff" | "application/font-woff" | "application/x-font-woff" => {
                Classification::Font(Font::Woff)
            }
            "font/woff2" | "application/font-woff2" | "application/x-font-woff2" => {
                Classification::Font(Font::Woff2)
            }
            "audio/mpeg" | "audio/mp3" | "audio/x-mp3" => Classification::Media(Media::Mp3),
            "audio/aac" | "audio/aacp" | "audio/x-aac" => Classification::Media(Media::AacAdts),
            "application/ogg" | "audio/ogg" | "video/ogg" => Classification::Media(Media::Ogg),
            "application/mp4" | "audio/mp4" | "video/mp4" => Classification::Media(Media::Mp4),
            "audio/webm" | "video/webm" => Classification::Media(Media::WebM),
            "application/xml"
            | "application/javascript"
            | "application/ecmascript"
            | "application/json" => Classification::GenericText,
            _ if essence.ends_with("+xml") || self.has_top_level_type(mime::TEXT) => {
                Classification::GenericText
            }
            _ => return None,
        };
        Some(classification)
    }

    fn has_essence(&self, expected: &str) -> bool {
        self.essence()
            .is_some_and(|essence| essence.eq_ignore_ascii_case(expected))
    }
}

impl PartialEq for MediaType {
    fn eq(&self, other: &Self) -> bool {
        match (self.parsed.as_ref(), other.parsed.as_ref()) {
            (Some(left), Some(right)) => left == right,
            _ => self.raw == other.raw,
        }
    }
}

impl Eq for MediaType {}

impl Hash for MediaType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(parsed) = &self.parsed {
            parsed.hash(state);
        } else {
            self.raw.hash(state);
        }
    }
}

impl AsRef<str> for MediaType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for MediaType {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Borrow<str> for MediaType {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<EpubString> for MediaType {
    fn from(value: EpubString) -> Self {
        Self::from_epub_string(value)
    }
}

impl TryFrom<&str> for MediaType {
    type Error = crate::string::EpubStringEmpty;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(Self::from_epub_string(EpubString::try_new(value)?))
    }
}

impl TryFrom<String> for MediaType {
    type Error = crate::string::EpubStringEmpty;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Ok(Self::from_epub_string(EpubString::try_new(value)?))
    }
}

impl PartialEq<str> for MediaType {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for MediaType {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<MediaType> for str {
    fn eq(&self, other: &MediaType) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<MediaType> for &str {
    fn eq(&self, other: &MediaType) -> bool {
        *self == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_preserves_raw_and_exposes_parsed_facts() {
        let media_type = MediaType::new("APPLICATION/XHTML+XML; charset=utf-8").unwrap();

        assert_eq!(media_type.raw(), "APPLICATION/XHTML+XML; charset=utf-8");
        assert!(media_type.is_valid());
        assert_eq!(media_type.essence(), Some("application/xhtml+xml"));
        assert!(media_type.is_xhtml());
    }

    #[test]
    fn media_type_preserves_invalid_raw_value() {
        let media_type = MediaType::new("text/plain/html").unwrap();

        assert_eq!(media_type.raw(), "text/plain/html");
        assert!(!media_type.is_valid());
        assert_eq!(media_type.essence(), None);
    }

    #[test]
    fn media_type_equality_uses_parsed_semantics_when_valid() {
        assert_eq!(
            MediaType::new("TEXT/PLAIN; charset=utf-8").unwrap(),
            MediaType::new("text/plain; charset=UTF-8").unwrap()
        );
        assert_ne!(
            MediaType::new("bad/type/extra").unwrap(),
            MediaType::new("BAD/TYPE/EXTRA").unwrap()
        );
    }

    #[test]
    fn media_type_classification_routes_epub_types_and_aliases() {
        use FontFormat as Font;
        use MediaContainer as Media;
        use MediaTypeClassification as Classification;
        use RasterImageFormat as Raster;

        let cases = [
            ("text/plain", Classification::GenericText),
            ("text/css; charset=utf-8", Classification::GenericText),
            ("text/vtt", Classification::WebVtt),
            ("image/svg+xml", Classification::Svg),
            ("image/jpeg", Classification::Raster(Raster::Jpeg)),
            ("image/jpg", Classification::Raster(Raster::Jpeg)),
            ("image/pjpeg", Classification::Raster(Raster::Jpeg)),
            ("image/png", Classification::Raster(Raster::Png)),
            ("image/x-png", Classification::Raster(Raster::Png)),
            ("image/gif", Classification::Raster(Raster::Gif)),
            ("image/webp", Classification::Raster(Raster::WebP)),
            ("image/avif", Classification::Raster(Raster::Avif)),
            ("image/jxl", Classification::Raster(Raster::JpegXl)),
            ("font/ttf", Classification::Font(Font::Ttf)),
            ("application/x-font-ttf", Classification::Font(Font::Ttf)),
            ("font/otf", Classification::Font(Font::Otf)),
            (
                "application/x-font-opentype",
                Classification::Font(Font::Otf),
            ),
            (
                "application/vnd.ms-opentype",
                Classification::Font(Font::Otf),
            ),
            ("font/collection", Classification::Font(Font::Ttc)),
            ("application/x-font-ttc", Classification::Font(Font::Ttc)),
            ("font/woff", Classification::Font(Font::Woff)),
            ("application/font-woff", Classification::Font(Font::Woff)),
            ("application/x-font-woff", Classification::Font(Font::Woff)),
            ("font/woff2", Classification::Font(Font::Woff2)),
            ("application/font-woff2", Classification::Font(Font::Woff2)),
            (
                "application/x-font-woff2",
                Classification::Font(Font::Woff2),
            ),
            ("audio/mpeg", Classification::Media(Media::Mp3)),
            ("audio/mp3", Classification::Media(Media::Mp3)),
            ("audio/x-mp3", Classification::Media(Media::Mp3)),
            ("audio/aac", Classification::Media(Media::AacAdts)),
            ("audio/aacp", Classification::Media(Media::AacAdts)),
            ("audio/x-aac", Classification::Media(Media::AacAdts)),
            ("application/ogg", Classification::Media(Media::Ogg)),
            ("audio/ogg", Classification::Media(Media::Ogg)),
            ("video/ogg", Classification::Media(Media::Ogg)),
            ("application/mp4", Classification::Media(Media::Mp4)),
            ("audio/mp4", Classification::Media(Media::Mp4)),
            ("video/mp4", Classification::Media(Media::Mp4)),
            ("audio/webm", Classification::Media(Media::WebM)),
            ("video/webm", Classification::Media(Media::WebM)),
        ];

        for (value, expected) in cases {
            assert_eq!(
                MediaType::new(value).unwrap().classification(),
                Some(expected),
                "classification for {value}"
            );
        }
    }

    #[test]
    fn media_type_classification_does_not_guess_unknown_or_invalid_types() {
        for value in [
            "application/octet-stream",
            "image/bmp",
            "font/fake",
            "text/plain/html",
        ] {
            assert_eq!(
                MediaType::new(value).unwrap().classification(),
                None,
                "classification for {value}"
            );
        }
    }
}
