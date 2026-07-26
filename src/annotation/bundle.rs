use std::collections::HashSet;
use std::io::{Read, Seek, Write};

use percent_encoding::percent_decode_str;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::resource::{AuthoredHref, ParsedHref, parse_href};

use super::{Annotation, AnnotationError, AnnotationModelError, AnnotationSet};

pub(super) const ANNOTATIONS_JSON: &str = "annotations.json";
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 4_096;
pub(crate) const MAX_ANNOTATIONS_JSON_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Reports why an annotation ZIP bundle could not be read, built, changed, or written.
///
/// Bundle operations enforce 4,096 non-directory entries, 8 MiB for `annotations.json`, 64 MiB
/// per detached resource, and 256 MiB total uncompressed data. Limits apply to declared sizes and
/// bounded actual reads, protecting against misleading ZIP metadata.
#[derive(Debug, thiserror::Error)]
pub enum AnnotationBundleError {
    /// The archive did not contain `annotations.json`.
    #[error("missing annotations.json in annotation archive")]
    MissingAnnotationsJson,
    /// An archive or resource path was unsafe or non-canonical.
    #[error("invalid annotation bundle path: {path}")]
    InvalidPath {
        /// Rejected path.
        path: String,
    },
    /// Two non-directory entries had the same path.
    #[error("duplicate annotation bundle path: {path}")]
    DuplicatePath {
        /// Duplicated path.
        path: String,
    },
    /// A resource attempted to use the reserved `annotations.json` path.
    #[error("annotation bundle resource path is reserved: {path}")]
    ReservedPath {
        /// Reserved path.
        path: String,
    },
    /// A referenced audiovisual body resource was absent.
    #[error("annotation bundle is missing referenced resource: {path}")]
    MissingResource {
        /// Missing normalized path.
        path: String,
    },
    /// A supplied resource was not referenced by an audiovisual body.
    #[error("annotation bundle contains unreferenced resource: {path}")]
    UnreferencedResource {
        /// Unreferenced path.
        path: String,
    },
    /// The non-directory entry ceiling was exceeded.
    #[error("annotation bundle contains {count} non-directory entries; maximum is {max}")]
    EntryCountExceeded {
        /// Observed entry count.
        count: usize,
        /// Maximum accepted count.
        max: usize,
    },
    /// `annotations.json` exceeded its byte ceiling.
    #[error("annotations.json is {size} bytes; maximum is {max}")]
    AnnotationsJsonTooLarge {
        /// Observed uncompressed bytes.
        size: u64,
        /// Maximum accepted bytes.
        max: u64,
    },
    /// One detached resource exceeded its byte ceiling.
    #[error("annotation bundle resource {path} is {size} bytes; maximum is {max}")]
    ResourceTooLarge {
        /// Resource path.
        path: String,
        /// Observed uncompressed bytes.
        size: u64,
        /// Maximum accepted bytes.
        max: u64,
    },
    /// Total uncompressed bundle data exceeded its byte ceiling.
    #[error("annotation bundle contains {size} uncompressed bytes; maximum is {max}")]
    TotalUncompressedSizeExceeded {
        /// Observed total bytes.
        size: u64,
        /// Maximum accepted total bytes.
        max: u64,
    },
    /// An underlying I/O operation failed.
    #[error("IO error: {source}")]
    Io {
        /// Underlying I/O error.
        #[from]
        source: std::io::Error,
    },
    /// ZIP structure or encoding was invalid.
    #[error("ZIP error: {source}")]
    Zip {
        /// Underlying ZIP error.
        #[from]
        source: zip::result::ZipError,
    },
    /// `annotations.json` could not be decoded or encoded.
    #[error("annotation JSON error: {source}")]
    Annotation {
        /// Annotation JSON error.
        #[from]
        source: AnnotationError,
    },
    /// A strict annotation model invariant failed.
    #[error("annotation model error: {source}")]
    Model {
        /// Annotation model error.
        #[from]
        source: AnnotationModelError,
    },
}

