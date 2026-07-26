use super::{Heading, NavigationDocument, NavigationList, NavigationPoint};
use crate::resource::{
    AuthoredHref, EpubHref, EpubPath, ParsedHref, parse_href, resolve_local_href_from_source,
};
use crate::string::EpubString;
use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
};
use std::path::{Component, Path, PathBuf};

const HTML: &str = "html";
const HEAD: &str = "head";
const TITLE: &str = "title";
const BODY: &str = "body";
const NAV: &str = "nav";
const OL: &str = "ol";
const LI: &str = "li";
const A: &str = "a";
const SPAN: &str = "span";
const HREF: &str = "href";
const HIDDEN: &str = "hidden";
const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const OPS_NS: &str = "http://www.idpf.org/2007/ops";

type GenerateResult<T> = Result<T, NavigationGenerateError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum NavigationGenerateError {
    #[error(
        "Cannot rebase same-document navigation href {href:?} from {source_path} to {output_path}"
    )]
    CannotRebaseSameDocumentHref {
        href: String,
        source_path: EpubPath,
        output_path: EpubPath,
    },
    #[error("Rebasing navigation href {href:?} produced invalid href {rebased:?}")]
    InvalidRebasedHref { href: String, rebased: String },
    #[error("XML error: {source}")]
    Xml {
        #[from]
        source: quick_xml::Error,
    },
    #[error("IO error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    #[error("UTF-8 encoding error: {source}")]
    Encoding {
        #[from]
        source: std::string::FromUtf8Error,
    },
}

impl NavigationDocument {
    pub(crate) fn generate_epub_nav_xhtml(
        &self,
        output_path: &EpubPath,
        title: &EpubString,
    ) -> GenerateResult<String> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 4);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        let mut html = BytesStart::new(HTML);
        html.push_attribute(("xmlns", XHTML_NS));
        html.push_attribute(("xmlns:epub", OPS_NS));
        writer.write_event(Event::Start(html))?;
        writer.write_event(Event::Start(BytesStart::new(HEAD)))?;
        writer.write_event(Event::Start(BytesStart::new(TITLE)))?;
        writer.write_event(Event::Text(BytesText::new(title.as_str())))?;
        writer.write_event(Event::End(BytesEnd::new(TITLE)))?;
        writer.write_event(Event::End(BytesEnd::new(HEAD)))?;
        writer.write_event(Event::Start(BytesStart::new(BODY)))?;
        self.lists().iter().try_for_each(|list| {
            write_navigation_list(&mut writer, list, self.path(), output_path)
        })?;
        writer.write_event(Event::End(BytesEnd::new(BODY)))?;
        writer.write_event(Event::End(BytesEnd::new(HTML)))?;
        Ok(String::from_utf8(writer.into_inner())?)
    }
}

fn write_navigation_list(
    writer: &mut Writer<Vec<u8>>,
    nav: &NavigationList,
    source_path: &EpubPath,
    output_path: &EpubPath,
) -> GenerateResult<()> {
    let mut element = BytesStart::new(NAV);
    let epub_type = nav.epub_type_token();
    if let Some(value) = epub_type.as_deref() {
        element.push_attribute(("epub:type", value));
    }
    if nav.hidden() {
        element.push_attribute((HIDDEN, HIDDEN));
    }
    writer.write_event(Event::Start(element))?;
    if let Some(heading) = nav.heading() {
        write_heading(writer, heading)?;
    }
    if !nav.points().is_empty() {
        writer.write_event(Event::Start(BytesStart::new(OL)))?;
        nav.points().iter().try_for_each(|point| {
            write_navigation_point(writer, point, source_path, output_path)
        })?;
        writer.write_event(Event::End(BytesEnd::new(OL)))?;
    }
    writer.write_event(Event::End(BytesEnd::new(NAV)))?;
    Ok(())
}

