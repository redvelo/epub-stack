use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart};
use std::borrow::Cow;
use std::io::{BufRead, Cursor, Read};

/// A failure to reconcile XML byte encoding with its declaration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlDecodeError {
    /// The XML declaration names an encoding unsupported by the decoder.
    #[error("unsupported XML encoding declaration: {encoding}")]
    UnsupportedEncoding {
        /// The unsupported declared encoding name.
        encoding: String,
    },
    /// The declaration conflicts with the byte-order encoding detected from the source.
    #[error("XML encoding declaration {declared} conflicts with detected {detected} encoding")]
    ConflictingEncoding {
        /// The encoding named by the XML declaration.
        declared: String,
        /// The encoding detected from the source byte order.
        detected: &'static str,
    },
    /// The source contains an invalid byte sequence for its selected encoding.
    #[error("invalid byte sequence for XML encoding {encoding}")]
    InvalidBytes {
        /// The encoding used while decoding the invalid bytes.
        encoding: &'static str,
    },
}

#[derive(Clone, Copy)]
enum DetectedEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl DetectedEncoding {
    fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
        }
    }

    fn encoding(self) -> &'static encoding_rs::Encoding {
        match self {
            Self::Utf8 => encoding_rs::UTF_8,
            Self::Utf16Le => encoding_rs::UTF_16LE,
            Self::Utf16Be => encoding_rs::UTF_16BE,
        }
    }
}

pub(crate) fn decode_xml(bytes: &[u8]) -> Result<Cow<'_, str>, XmlDecodeError> {
    if bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff]) || bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00])
    {
        return Err(XmlDecodeError::UnsupportedEncoding {
            encoding: "UTF-32".to_string(),
        });
    }

    let (detected, content) = if let Some(content) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (Some(DetectedEncoding::Utf8), content)
    } else if let Some(content) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (Some(DetectedEncoding::Utf16Le), content)
    } else if let Some(content) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (Some(DetectedEncoding::Utf16Be), content)
    } else if bytes.starts_with(&[0x3c, 0x00, 0x3f, 0x00]) {
        (Some(DetectedEncoding::Utf16Le), bytes)
    } else if bytes.starts_with(&[0x00, 0x3c, 0x00, 0x3f]) {
        (Some(DetectedEncoding::Utf16Be), bytes)
    } else {
        (None, bytes)
    };

    if let Some(detected) = detected {
        let decoded = decode_strict(content, detected.encoding(), detected.name())?;
        if let Some(declared) = declaration_encoding(decoded.as_ref()) {
            ensure_declaration_matches(&declared, detected)?;
        }
        return Ok(decoded);
    }

    let declared = ascii_declaration_encoding(content);
    let encoding = match declared.as_deref() {
        None => encoding_rs::UTF_8,
        Some(label) if is_utf16_label(label) => {
            return Err(XmlDecodeError::UnsupportedEncoding {
                encoding: label.to_string(),
            });
        }
        Some(label) => encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            XmlDecodeError::UnsupportedEncoding {
                encoding: label.to_string(),
            }
        })?,
    };
    decode_strict(content, encoding, encoding.name())
}

type DecodedXmlStream<R> =
    quick_xml::encoding::DecodingReader<std::io::BufReader<std::io::Chain<Cursor<Vec<u8>>, R>>>;

pub(crate) struct XmlUtf8Reader<R: BufRead> {
    input: Option<R>,
    decoder: Option<DecodedXmlStream<R>>,
    prefix: Cursor<Vec<u8>>,
}