/// Supplies one bundle-relative audiovisual body resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationResource {
    path: String,
    bytes: Vec<u8>,
}

impl AnnotationResource {
    /// Supplies a path and bytes for later validation by [`AnnotationBundle`].
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }

    /// Returns the bundle-relative resource path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the resource bytes.
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

/// Exchanges an annotation set with exactly the audiovisual resources it references.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationBundle {
    pub(super) set: AnnotationSet,
    pub(super) resources: Vec<AnnotationResource>,
}

impl AnnotationBundle {
    /// Builds a bundle after validating paths, limits, and referenced-resource completeness.
    pub fn new(
        set: AnnotationSet,
        resources: Vec<AnnotationResource>,
    ) -> Result<Self, AnnotationBundleError> {
        validate_bundle(&set, &resources)?;
        Ok(Self { set, resources })
    }

    /// Returns the annotation set.
    pub fn set(&self) -> &AnnotationSet {
        &self.set
    }

    /// Returns detached resources in bundle order.
    pub fn resources(&self) -> &[AnnotationResource] {
        self.resources.as_slice()
    }

    /// Consumes the bundle into its set and detached resources.
    pub fn into_parts(self) -> (AnnotationSet, Vec<AnnotationResource>) {
        (self.set, self.resources)
    }

    /// Adds an annotation and its supplied resources as one atomic change.
    pub fn add_annotation(
        &mut self,
        annotation: Annotation,
        resources: Vec<AnnotationResource>,
    ) -> Result<(), AnnotationBundleError> {
        let mut set = self.set.clone();
        set.add_annotation(annotation)?;
        let resources = resources_for_set(&set, self.resources.clone(), resources)?;
        *self = Self::new(set, resources)?;
        Ok(())
    }

    /// Replaces an annotation and updates its resources as one atomic change.
    pub fn replace_annotation(
        &mut self,
        annotation: Annotation,
        resources: Vec<AnnotationResource>,
    ) -> Result<Annotation, AnnotationBundleError> {
        let mut set = self.set.clone();
        let replaced = set.replace_annotation(annotation)?;
        let resources = resources_for_set(&set, self.resources.clone(), resources)?;
        *self = Self::new(set, resources)?;
        Ok(replaced)
    }

    /// Removes an annotation and now-unreferenced resources as one atomic change.
    pub fn remove_annotation(&mut self, id: &str) -> Result<Annotation, AnnotationBundleError> {
        let mut set = self.set.clone();
        let removed = set.remove_annotation(id)?;
        let resources = resources_for_set(&set, self.resources.clone(), Vec::new())?;
        *self = Self::new(set, resources)?;
        Ok(removed)
    }

    /// Imports a ZIP bundle while enforcing path, entry-count, and uncompressed-size limits.
    pub fn read_archive<R: Read + Seek>(reader: R) -> Result<Self, AnnotationBundleError> {
        let mut zip = ZipArchive::new(reader)?;
        let mut entry_count = 0;
        let mut declared_total_size = 0_u64;
        for index in 0..zip.len() {
            let file = zip.by_index(index)?;
            let path = file.name().to_string();
            validate_archive_entry_path(&path, file.is_dir())?;
            if file.is_dir() {
                continue;
            }
            entry_count += 1;
            if entry_count > MAX_ARCHIVE_ENTRIES {
                return Err(AnnotationBundleError::EntryCountExceeded {
                    count: entry_count,
                    max: MAX_ARCHIVE_ENTRIES,
                });
            }
            check_archive_entry_size(&path, file.size())?;
            declared_total_size = declared_total_size.saturating_add(file.size());
            check_total_size(declared_total_size)?;
        }
        let mut seen = HashSet::new();
        let mut annotations_json = None;
        let mut resources = Vec::new();
        let mut total_size = 0_u64;

        for index in 0..zip.len() {
            let mut file = zip.by_index(index)?;
            let path = file.name().to_string();
            validate_archive_entry_path(&path, file.is_dir())?;
            if file.is_dir() {
                continue;
            }
            if !seen.insert(path.clone()) {
                return Err(AnnotationBundleError::DuplicatePath { path });
            }

            let limit = if path == ANNOTATIONS_JSON {
                MAX_ANNOTATIONS_JSON_BYTES
            } else {
                MAX_ARCHIVE_RESOURCE_BYTES
            };
            check_archive_entry_size(&path, file.size())?;
            let bytes = read_archive_entry_bounded(&mut file, limit)?;
            let size = bytes.len() as u64;
            check_archive_entry_size(&path, size)?;
            total_size = total_size.saturating_add(size);
            check_total_size(total_size)?;

            if path == ANNOTATIONS_JSON {
                let content = std::str::from_utf8(&bytes)
                    .map_err(|source| AnnotationError::Utf8 { source })?
                    .to_string();
                annotations_json = Some(content);
            } else {
                resources.push(AnnotationResource::new(path, bytes));
            }
        }

        let annotations_json =
            annotations_json.ok_or(AnnotationBundleError::MissingAnnotationsJson)?;
        let set = AnnotationSet::parse_json(&annotations_json)?;
        Self::new(set, resources)
    }