fn write_navigation_point(
    writer: &mut Writer<Vec<u8>>,
    point: &NavigationPoint,
    source_path: &EpubPath,
    output_path: &EpubPath,
) -> GenerateResult<()> {
    let mut li = BytesStart::new(LI);
    let epub_type = point.semantic().map(|value| value.to_string());
    if point.hidden() {
        li.push_attribute((HIDDEN, HIDDEN));
    }
    writer.write_event(Event::Start(li))?;
    match (point.authored_href(), point.label()) {
        (Some(authored_href), label) => {
            let mut element = BytesStart::new(A);
            let href = match point.href() {
                Some(href) => rebase_href(&href, source_path, output_path)?.to_string(),
                None => authored_href.to_string(),
            };
            element.push_attribute((HREF, href.as_str()));
            if let Some(value) = epub_type.as_deref() {
                element.push_attribute(("epub:type", value));
            }
            writer.write_event(Event::Start(element))?;
            if let Some(text) = label {
                writer.write_event(Event::Text(BytesText::new(text.as_str())))?;
            }
            writer.write_event(Event::End(BytesEnd::new(A)))?;
        }
        (None, Some(text)) => {
            let mut element = BytesStart::new(SPAN);
            if let Some(value) = epub_type.as_deref() {
                element.push_attribute(("epub:type", value));
            }
            writer.write_event(Event::Start(element))?;
            writer.write_event(Event::Text(BytesText::new(text.as_str())))?;
            writer.write_event(Event::End(BytesEnd::new(SPAN)))?;
        }
        _ => {}
    }
    if !point.children().is_empty() {
        writer.write_event(Event::Start(BytesStart::new(OL)))?;
        point.children().iter().try_for_each(|child| {
            write_navigation_point(writer, child, source_path, output_path)
        })?;
        writer.write_event(Event::End(BytesEnd::new(OL)))?;
    }
    writer.write_event(Event::End(BytesEnd::new(LI)))?;
    Ok(())
}

fn rebase_href(
    href: &EpubHref,
    source_path: &EpubPath,
    output_path: &EpubPath,
) -> GenerateResult<EpubHref> {
    if source_path == output_path {
        return Ok(href.clone());
    }
    let parsed = parse_href(AuthoredHref::from(href));
    if matches!(parsed, ParsedHref::SameDocument { .. }) {
        return Err(NavigationGenerateError::CannotRebaseSameDocumentHref {
            href: href.to_string(),
            source_path: source_path.clone(),
            output_path: output_path.clone(),
        });
    }
    if !matches!(parsed, ParsedHref::Local { .. }) {
        return Ok(href.clone());
    }
    if resolve_local_href_from_source(&AuthoredHref::from(href), source_path)
        .is_some_and(|(target, _)| target == *source_path)
    {
        return Err(NavigationGenerateError::CannotRebaseSameDocumentHref {
            href: href.to_string(),
            source_path: source_path.clone(),
            output_path: output_path.clone(),
        });
    }
    let original = href.as_str();
    let suffix_start = original.find(['?', '#']).unwrap_or(original.len());
    let (authored_path, suffix) = original.split_at(suffix_start);
    let source_parent = source_path
        .as_path()
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let output_parent = output_path
        .as_path()
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let absolute_target = if authored_path.is_empty() {
        normalize_path_buf(source_path.as_path().to_path_buf())
    } else {
        normalize_path_buf(source_parent.join(authored_path))
    };
    if absolute_target == normalize_path_buf(source_path.as_path().to_path_buf()) {
        return Err(NavigationGenerateError::CannotRebaseSameDocumentHref {
            href: href.to_string(),
            source_path: source_path.clone(),
            output_path: output_path.clone(),
        });
    }
    let relative = relative_path_between(output_parent, &absolute_target);
    let mut value = relative.to_string_lossy().replace('\\', "/");
    if value.is_empty() {
        value.push('.');
    }
    value.push_str(suffix);
    EpubHref::try_new(&value).map_err(|_| NavigationGenerateError::InvalidRebasedHref {
        href: href.to_string(),
        rebased: value,
    })
}

fn normalize_path_buf(path: PathBuf) -> PathBuf {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    parts.push("..".to_string());
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.into_iter().collect()
}

fn relative_path_between(from_dir: &Path, to_path: &Path) -> PathBuf {
    let from = path_parts(from_dir);
    let to = path_parts(to_path);
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for part in &to[common..] {
        result.push(part);
    }
    result
}

fn path_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

