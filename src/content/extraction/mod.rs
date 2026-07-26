pub(crate) mod css;
pub(crate) mod svg;
pub(crate) mod xhtml;

pub(crate) use xhtml::{
    XhtmlExtraction, XhtmlLinkAssociations, parse_xhtml_document_from_reader_counted,
};