    /// Exports a revalidated bundle as a normalized, deflated ZIP archive.
    pub fn write_archive<W: Write + Seek>(&self, writer: W) -> Result<W, AnnotationBundleError> {
        validate_bundle(&self.set, &self.resources)?;
        let mut zip = ZipWriter::new(writer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file(ANNOTATIONS_JSON, options)?;
        zip.write_all(self.set.to_json_string()?.as_bytes())?;
        for resource in &self.resources {
            zip.start_file(resource.path(), options)?;
            zip.write_all(resource.bytes())?;
        }
        Ok(zip.finish()?)
    }
}

fn resources_for_set(
    set: &AnnotationSet,
    mut existing: Vec<AnnotationResource>,
    supplied: Vec<AnnotationResource>,
) -> Result<Vec<AnnotationResource>, AnnotationBundleError> {
    let referenced = set
        .audiovisual_body_resource_paths()
        .collect::<HashSet<_>>();
    existing.retain(|resource| referenced.contains(resource.path()));

    let mut supplied_paths = HashSet::new();
    for resource in supplied {
        let path = resource.path().to_string();
        if !supplied_paths.insert(path.clone()) {
            return Err(AnnotationBundleError::DuplicatePath { path });
        }
        if !referenced.contains(&path) {
            return Err(AnnotationBundleError::UnreferencedResource { path });
        }
        if let Some(index) = existing.iter().position(|item| item.path() == path) {
            existing[index] = resource;
        } else {
            existing.push(resource);
        }
    }
    Ok(existing)
}

fn validate_bundle(
    set: &AnnotationSet,
    resources: &[AnnotationResource],
) -> Result<(), AnnotationBundleError> {
    let entry_count = resources.len().saturating_add(1);
    if entry_count > MAX_ARCHIVE_ENTRIES {
        return Err(AnnotationBundleError::EntryCountExceeded {
            count: entry_count,
            max: MAX_ARCHIVE_ENTRIES,
        });
    }

    let json_size = set.to_json_string()?.len() as u64;
    check_archive_entry_size(ANNOTATIONS_JSON, json_size)?;
    let mut total_size = json_size;
    let mut seen = HashSet::new();
    for resource in resources {
        validate_archive_path(resource.path())?;
        if resource.path() == ANNOTATIONS_JSON {
            return Err(AnnotationBundleError::ReservedPath {
                path: resource.path().to_string(),
            });
        }
        if !seen.insert(resource.path()) {
            return Err(AnnotationBundleError::DuplicatePath {
                path: resource.path().to_string(),
            });
        }
        let size = resource.bytes().len() as u64;
        check_archive_entry_size(resource.path(), size)?;
        total_size = total_size.saturating_add(size);
        check_total_size(total_size)?;
    }

    let referenced = set
        .audiovisual_body_resource_paths()
        .collect::<HashSet<_>>();
    if let Some(path) = referenced
        .iter()
        .filter(|path| !seen.contains(path.as_str()))
        .min()
    {
        return Err(AnnotationBundleError::MissingResource { path: path.clone() });
    }
    if let Some(path) = seen
        .iter()
        .filter(|path| !referenced.contains(**path))
        .min()
    {
        return Err(AnnotationBundleError::UnreferencedResource {
            path: (*path).to_string(),
        });
    }
    Ok(())
}

fn check_archive_entry_size(path: &str, size: u64) -> Result<(), AnnotationBundleError> {
    let limit = if path == ANNOTATIONS_JSON {
        MAX_ANNOTATIONS_JSON_BYTES
    } else {
        MAX_ARCHIVE_RESOURCE_BYTES
    };
    if size > limit {
        return Err(archive_entry_size_error(path, size));
    }
    Ok(())
}

fn check_total_size(size: u64) -> Result<(), AnnotationBundleError> {
    if size > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
        return Err(AnnotationBundleError::TotalUncompressedSizeExceeded {
            size,
            max: MAX_ARCHIVE_UNCOMPRESSED_BYTES,
        });
    }
    Ok(())
}