impl<R: BufRead> XmlUtf8Reader<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input: Some(input),
            decoder: None,
            prefix: Cursor::new(Vec::new()),
        }
    }

    fn initialize(&mut self) -> std::io::Result<()> {
        if self.decoder.is_some() {
            return Ok(());
        }
        let mut input = self.input.take().expect("XML reader initializes once");
        let mut raw_prefix = Vec::new();
        while raw_prefix.len() < 12 {
            let mut byte = [0u8; 1];
            if input.read(&mut byte)? == 0 {
                break;
            }
            raw_prefix.push(byte[0]);
        }
        if raw_prefix.starts_with(&[0x00, 0x00, 0xfe, 0xff])
            || raw_prefix.starts_with(&[0xff, 0xfe, 0x00, 0x00])
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported XML encoding declaration: UTF-32",
            ));
        }
        let detected = detect_stream_encoding(&raw_prefix);
        let has_declaration = stream_has_declaration(&raw_prefix, detected);
        if has_declaration {
            while !stream_declaration_complete(&raw_prefix, detected) {
                let mut byte = [0u8; 1];
                if input.read(&mut byte)? == 0 {
                    break;
                }
                raw_prefix.push(byte[0]);
            }
        }

        let decoded_declaration = has_declaration
            .then(|| decode_xml(&raw_prefix).map(Cow::into_owned))
            .transpose()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let declared = decoded_declaration
            .as_deref()
            .and_then(declaration_encoding);
        let encoding = match (detected, declared.as_deref()) {
            (Some(detected), _) => detected.encoding(),
            (None, Some(label)) if is_utf16_label(label) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported XML encoding declaration: {label}"),
                ));
            }
            (None, Some(label)) => {
                encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unsupported XML encoding declaration: {label}"),
                    )
                })?
            }
            (None, None) => encoding_rs::UTF_8,
        };
        let replay = std::io::BufReader::new(Cursor::new(raw_prefix).chain(input));
        let mut decoder = quick_xml::encoding::DecodingReader::new(replay);
        decoder.set_encoding(encoding);

        if has_declaration {
            let mut declaration = Vec::new();
            while !declaration.ends_with(b"?>") {
                let mut byte = [0u8; 1];
                if decoder.read(&mut byte)? == 0 {
                    break;
                }
                declaration.push(byte[0]);
            }
            if let Some(declared) = declared {
                normalize_declaration_encoding(&mut declaration, &declared);
            }
            self.prefix = Cursor::new(declaration);
        }
        self.decoder = Some(decoder);
        Ok(())
    }
}

impl<R: BufRead> Read for XmlUtf8Reader<R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.initialize()?;
        let prefix_read = self.prefix.read(output)?;
        if prefix_read != 0 || output.is_empty() {
            return Ok(prefix_read);
        }
        self.decoder
            .as_mut()
            .expect("XML reader is initialized")
            .read(output)
    }
}

fn detect_stream_encoding(bytes: &[u8]) -> Option<DetectedEncoding> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        Some(DetectedEncoding::Utf8)
    } else if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0x3c, 0x00, 0x3f, 0x00]) {
        Some(DetectedEncoding::Utf16Le)
    } else if bytes.starts_with(&[0xfe, 0xff]) || bytes.starts_with(&[0x00, 0x3c, 0x00, 0x3f]) {
        Some(DetectedEncoding::Utf16Be)
    } else {
        None
    }
}

fn stream_has_declaration(bytes: &[u8], detected: Option<DetectedEncoding>) -> bool {
    match detected {
        Some(DetectedEncoding::Utf16Le) => bytes
            .strip_prefix(&[0xff, 0xfe])
            .unwrap_or(bytes)
            .starts_with(b"<\0?\0x\0m\0l\0"),
        Some(DetectedEncoding::Utf16Be) => bytes
            .strip_prefix(&[0xfe, 0xff])
            .unwrap_or(bytes)
            .starts_with(b"\0<\0?\0x\0m\0l"),
        Some(DetectedEncoding::Utf8) | None => bytes
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(bytes)
            .starts_with(b"<?xml"),
    }
}

fn stream_declaration_complete(bytes: &[u8], detected: Option<DetectedEncoding>) -> bool {
    match detected {
        Some(DetectedEncoding::Utf16Le) => bytes.ends_with(b"?\0>\0"),
        Some(DetectedEncoding::Utf16Be) => bytes.ends_with(b"\0?\0>"),
        Some(DetectedEncoding::Utf8) | None => bytes.ends_with(b"?>"),
    }
}

fn normalize_declaration_encoding(declaration: &mut Vec<u8>, declared: &str) {
    let Some(name) = declaration
        .windows(b"encoding".len())
        .position(|window| window == b"encoding")
    else {
        return;
    };
    let Some(value) = declaration[name + b"encoding".len()..]
        .windows(declared.len())
        .position(|window| window == declared.as_bytes())
    else {
        return;
    };
    let start = name + b"encoding".len() + value;
    declaration.splice(start..start + declared.len(), b"UTF-8".iter().copied());
}

