//! Choose whether a staged annotation removal also removes referenced body resources.

/// Chooses whether removing an embedded annotation set also removes its referenced resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedAnnotationResourceRemoval {
    /// Remove only `META-INF/annotations.json`, leaving referenced resources in place.
    SetOnly,
    /// Remove the annotation set and every referenced embedded annotation resource.
    SetAndReferencedResources,
}
