use super::Epub;
use crate::{
    annotation::{
        AnnotationBundle, AnnotationBundleError, AnnotationError, AnnotationResource,
        AnnotationSet, EmbeddedAnnotationsError, MAX_ANNOTATIONS_JSON_BYTES, MAX_ARCHIVE_ENTRIES,
        MAX_ARCHIVE_RESOURCE_BYTES, MAX_ARCHIVE_UNCOMPRESSED_BYTES,
    },
    edit::EditError,
    publication::persistence::ResourceChanges,
    resource::{
        EpubPath,
        provider::{ProviderReadError, ResourceProvider, ResourceProviderIndex},
    },
};
use std::{collections::HashSet, io::Read};

impl<R: ResourceProvider> Epub<R> {
    /// Loads annotations packaged inside this EPUB, when present.
    ///
    /// Each call reflects the current committed publication state and returns an independent
    /// [`AnnotationBundle`]. It reads `META-INF/annotations.json` and each distinct referenced
    /// audiovisual body below `META-INF/`; unrelated files are not read. A missing annotations
    /// document returns `Ok(None)`. The result can become stale after a later edit.
    ///
    /// The operation permits at most 4,096 entries including `annotations.json`, 8 MiB of JSON,
    /// 64 MiB per referenced resource, and 256 MiB of total loaded payload bytes. The aggregate
    /// limit counts JSON and referenced resource payloads, not all memory allocated while parsing
    /// or modeling them. Provider-index limits come from [`super::EpubOpenLimits`]. Reads may
    /// consume one byte beyond an individual limit to detect overflow.
    ///
    /// Parsing recovers malformed representable members according to [`AnnotationSet`] rules and
    /// retains supported extension data, but original JSON whitespace, member ordering, and
    /// malformed shapes are not source-preserved; later serialization is normalized. Referenced
    /// resource payload bytes are preserved exactly in the returned bundle.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddedAnnotationsError`] if the effective index cannot be built, JSON or a
    /// referenced resource cannot be read within limits, the JSON has no usable annotation-set
    /// root, a protected OCF control path is referenced, or referenced-resource closure cannot be
    /// represented. Missing referenced body resources are reported as bundle errors rather than
    /// silently producing a partial bundle.
    pub fn embedded_annotations(
        &self,
    ) -> std::result::Result<Option<AnnotationBundle>, EmbeddedAnnotationsError> {
        let provider_index = self
            .resource_changes
            .apply_to_index(
                &self.provider_index,
                self.open_limits.provider_index_limits(),
            )
            .map_err(|source| EmbeddedAnnotationsError::ProviderIndex { source })?;
        load_embedded_annotations(&self.container, &self.resource_changes, &provider_index)
    }

    pub(crate) fn embedded_annotations_with_changes(
        &self,
        changes: &ResourceChanges,
        provider_index: &ResourceProviderIndex,
    ) -> std::result::Result<Option<AnnotationBundle>, EmbeddedAnnotationsError> {
        load_embedded_annotations(&self.container, changes, provider_index)
    }
}

fn load_embedded_annotations<R: ResourceProvider>(
    provider: &R,
    changes: &ResourceChanges,
    provider_index: &ResourceProviderIndex,
) -> std::result::Result<Option<AnnotationBundle>, EmbeddedAnnotationsError> {
    let annotations_path =
        EpubPath::new("META-INF/annotations.json").expect("static annotation path is valid");
    if provider_index.get(&annotations_path).is_none() {
        return Ok(None);
    }

    let bytes = embedded_annotation_bytes_bounded(
        provider,
        changes,
        &annotations_path,
        MAX_ANNOTATIONS_JSON_BYTES,
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|source| AnnotationError::Utf8 { source })?;
    let set = AnnotationSet::parse_json(text)?;
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut total_size = bytes.len() as u64;
    for path in set.audiovisual_body_resource_paths() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if is_protected_embedded_annotation_resource_path(&path) {
            return Err(AnnotationBundleError::ReservedPath { path }.into());
        }
        if seen.len().saturating_add(1) > MAX_ARCHIVE_ENTRIES {
            return Err(AnnotationBundleError::EntryCountExceeded {
                count: seen.len() + 1,
                max: MAX_ARCHIVE_ENTRIES,
            }
            .into());
        }
        let epub_path = EpubPath::new(format!("META-INF/{path}"))
            .expect("normalized annotation resource path is valid");
        if provider_index.get(&epub_path).is_none() {
            continue;
        }
        let bytes = embedded_annotation_bytes_bounded(
            provider,
            changes,
            &epub_path,
            MAX_ARCHIVE_RESOURCE_BYTES,
        )?;
        total_size = total_size.saturating_add(bytes.len() as u64);
        if total_size > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
            return Err(AnnotationBundleError::TotalUncompressedSizeExceeded {
                size: total_size,
                max: MAX_ARCHIVE_UNCOMPRESSED_BYTES,
            }
            .into());
        }
        resources.push(AnnotationResource::new(path, bytes));
    }

    Ok(Some(AnnotationBundle::new(set, resources)?))
}