fn decode_strict<'a>(
    bytes: &'a [u8],
    encoding: &'static encoding_rs::Encoding,
    name: &'static str,
) -> Result<Cow<'a, str>, XmlDecodeError> {
    let (decoded, had_errors) = encoding.decode_without_bom_handling(bytes);
    if had_errors {
        Err(XmlDecodeError::InvalidBytes { encoding: name })
    } else {
        Ok(decoded)
    }
}

fn ensure_declaration_matches(
    declared: &str,
    detected: DetectedEncoding,
) -> Result<(), XmlDecodeError> {
    let matches = match detected {
        DetectedEncoding::Utf8 => encoding_rs::Encoding::for_label(declared.as_bytes())
            .is_some_and(|encoding| encoding == encoding_rs::UTF_8),
        DetectedEncoding::Utf16Le => {
            declared.eq_ignore_ascii_case("utf-16") || declared.eq_ignore_ascii_case("utf-16le")
        }
        DetectedEncoding::Utf16Be => {
            declared.eq_ignore_ascii_case("utf-16") || declared.eq_ignore_ascii_case("utf-16be")
        }
    };
    if matches {
        Ok(())
    } else if encoding_rs::Encoding::for_label(declared.as_bytes()).is_none()
        && !is_utf16_label(declared)
    {
        Err(XmlDecodeError::UnsupportedEncoding {
            encoding: declared.to_string(),
        })
    } else {
        Err(XmlDecodeError::ConflictingEncoding {
            declared: declared.to_string(),
            detected: detected.name(),
        })
    }
}

fn is_utf16_label(label: &str) -> bool {
    label.eq_ignore_ascii_case("utf-16")
        || label.eq_ignore_ascii_case("utf-16le")
        || label.eq_ignore_ascii_case("utf-16be")
}

fn ascii_declaration_encoding(bytes: &[u8]) -> Option<String> {
    let end = bytes.windows(2).position(|window| window == b"?>")? + 2;
    if !bytes[..end].is_ascii() {
        return None;
    }
    declaration_encoding(std::str::from_utf8(&bytes[..end]).ok()?)
}

fn declaration_encoding(xml: &str) -> Option<String> {
    let declaration = xml.strip_prefix("<?xml")?;
    if !declaration
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let declaration = declaration.split_once("?>")?.0;
    let bytes = declaration.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let name_start = index;
        while bytes.get(index).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.')
        }) {
            index += 1;
        }
        let name = &declaration[name_start..index];
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') {
            return None;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let quote = *bytes.get(index)?;
        if !matches!(quote, b'\'' | b'"') {
            return None;
        }
        index += 1;
        let value_start = index;
        while bytes.get(index).is_some_and(|byte| *byte != quote) {
            index += 1;
        }
        let value = &declaration[value_start..index];
        index += 1;
        if name == "encoding" {
            return Some(value.to_string());
        }
    }
    None
}

