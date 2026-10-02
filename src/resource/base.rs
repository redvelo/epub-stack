use super::{AuthoredHref, EpubPath, InvalidHref, ParsedHref, ResolvedHref, href, parse_href};

/// Resolves `declared` against the authored `<base>` values of `source_path`.
///
/// An authored base the href cannot be joined against makes the reference invalid; the base is
/// never silently dropped.
pub(crate) fn resolve_href_with_bases(
    source_path: &EpubPath,
    authored_bases: &[&AuthoredHref],
    declared: &AuthoredHref,
) -> Result<ResolvedHref, InvalidHref> {
    const LOCAL_ORIGIN: &str = "analysis.invalid";
    const LOCAL_ROOT: &str = "/__epub_root__/";
    if authored_bases
        .iter()
        .copied()
        .chain(std::iter::once(declared))
        .any(|href| matches!(parse_href(href.clone()), ParsedHref::Invalid { .. }))
    {
        return Err(InvalidHref::new(declared.clone()));
    }
    let invalid = || InvalidHref::new(declared.clone());
    let mut document =
        url::Url::parse(&format!("https://{LOCAL_ORIGIN}/")).map_err(|_| invalid())?;
    document.set_path(&format!("{LOCAL_ROOT}{}", source_path.as_str()));
    let bases_are_relative = authored_bases
        .iter()
        .all(|base| is_relative_url_reference(base));
    let target_is_relative = is_relative_url_reference(declared);
    let mut base = document;
    for authored_base in authored_bases {
        base = base.join(authored_base.as_str()).map_err(|_| invalid())?;
    }
    let target = base.join(declared.as_str()).map_err(|_| invalid())?;
    if bases_are_relative
        && target_is_relative
        && target.scheme() == "https"
        && target.host_str() == Some(LOCAL_ORIGIN)
    {
        let Some(path) = target.path().strip_prefix(LOCAL_ROOT) else {
            return Err(invalid());
        };
        let mut href = path.to_string();
        if let Some(query) = target.query() {
            href.push('?');
            href.push_str(query);
        }
        if let Some(fragment) = target.fragment() {
            href.push('#');
            href.push_str(fragment);
        }
        let root = EpubPath::new("__analysis_root__.xhtml").expect("constant path is valid");
        href::resolve_href(&AuthoredHref::new(href), &root).map_err(|_| invalid())
    } else {
        href::resolve_href(&AuthoredHref::new(target.as_str()), source_path).map_err(|_| invalid())
    }
}

fn is_relative_url_reference(href: &AuthoredHref) -> bool {
    !href.as_str().starts_with("//") && url::Url::parse(href.as_str()).is_err()
}