fn read_archive_entry_bounded(
    file: &mut impl Read,
    limit: u64,
) -> Result<Vec<u8>, AnnotationBundleError> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn archive_entry_size_error(path: &str, size: u64) -> AnnotationBundleError {
    if path == ANNOTATIONS_JSON {
        AnnotationBundleError::AnnotationsJsonTooLarge {
            size,
            max: MAX_ANNOTATIONS_JSON_BYTES,
        }
    } else {
        AnnotationBundleError::ResourceTooLarge {
            path: path.to_string(),
            size,
            max: MAX_ARCHIVE_RESOURCE_BYTES,
        }
    }
}

fn validate_archive_path(path: &str) -> Result<(), AnnotationBundleError> {
    if !is_safe_annotation_resource_path(path) {
        return Err(AnnotationBundleError::InvalidPath {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn validate_archive_entry_path(path: &str, is_dir: bool) -> Result<(), AnnotationBundleError> {
    let path_to_validate = if is_dir {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if normalize_annotation_resource_path(path_to_validate).as_deref() != Some(path_to_validate) {
        return Err(AnnotationBundleError::InvalidPath {
            path: path.to_string(),
        });
    }
    Ok(())
}

fn is_safe_annotation_resource_path(path: &str) -> bool {
    is_safe_normalized_annotation_resource_path(path)
}

pub(crate) fn normalize_annotation_resource_path(path: &str) -> Option<String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return None;
    }
    let ParsedHref::Local { target, .. } = parse_href(AuthoredHref::new(path.to_string())) else {
        return None;
    };
    let path = target
        .as_str()
        .split_once('?')
        .map_or(target.as_str(), |(path, _)| path);
    let mut segments = Vec::new();
    for segment in path.split('/') {
        let segment = percent_decode_str(segment).decode_utf8().ok()?;
        if segment.contains(['/', '\0', '\\']) {
            return None;
        }
        match segment.as_ref() {
            "" => return None,
            "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment.to_string()),
        }
    }
    let normalized = segments.join("/");
    is_safe_normalized_annotation_resource_path(&normalized).then_some(normalized)
}

fn is_safe_normalized_annotation_resource_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with("./")
        && !path.contains('\\')
        && !path.contains(':')
        && !path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    fn empty_set() -> AnnotationSet {
        AnnotationSet::parse_json(
            r#"{"id":"urn:test:set","type":"AnnotationSet","about":{},"items":[]}"#,
        )
        .unwrap()
    }

    fn annotation(id: &str, body: &str) -> Annotation {
        let json = format!(
            r#"{{"id":"urn:test:set","type":"AnnotationSet","about":{{}},"items":[{{"id":"urn:test:{id}","type":"Annotation","created":"2026-07-15T00:00:00Z","target":{{"source":"chapter.xhtml"}}{body}}}]}}"#
        );
        AnnotationSet::parse_json(&json).unwrap().items()[0].clone()
    }

    fn set_with(annotation: Annotation) -> AnnotationSet {
        let mut set = empty_set();
        set.add_annotation(annotation).unwrap();
        set
    }

    fn archive_with(write: impl FnOnce(&mut ZipWriter<&mut Cursor<Vec<u8>>>)) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            write(&mut zip);
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn archive_round_trip_keeps_detached_resources() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[{
                    "id":"urn:test:a", "type":"Annotation", "created":"2026-07-15T00:00:00Z",
                    "target":{"source":"chapter.xhtml"},
                    "body":{"type":"Audio","id":"audio/note.mp3"}
                }]
            }"#,
        )
        .unwrap();
        let bundle = AnnotationBundle::new(
            set,
            vec![AnnotationResource::new("audio/note.mp3", b"audio".to_vec())],
        )
        .unwrap();

        let archive = bundle.write_archive(Cursor::new(Vec::new())).unwrap();
        let read = AnnotationBundle::read_archive(Cursor::new(archive.into_inner())).unwrap();

        assert_eq!(read.resources()[0].path(), "audio/note.mp3");
        assert_eq!(read.resources()[0].bytes(), b"audio");
    }

    #[test]
    fn resource_paths_are_normalized_without_escaping_the_bundle() {
        for (path, expected) in [
            ("audio/my%20note.mp3", Some("audio/my note.mp3")),
            (
                "./audio/x/../note.mp3?download=1#t=1",
                Some("audio/note.mp3"),
            ),
            ("%2e%2e/voice.mp3", None),
            ("%2E%2E/voice.mp3", None),
            ("audio/%2e%2e/%2e%2e/voice.mp3", None),
            ("audio%2fvoice.mp3", None),
            ("audio%5cvoice.mp3", None),
            ("../outside.mp3", None),
            ("https://example.com/note.mp3", None),
        ] {
            assert_eq!(
                normalize_annotation_resource_path(path).as_deref(),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn bundle_enforces_exact_resource_closure() {
        let first = annotation(
            "first",
            r#","body":{"type":"Audio","id":"./audio/voice.mp3"}"#,
        );
        let second = annotation(
            "second",
            r#","body":{"type":"Audio","id":"audio/voice.mp3"}"#,
        );
        let mut bundle = AnnotationBundle::new(
            set_with(first),
            vec![AnnotationResource::new(
                "audio/voice.mp3",
                b"voice".to_vec(),
            )],
        )
        .unwrap();

        bundle.add_annotation(second, Vec::new()).unwrap();
        bundle.remove_annotation("urn:test:first").unwrap();
        assert_eq!(bundle.resources().len(), 1);

        let replacement = annotation(
            "second",
            r#","body":{"type":"TextualBody","value":"transcript"}"#,
        );
        bundle.replace_annotation(replacement, Vec::new()).unwrap();
        assert!(bundle.resources().is_empty());

        let missing = annotation(
            "third",
            r#","body":{"type":"Video","id":"video/note.webm"}"#,
        );
        let before = bundle.clone();
        assert!(matches!(
            bundle.add_annotation(missing.clone(), Vec::new()),
            Err(AnnotationBundleError::MissingResource { .. })
        ));
        assert_eq!(bundle, before);

        bundle
            .add_annotation(
                missing,
                vec![AnnotationResource::new(
                    "video/note.webm",
                    b"video".to_vec(),
                )],
            )
            .unwrap();
        bundle
            .replace_annotation(
                annotation(
                    "third",
                    r#","body":{"type":"Video","id":"video/note.webm"}"#,
                ),
                vec![AnnotationResource::new(
                    "video/note.webm",
                    b"updated".to_vec(),
                )],
            )
            .unwrap();
        assert_eq!(bundle.resources()[0].bytes(), b"updated");
    }

    #[test]
    fn constructor_rejects_resource_closure_and_path_errors() {
        let audio = set_with(annotation(
            "audio",
            r#","body":{"type":"Audio","id":"audio.mp3"}"#,
        ));
        assert!(matches!(
            AnnotationBundle::new(audio.clone(), Vec::new()),
            Err(AnnotationBundleError::MissingResource { path }) if path == "audio.mp3"
        ));
        assert!(matches!(
            AnnotationBundle::new(
                audio.clone(),
                vec![
                    AnnotationResource::new("audio.mp3", Vec::new()),
                    AnnotationResource::new("audio.mp3", Vec::new()),
                ],
            ),
            Err(AnnotationBundleError::DuplicatePath { path }) if path == "audio.mp3"
        ));
        assert!(matches!(
            AnnotationBundle::new(
                empty_set(),
                vec![AnnotationResource::new("unused.bin", Vec::new())],
            ),
            Err(AnnotationBundleError::UnreferencedResource { path }) if path == "unused.bin"
        ));
        assert!(matches!(
            AnnotationBundle::new(
                audio,
                vec![AnnotationResource::new("../escape.mp3", Vec::new())],
            ),
            Err(AnnotationBundleError::InvalidPath { .. })
        ));
    }

    #[test]
    fn recovered_partial_set_can_be_bundled() {
        let set =
            AnnotationSet::parse_json(r#"{"type":"AnnotationSet","about":{},"items":[]}"#).unwrap();
        assert!(
            AnnotationBundle::new(set, Vec::new())
                .unwrap()
                .set()
                .id()
                .is_none()
        );
    }

    #[test]
    fn archive_reports_invalid_utf8_and_recovers_malformed_members() {
        let bytes = archive_with(|zip| {
            zip.start_file(ANNOTATIONS_JSON, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&[0xff, 0xfe]).unwrap();
        });
        assert!(matches!(
            AnnotationBundle::read_archive(Cursor::new(bytes)),
            Err(AnnotationBundleError::Annotation {
                source: AnnotationError::Utf8 { .. }
            })
        ));

        let bytes = archive_with(|zip| {
            zip.start_file(ANNOTATIONS_JSON, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(br#"{"id":"urn:test:set","type":"AnnotationSet","about":{},"items":{}}"#)
                .unwrap();
        });
        let bundle = AnnotationBundle::read_archive(Cursor::new(bytes)).unwrap();
        let written = bundle.write_archive(Cursor::new(Vec::new())).unwrap();
        assert!(
            AnnotationBundle::read_archive(Cursor::new(written.into_inner()))
                .unwrap()
                .set()
                .items()
                .is_empty()
        );
    }

    #[test]
    fn archive_entry_limit_ignores_directories() {
        let bytes = archive_with(|zip| {
            for index in 0..=MAX_ARCHIVE_ENTRIES {
                zip.start_file(format!("resource-{index}"), SimpleFileOptions::default())
                    .unwrap();
            }
        });
        assert!(matches!(
            AnnotationBundle::read_archive(Cursor::new(bytes)),
            Err(AnnotationBundleError::EntryCountExceeded { count, max })
                if count == MAX_ARCHIVE_ENTRIES + 1 && max == MAX_ARCHIVE_ENTRIES
        ));

        let bytes = archive_with(|zip| {
            for index in 0..=MAX_ARCHIVE_ENTRIES {
                zip.add_directory(format!("directory-{index}/"), SimpleFileOptions::default())
                    .unwrap();
            }
            zip.start_file(ANNOTATIONS_JSON, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(empty_set().to_json_string().unwrap().as_bytes())
                .unwrap();
        });
        assert!(
            AnnotationBundle::read_archive(Cursor::new(bytes))
                .unwrap()
                .resources()
                .is_empty()
        );
    }

    #[test]
    fn archive_rejects_unsafe_directory_paths() {
        for path in [
            "../../escape/",
            "/absolute/",
            "backslash\\path/",
            "%2E%2E/escape/",
            "percent%2Fslash/",
        ] {
            let bytes = archive_with(|zip| {
                zip.add_directory(path, SimpleFileOptions::default())
                    .unwrap();
            });
            assert!(matches!(
                AnnotationBundle::read_archive(Cursor::new(bytes)),
                Err(AnnotationBundleError::InvalidPath { path: actual }) if actual == path
            ));
        }
    }

    #[test]
    fn construction_and_write_enforce_json_and_entry_limits() {
        let mut oversized = empty_set();
        oversized.extra.insert(
            "large".to_string(),
            serde_json::Value::String("x".repeat(MAX_ANNOTATIONS_JSON_BYTES as usize)),
        );
        assert!(matches!(
            AnnotationBundle::new(oversized, Vec::new()),
            Err(AnnotationBundleError::AnnotationsJsonTooLarge { .. })
        ));

        let mut bundle = AnnotationBundle::new(empty_set(), Vec::new()).unwrap();
        bundle.set.extra.insert(
            "large".to_string(),
            serde_json::Value::String("x".repeat(MAX_ANNOTATIONS_JSON_BYTES as usize)),
        );
        assert!(matches!(
            bundle.write_archive(Cursor::new(Vec::new())),
            Err(AnnotationBundleError::AnnotationsJsonTooLarge { .. })
        ));

        let resources = (0..MAX_ARCHIVE_ENTRIES)
            .map(|index| AnnotationResource::new(format!("resource-{index}"), Vec::new()))
            .collect();
        assert!(matches!(
            AnnotationBundle::new(empty_set(), resources),
            Err(AnnotationBundleError::EntryCountExceeded { .. })
        ));
    }

    #[test]
    fn archive_bounds_json_resource_and_total_uncompressed_reads() {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let bytes = archive_with(|zip| {
            zip.start_file(ANNOTATIONS_JSON, options).unwrap();
            write_repeated(zip, MAX_ANNOTATIONS_JSON_BYTES + 1, b' ');
        });
        assert!(matches!(
            AnnotationBundle::read_archive(Cursor::new(bytes)),
            Err(AnnotationBundleError::AnnotationsJsonTooLarge { size, max })
                if size > MAX_ANNOTATIONS_JSON_BYTES && max == MAX_ANNOTATIONS_JSON_BYTES
        ));

        let bytes = archive_with(|zip| {
            zip.start_file(ANNOTATIONS_JSON, options).unwrap();
            zip.write_all(empty_set().to_json_string().unwrap().as_bytes())
                .unwrap();
            zip.start_file("large.bin", options).unwrap();
            write_repeated(zip, MAX_ARCHIVE_RESOURCE_BYTES + 1, 0);
        });
        assert!(matches!(
            AnnotationBundle::read_archive(Cursor::new(bytes)),
            Err(AnnotationBundleError::ResourceTooLarge { path, size, max })
                if path == "large.bin" && size > MAX_ARCHIVE_RESOURCE_BYTES
                    && max == MAX_ARCHIVE_RESOURCE_BYTES
        ));

        let bytes = archive_with(|zip| {
            zip.start_file(ANNOTATIONS_JSON, options).unwrap();
            zip.write_all(b"{}").unwrap();
            for index in 0..(MAX_ARCHIVE_UNCOMPRESSED_BYTES / MAX_ARCHIVE_RESOURCE_BYTES) {
                zip.start_file(format!("resource-{index}.bin"), options)
                    .unwrap();
                write_repeated(zip, MAX_ARCHIVE_RESOURCE_BYTES, 0);
            }
        });
        assert!(matches!(
            AnnotationBundle::read_archive(Cursor::new(bytes)),
            Err(AnnotationBundleError::TotalUncompressedSizeExceeded { size, max })
                if size > MAX_ARCHIVE_UNCOMPRESSED_BYTES
                    && max == MAX_ARCHIVE_UNCOMPRESSED_BYTES
        ));
    }

    fn write_repeated(writer: &mut impl Write, bytes: u64, byte: u8) {
        let chunk = [byte; 8192];
        for _ in 0..(bytes / chunk.len() as u64) {
            writer.write_all(&chunk).unwrap();
        }
        writer
            .write_all(&chunk[..(bytes % chunk.len() as u64) as usize])
            .unwrap();
    }
}