fn embedded_annotation_bytes_bounded<R: ResourceProvider>(
    provider: &R,
    changes: &ResourceChanges,
    path: &EpubPath,
    limit: u64,
) -> std::result::Result<Vec<u8>, EmbeddedAnnotationsError> {
    let bytes = if let Some(change) = changes.entry(path.as_path()) {
        change
            .map(Vec::from)
            .ok_or_else(|| EmbeddedAnnotationsError::ResourceRead {
                path: path.clone(),
                source: ProviderReadError::MissingResource { path: path.clone() },
            })?
    } else {
        provider
            .read_with(path, |reader| {
                let mut bytes = Vec::new();
                reader
                    .take(limit.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|source| ProviderReadError::IoPath {
                        source,
                        path: path.clone(),
                    })?;
                Ok::<_, ProviderReadError>(bytes)
            })
            .map_err(|source| EmbeddedAnnotationsError::ResourceRead {
                path: path.clone(),
                source,
            })?
            .map_err(|source| EmbeddedAnnotationsError::ResourceRead {
                path: path.clone(),
                source,
            })?
    };
    if bytes.len() as u64 > limit {
        let source = if path.as_str() == "META-INF/annotations.json" {
            AnnotationBundleError::AnnotationsJsonTooLarge {
                size: bytes.len() as u64,
                max: limit,
            }
        } else {
            AnnotationBundleError::ResourceTooLarge {
                path: path.as_str().to_string(),
                size: bytes.len() as u64,
                max: limit,
            }
        };
        return Err(source.into());
    }
    Ok(bytes)
}

pub(crate) fn is_protected_embedded_annotation_resource_path(path: &str) -> bool {
    matches!(
        path,
        "annotations.json"
            | "container.xml"
            | "encryption.xml"
            | "manifest.xml"
            | "metadata.xml"
            | "rights.xml"
            | "signatures.xml"
    )
}

