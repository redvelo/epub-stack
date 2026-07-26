use crate::{
    annotation::{AnnotationBundleError, AnnotationError},
    container::ContainerError,
    media_overlay::SmilError,
    navigation::{
        NavigationDepthError, NavigationGenerateError, NavigationXhtmlError,
        parse::NavigationParseError,
    },
    package::PackageError,
    resource::provider::{ProviderReadError, ResourceProviderIndexError},
    resource::{EpubHrefError, ResourceLookupError},
};

pub type Result<T, E = EpubError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    #[error("Resource address is not local: {address}")]
    NonLocalResource { address: String },
    #[error(transparent)]
    NavigationParse(#[from] NavigationParseError),
    #[error(transparent)]
    NavigationDepth(#[from] NavigationDepthError),
    #[error(transparent)]
    NavigationXhtml(#[from] NavigationXhtmlError),
    #[error("Navigation generation failed: {message}")]
    NavigationGeneration { message: String },
    #[error(transparent)]
    Container(#[from] ContainerError),
    #[error(transparent)]
    ProviderRead(#[from] ProviderReadError),
    #[error(transparent)]
    ProviderIndex(#[from] ResourceProviderIndexError),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Smil(#[from] SmilError),
    #[error(transparent)]
    Resource(#[from] ResourceLookupError),
    #[error(transparent)]
    Annotation(#[from] AnnotationError),
    #[error(transparent)]
    AnnotationBundle(#[from] AnnotationBundleError),
    #[error(transparent)]
    Href(#[from] EpubHrefError),
}

impl From<NavigationGenerateError> for EpubError {
    fn from(source: NavigationGenerateError) -> Self {
        Self::NavigationGeneration {
            message: source.to_string(),
        }
    }
}
