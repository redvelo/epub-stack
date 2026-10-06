//! Font obfuscation declared by `META-INF/encryption.xml`.
//!
//! OCF lets a publication obfuscate an embedded font by exclusive-or of its opening
//! bytes with a key derived from the publication identifier. That is an encoding of
//! the container rather than a property of the resource, so a committed read yields
//! the font the publication meant, the way a deflated entry yields its contents.
//!
//! Only the two obfuscation algorithms are undone. Anything else declared here is
//! encryption this crate cannot read, and those resources are left exactly as stored:
//! a reader that returned a plausible-looking decryption would be lying.
//!
//! Edit previews read the stored bytes instead, since an edit replaces them.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};

use quick_xml::{events::Event, reader::NsReader};
use sha1::{Digest, Sha1};

use crate::resource::EpubPath;
use crate::xml::decode_xml;

pub(crate) const ENCRYPTION_PATH: &str = "META-INF/encryption.xml";
pub(crate) const MAX_ENCRYPTION_BYTES: u64 = 4 * 1024 * 1024;

const IDPF_ALGORITHM: &str = "http://www.idpf.org/2008/embedding";
const ADOBE_ALGORITHM: &str = "http://ns.adobe.com/pdf/enc#RC";
const IDPF_PREFIX_BYTES: usize = 1040;
const ADOBE_PREFIX_BYTES: usize = 1024;

/// The obfuscation declared for one resource, resolved against the publication
/// identifier it is keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Obfuscation {
    key: Vec<u8>,
    prefix_bytes: usize,
}

/// Every resource whose stored bytes are obfuscated, by canonical path.
pub(crate) type Obfuscations = BTreeMap<EpubPath, Obfuscation>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Idpf,
    Adobe,
}

impl Algorithm {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            IDPF_ALGORITHM => Some(Self::Idpf),
            ADOBE_ALGORITHM => Some(Self::Adobe),
            _ => None,
        }
    }

    fn prefix_bytes(self) -> usize {
        match self {
            Self::Idpf => IDPF_PREFIX_BYTES,
            Self::Adobe => ADOBE_PREFIX_BYTES,
        }
    }

    /// The key this algorithm derives from the publication's unique identifier, or
    /// `None` when the identifier is not of the shape the algorithm requires.
    fn key(self, identifier: &str) -> Option<Vec<u8>> {
        match self {
            Self::Idpf => {
                let stripped: String = identifier
                    .chars()
                    .filter(|character| !matches!(character, ' ' | '\t' | '\r' | '\n'))
                    .collect();
                (!stripped.is_empty()).then(|| Sha1::digest(stripped.as_bytes()).to_vec())
            }
            Self::Adobe => {
                let hexadecimal: String = identifier
                    .rsplit(':')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .filter(|character| *character != '-')
                    .collect();
                if hexadecimal.len() != 32 {
                    return None;
                }
                (0..16)
                    .map(|index| {
                        u8::from_str_radix(&hexadecimal[index * 2..index * 2 + 2], 16).ok()
                    })
                    .collect()
            }
        }
    }
}

/// Reads `META-INF/encryption.xml` as the obfuscations it declares.
///
/// A document this cannot read declares nothing: the publication still opens, and its
/// resources are served as stored.
pub(crate) fn parse_obfuscations(input: impl BufRead, identifier: Option<&str>) -> Obfuscations {
    let mut obfuscations = Obfuscations::new();
    let Some(identifier) = identifier else {
        return obfuscations;
    };
    let mut input = input;
    let mut bytes = Vec::new();
    if input.read_to_end(&mut bytes).is_err() {
        return obfuscations;
    }
    let Ok(xml) = decode_xml(&bytes) else {
        return obfuscations;
    };
    let mut reader = NsReader::from_reader(xml.as_bytes());
    let mut buffer = Vec::new();
    let mut algorithm = None;
    let mut uri = None;
    loop {
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(_) => return obfuscations,
        };
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                match local_name(element.name().as_ref()) {
                    "EncryptedData" => {
                        algorithm = None;
                        uri = None;
                    }
                    "EncryptionMethod" => {
                        algorithm = attribute(element, "Algorithm").and_then(|value| {
                            Algorithm::parse(&value).map(|algorithm| (algorithm, value))
                        });
                    }
                    "CipherReference" => uri = attribute(element, "URI"),
                    _ => {}
                }
            }
            Event::End(ref element) if local_name(element.name().as_ref()) == "EncryptedData" => {
                if let (Some((algorithm, _)), Some(uri)) = (algorithm.take(), uri.take())
                    && let Some(path) = cipher_path(&uri)
                    && let Some(key) = algorithm.key(identifier)
                {
                    obfuscations.insert(
                        path,
                        Obfuscation {
                            key,
                            prefix_bytes: algorithm.prefix_bytes(),
                        },
                    );
                }
            }
            Event::Eof => return obfuscations,
            _ => {}
        }
        buffer.clear();
    }
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn attribute(element: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        (local_name(attribute.key.as_ref()) == name).then(|| attribute.value.into_owned())
    })
}