pub(crate) fn local_name(name: &[u8]) -> &[u8] {
    let name = name.rsplit(|byte| *byte == b'}').next().unwrap_or(name);
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

pub(crate) struct XmlAttrs {
    pub(crate) invalid: bool,
}

impl XmlAttrs {
    pub(crate) fn from_event(event: &BytesStart<'_>) -> Self {
        let mut invalid = false;
        for attr in event.attributes() {
            let attr = match attr {
                Ok(attr) => attr,
                Err(_) => {
                    invalid = true;
                    continue;
                }
            };
            if attr.normalized_value(XmlVersion::default()).is_err() {
                invalid = true;
            }
        }
        Self { invalid }
    }
}

pub(crate) fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn text_content(text: &quick_xml::events::BytesText<'_>) -> quick_xml::Result<String> {
    Ok(text.xml_content(XmlVersion::default())?.to_string())
}

pub(crate) fn cdata_content(
    cdata: &quick_xml::events::BytesCData<'_>,
) -> quick_xml::Result<String> {
    Ok(cdata.xml_content(XmlVersion::default())?.to_string())
}

pub(crate) fn push_general_ref(
    output: &mut String,
    reference: &BytesRef<'_>,
) -> quick_xml::Result<bool> {
    if let Some(ch) = reference.resolve_char_ref()? {
        output.push(ch);
        return Ok(false);
    }
    let name = reference.decode()?;
    if let Some(value) = resolve_predefined_entity(name.as_ref()) {
        output.push_str(value);
        Ok(false)
    } else {
        output.push('&');
        output.push_str(name.as_ref());
        output.push(';');
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_bytes(value: &str, little_endian: bool, bom: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if bom {
            bytes.extend(if little_endian {
                [0xff, 0xfe]
            } else {
                [0xfe, 0xff]
            });
        }
        for unit in value.encode_utf16() {
            bytes.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    #[test]
    fn decodes_utf8_bom_and_declaration() {
        let bytes = b"\xef\xbb\xbf<?xml version='1.0' encoding='UTF-8'?><root>caf\xc3\xa9</root>";

        assert_eq!(
            decode_xml(bytes).unwrap(),
            "<?xml version='1.0' encoding='UTF-8'?><root>caf\u{e9}</root>"
        );
    }

    #[test]
    fn decodes_utf16_bom_and_declarations() {
        let little = utf16_bytes(
            "<?xml version='1.0' encoding='UTF-16'?><root>\u{2603}</root>",
            true,
            true,
        );
        let big = utf16_bytes(
            "<?xml version='1.0' encoding='UTF-16BE'?><root>\u{2603}</root>",
            false,
            true,
        );

        assert!(
            decode_xml(&little)
                .unwrap()
                .contains("<root>\u{2603}</root>")
        );
        assert!(decode_xml(&big).unwrap().contains("<root>\u{2603}</root>"));
    }

    #[test]
    fn decodes_utf16_declaration_without_bom_from_signature() {
        let bytes = utf16_bytes(
            "<?xml version='1.0' encoding='UTF-16LE'?><root/>",
            true,
            false,
        );

        assert!(decode_xml(&bytes).unwrap().ends_with("<root/>"));
    }

    #[test]
    fn decodes_supported_encoding_rs_declaration() {
        let bytes = b"<?xml version='1.0' encoding='windows-1252'?><root>caf\xe9</root>";

        assert!(decode_xml(bytes).unwrap().contains("caf\u{e9}"));
    }

    #[test]
    fn streaming_decoder_handles_long_non_utf8_declarations() {
        let mut bytes = format!(
            "<?xml version='1.0' {} encoding='windows-1252'?><root>caf",
            " ".repeat(128)
        )
        .into_bytes();
        bytes.extend([0xe9]);
        bytes.extend(b"</root>");
        let mut decoded = String::new();

        XmlUtf8Reader::new(std::io::Cursor::new(bytes))
            .read_to_string(&mut decoded)
            .unwrap();

        assert!(decoded.contains("encoding='UTF-8'"));
        assert!(decoded.ends_with("<root>caf\u{e9}</root>"));
    }

    #[test]
    fn streaming_decoder_rejects_bom_declaration_conflicts() {
        let xml = "<?xml version='1.0' encoding='UTF-16BE'?><root/>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let mut decoded = String::new();

        assert!(
            XmlUtf8Reader::new(std::io::Cursor::new(bytes))
                .read_to_string(&mut decoded)
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_byte_sequences_without_substitution() {
        let error = decode_xml(b"<?xml version='1.0'?><root>\xff</root>").unwrap_err();

        assert_eq!(error, XmlDecodeError::InvalidBytes { encoding: "UTF-8" });
    }

    #[test]
    fn rejects_unsupported_declarations() {
        let error = decode_xml(b"<?xml version='1.0' encoding='UTF-32'?><root/>").unwrap_err();

        assert_eq!(
            error,
            XmlDecodeError::UnsupportedEncoding {
                encoding: "UTF-32".to_string()
            }
        );
    }

    #[test]
    fn rejects_declarations_conflicting_with_bom() {
        let bytes = b"\xef\xbb\xbf<?xml version='1.0' encoding='windows-1252'?><root/>";

        assert!(matches!(
            decode_xml(bytes),
            Err(XmlDecodeError::ConflictingEncoding { .. })
        ));
    }
}