fn write_heading(writer: &mut Writer<Vec<u8>>, heading: &Heading) -> GenerateResult<()> {
    let tag = match heading.level().get() {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        6 => "h6",
        _ => unreachable!("HeadingLevel accepts only 1 through 6"),
    };
    writer.write_event(Event::Start(BytesStart::new(tag)))?;
    writer.write_event(Event::Text(BytesText::new(heading.text().as_str())))?;
    writer.write_event(Event::End(BytesEnd::new(tag)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        navigation::{NavigationList, NavigationPoint, parse::epub_nav},
        semantics::EpubStructuralSemantic,
        string::EpubString,
    };

    #[test]
    fn generation_round_trips_and_rebases_local_hrefs() {
        let source = EpubPath::new("EPUB/text/navigation/nav.xhtml").unwrap();
        let output = EpubPath::new("EPUB/generated/nav.xhtml").unwrap();
        let xml = r##"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol>
            <li><a href="../../images/cover.xhtml">Parent</a></li>
            <li><a href="chapter.xhtml?mode=full&amp;lang=en">Query</a></li>
            <li><a href="chapter.xhtml#section-1">Fragment</a></li>
            <li><a href="../My%20Book/chapter%201.xhtml">Encoded components</a></li>
            <li><a href="chapter%20two.xhtml?mode=%20wide&amp;name=%E6%9C%AC#caf%C3%A9%20note">Encoded suffixes</a></li>
            <li><a href="../%E6%9C%AC/%E7%AB%A0.xhtml">Unicode path</a></li>
            <li><a href="">Empty</a></li>
        </ol></nav></body></html>"##;
        let document = epub_nav(source, xml).unwrap();
        let generated = document
            .generate_epub_nav_xhtml(&output, &EpubString::new("Contents").unwrap())
            .unwrap();
        let reparsed = epub_nav(output, &generated).unwrap();
        assert_eq!(
            reparsed.lists()[0]
                .points()
                .iter()
                .map(|point| point.authored_href().map(AuthoredHref::as_str))
                .collect::<Vec<_>>(),
            vec![
                Some("../images/cover.xhtml"),
                Some("../text/navigation/chapter.xhtml?mode=full&lang=en"),
                Some("../text/navigation/chapter.xhtml#section-1"),
                Some("../text/My%20Book/chapter%201.xhtml"),
                Some(
                    "../text/navigation/chapter%20two.xhtml?mode=%20wide&name=%E6%9C%AC#caf%C3%A9%20note"
                ),
                Some("../text/%E6%9C%AC/%E7%AB%A0.xhtml"),
                Some("")
            ]
        );
    }

    #[test]
    fn generation_uses_dot_for_empty_rebased_directory_paths() {
        let source = EpubPath::new("EPUB/source.xhtml").unwrap();
        let output = EpubPath::new("EPUB/output.xhtml").unwrap();
        assert_eq!(
            rebase_href(&EpubHref::try_new(".").unwrap(), &source, &output)
                .unwrap()
                .as_str(),
            "."
        );
        assert_eq!(
            rebase_href(&EpubHref::try_new("./").unwrap(), &source, &output)
                .unwrap()
                .as_str(),
            "."
        );
    }

    #[test]
    fn moving_rejects_same_document_targets_but_same_path_preserves_them() {
        let source = EpubPath::new("EPUB/source.xhtml").unwrap();
        let output = EpubPath::new("EPUB/output.xhtml").unwrap();
        for href in ["#fragment", "?query", "?query#fragment"] {
            let xml = format!(
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol><li><a href="{href}">Source</a></li></ol></nav></body></html>"#
            );
            let document = epub_nav(source.clone(), &xml).unwrap();
            assert!(matches!(
                document.generate_epub_nav_xhtml(&output, &EpubString::new("Contents").unwrap()),
                Err(NavigationGenerateError::CannotRebaseSameDocumentHref { .. })
            ));
            let generated = document
                .generate_epub_nav_xhtml(&source, &EpubString::new("Contents").unwrap())
                .unwrap();
            assert_eq!(
                epub_nav(source.clone(), &generated).unwrap().lists()[0].points()[0]
                    .authored_href()
                    .map(AuthoredHref::as_str),
                Some(href)
            );
        }
    }

    #[test]
    fn generated_span_semantics_and_auxiliary_lists_round_trip() {
        let point = NavigationPoint::builder()
            .label(EpubString::new("Chapter").unwrap())
            .semantic(EpubStructuralSemantic::Chapter)
            .build()
            .unwrap();
        let list = NavigationList::builder()
            .semantic(EpubStructuralSemantic::Loi)
            .points(vec![point])
            .build()
            .unwrap();
        let document = NavigationDocument::builder()
            .path(EpubPath::new("EPUB/nav.xhtml").unwrap())
            .lists(vec![list])
            .build()
            .unwrap();
        let generated = document
            .generate_epub_nav_xhtml(document.path(), &EpubString::new("Contents").unwrap())
            .unwrap();
        let reparsed = epub_nav(document.path().clone(), &generated).unwrap();
        assert_eq!(
            reparsed.lists()[0].semantic(),
            Some(EpubStructuralSemantic::Loi)
        );
        assert_eq!(
            reparsed.lists()[0].points()[0].semantic(),
            Some(EpubStructuralSemantic::Chapter)
        );
        assert!(
            reparsed.lists()[0].points()[0]
                .authored_semantic_tokens()
                .iter()
                .all(|token| token.raw() != "doc-chapter")
        );
    }
}