/// A cipher reference is an OCF path from the container root, percent-encoded.
fn cipher_path(uri: &str) -> Option<EpubPath> {
    let decoded = percent_encoding::percent_decode_str(uri)
        .decode_utf8()
        .ok()?;
    EpubPath::new(decoded.as_ref()).ok()
}

impl Obfuscation {
    /// Undoes the obfuscation as the stored bytes stream past.
    pub(crate) fn reader<'a>(&'a self, inner: &'a mut dyn Read) -> Deobfuscating<'a> {
        Deobfuscating {
            inner,
            obfuscation: self,
            position: 0,
        }
    }
}

pub(crate) struct Deobfuscating<'a> {
    inner: &'a mut dyn Read,
    obfuscation: &'a Obfuscation,
    position: usize,
}

impl Read for Deobfuscating<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        let start = self.position;
        self.position = self.position.saturating_add(read);
        if start >= self.obfuscation.prefix_bytes {
            return Ok(read);
        }
        let end = read.min(self.obfuscation.prefix_bytes - start);
        let key = &self.obfuscation.key;
        for (offset, byte) in buffer[..end].iter_mut().enumerate() {
            *byte ^= key[(start + offset) % key.len()];
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::{Algorithm, parse_obfuscations};
    use std::io::{Cursor, Read};

    const DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
    <enc:CipherData><enc:CipherReference URI="EPUB/fonts/Lobster%20Two.ttf"/></enc:CipherData>
  </enc:EncryptedData>
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://ns.adobe.com/pdf/enc#RC"/>
    <enc:CipherData><enc:CipherReference URI="EPUB/fonts/Adobe.otf"/></enc:CipherData>
  </enc:EncryptedData>
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes256-cbc"/>
    <enc:CipherData><enc:CipherReference URI="EPUB/fonts/Encrypted.otf"/></enc:CipherData>
  </enc:EncryptedData>
</encryption>"#;

    #[test]
    fn reads_only_the_obfuscation_algorithms() {
        let obfuscations = parse_obfuscations(
            Cursor::new(DOCUMENT),
            Some("urn:uuid:0123456789abcdef0123456789abcdef"),
        );
        let paths: Vec<_> = obfuscations.keys().map(|path| path.as_str()).collect();
        assert_eq!(
            paths,
            ["EPUB/fonts/Adobe.otf", "EPUB/fonts/Lobster Two.ttf"]
        );
    }

    #[test]
    fn declares_nothing_without_an_identifier_or_a_readable_document() {
        assert!(parse_obfuscations(Cursor::new(DOCUMENT), None).is_empty());
        assert!(parse_obfuscations(Cursor::new("<encryption>"), Some("x")).is_empty());
        assert!(parse_obfuscations(Cursor::new(""), Some("x")).is_empty());
    }

    #[test]
    fn derives_the_idpf_key_from_the_identifier_without_its_whitespace() {
        let spaced = Algorithm::Idpf.key("urn:uuid:0123\n4567").unwrap();
        assert_eq!(spaced, Algorithm::Idpf.key("urn:uuid:01234567").unwrap());
        assert_eq!(spaced.len(), 20);
        assert_eq!(Algorithm::Idpf.key(" "), None);
    }

    #[test]
    fn derives_the_adobe_key_from_the_uuid_only() {
        let key = Algorithm::Adobe
            .key("urn:uuid:01234567-89ab-cdef-0123-456789abcdef")
            .unwrap();
        assert_eq!(key.len(), 16);
        assert_eq!(key[0], 0x01);
        assert_eq!(key[15], 0xef);
        assert_eq!(Algorithm::Adobe.key("urn:uuid:not-a-uuid"), None);
        assert_eq!(Algorithm::Adobe.key("plain-identifier"), None);
    }

    #[test]
    fn undoes_the_obfuscation_across_partial_reads() {
        let identifier = "urn:uuid:01234567-89ab-cdef-0123-456789abcdef";
        let obfuscations = parse_obfuscations(Cursor::new(DOCUMENT), Some(identifier));
        let obfuscation = obfuscations
            .get(&crate::resource::EpubPath::new("EPUB/fonts/Adobe.otf").unwrap())
            .unwrap();
        let font: Vec<u8> = (0..1200_u32).map(|index| index as u8).collect();
        let mut stored = font.clone();
        let key = Algorithm::Adobe.key(identifier).unwrap();
        for (index, byte) in stored.iter_mut().take(1024).enumerate() {
            *byte ^= key[index % key.len()];
        }
        assert_ne!(stored, font);

        let mut source = Cursor::new(stored);
        let mut reader = obfuscation.reader(&mut source);
        let mut restored = Vec::new();
        let mut chunk = [0_u8; 7];
        loop {
            let read = reader.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            restored.extend_from_slice(&chunk[..read]);
        }
        assert_eq!(restored, font);
    }
}