pub(crate) fn provider_bytes_with_changes_bounded<R: ResourceProvider>(
    provider: &R,
    changes: &ResourceChanges,
    path: &EpubPath,
    limit: u64,
) -> std::result::Result<Vec<u8>, EditError> {
    let bytes = if let Some(change) = changes.entry(path.as_path()) {
        change
            .map(Vec::from)
            .ok_or_else(|| ProviderReadError::MissingResource { path: path.clone() })?
    } else {
        provider.read_with(path, |reader| {
            let mut bytes = Vec::new();
            reader
                .take(limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| ProviderReadError::IoPath {
                    source,
                    path: path.clone(),
                })?;
            Ok::<_, ProviderReadError>(bytes)
        })??
    };
    if bytes.len() as u64 > limit {
        let error = if path.as_str() == "META-INF/annotations.json" {
            AnnotationBundleError::AnnotationsJsonTooLarge {
                size: bytes.len() as u64,
                max: MAX_ANNOTATIONS_JSON_BYTES,
            }
        } else {
            AnnotationBundleError::ResourceTooLarge {
                path: path.as_str().to_string(),
                size: bytes.len() as u64,
                max: limit,
            }
        };
        return Err(error.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::provider::MemoryResourceProvider;

    fn memory_provider() -> MemoryResourceProvider {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/text/chapter.xhtml", b"<html/>".to_vec()),
        ])
        .unwrap()
    }

    fn memory_provider_epub() -> Epub<MemoryResourceProvider> {
        Epub::from_provider(memory_provider(), "EPUB/package.opf").unwrap()
    }

    fn annotation_json(body_type: &str, body_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "about": {},
            "items": [{
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "text/chapter.xhtml"},
                "body": {"type": body_type, "id": body_id}
            }]
        }))
        .unwrap()
    }

    #[test]
    fn embedded_annotations_are_absent_when_meta_inf_annotations_json_is_missing() {
        assert!(
            memory_provider_epub()
                .embedded_annotations()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn embedded_annotations_load_annotation_set_and_meta_inf_resources() {
        let mut provider = memory_provider();
        provider
            .insert(
                "META-INF/annotations.json",
                annotation_json("Audio", "audio/note.mp3"),
            )
            .unwrap();
        provider
            .insert("META-INF/audio/note.mp3", b"audio".to_vec())
            .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        let annotations = epub.embedded_annotations().unwrap().unwrap();
        assert_eq!(annotations.set().items().len(), 1);
        assert_eq!(annotations.resources().len(), 1);
        assert_eq!(annotations.resources()[0].path(), "audio/note.mp3");
        assert_eq!(annotations.resources()[0].bytes(), b"audio");
    }

    #[test]
    fn embedded_annotations_reject_missing_meta_inf_body_resource() {
        let mut provider = memory_provider();
        provider
            .insert(
                "META-INF/annotations.json",
                annotation_json("Image", "images/missing.png"),
            )
            .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        assert!(matches!(
            epub.embedded_annotations().unwrap_err(),
            EmbeddedAnnotationsError::Bundle {
                source: AnnotationBundleError::MissingResource { path }
            } if path == "images/missing.png"
        ));
    }

    #[test]
    fn embedded_annotations_reject_ocf_control_resources() {
        let mut provider = memory_provider();
        provider
            .insert(
                "META-INF/annotations.json",
                annotation_json("Audio", "container.xml"),
            )
            .unwrap();
        provider
            .insert("META-INF/container.xml", b"not annotation data".to_vec())
            .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        assert!(matches!(
            epub.embedded_annotations(),
            Err(EmbeddedAnnotationsError::Bundle {
                source: AnnotationBundleError::ReservedPath { path }
            }) if path == "container.xml"
        ));
    }

    #[test]
    fn embedded_annotations_bound_json_before_parsing() {
        let mut provider = memory_provider();
        provider
            .insert(
                "META-INF/annotations.json",
                vec![b' '; MAX_ANNOTATIONS_JSON_BYTES as usize + 1],
            )
            .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        assert!(matches!(
            epub.embedded_annotations(),
            Err(EmbeddedAnnotationsError::Bundle {
                source: AnnotationBundleError::AnnotationsJsonTooLarge { .. }
            })
        ));
    }

    #[test]
    fn embedded_annotations_use_staged_resource_changes() {
        let mut epub = memory_provider_epub();
        epub.resource_changes.upsert(
            EpubPath::new("META-INF/annotations.json").unwrap(),
            annotation_json("Audio", "audio/note.mp3"),
        );
        epub.resource_changes.upsert(
            EpubPath::new("META-INF/audio/note.mp3").unwrap(),
            b"changed audio".to_vec(),
        );

        let annotations = epub.embedded_annotations().unwrap().unwrap();
        assert_eq!(annotations.resources().len(), 1);
        assert_eq!(annotations.resources()[0].bytes(), b"changed audio");
    }

    #[test]
    fn embedded_annotations_recover_invalid_body_resource_path() {
        let mut provider = memory_provider();
        provider
            .insert(
                "META-INF/annotations.json",
                annotation_json("Image", "../bad.png"),
            )
            .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        let annotations = epub.embedded_annotations().unwrap().unwrap();
        assert!(annotations.set().items()[0].body().unwrap().id().is_none());
    }
}
