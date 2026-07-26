use epub_stack::accessibility::{AccessibilityFact, AccessibilityObservationRef};
use epub_stack::analysis::coverage::{CoverageState, RelationshipSource};
use epub_stack::analysis::inspection::InspectionKind;
use epub_stack::analysis::reference::{
    AuthoredReference, HrefReference, HrefRole, HrefTarget, ReferenceContext,
};
use epub_stack::analysis::{ResourceClassification, SemanticFormat};
use epub_stack::content::text::TextChunkKind;
use epub_stack::content::{
    ContentFacts, FormFact, MediaFact, ScriptFact, StructureFact, XhtmlFacts,
};
use epub_stack::{AnalysisLimits, EpubZip, PublicationAnalysis};

const MAX_FACTS_PER_CATEGORY: usize = 8;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: analysis <book.epub>")?;
    let zip = EpubZip::open(path)?;
    let book = zip.default_rendition()?;
    let analysis = book.analyze_with_limits(AnalysisLimits::default());

    println!("Resources: {}", analysis.resources().len());
    print_broken_references(&analysis);
    print_xhtml_resources(&analysis)?;
    print_stylesheets(&analysis)?;
    print_svg_resources(&analysis)?;
    print_coverage(&analysis);

    Ok(())
}

fn print_xhtml_resources(analysis: &PublicationAnalysis) -> Result<(), Box<dyn std::error::Error>> {
    println!("\nXHTML resources");
    let mut found = false;
    for resource_facts in analysis.resource_facts() {
        let Some(ContentFacts::Xhtml(facts)) = resource_facts.content().value() else {
            continue;
        };
        found = true;
        let key = resource_facts.resource();
        let resource = analysis.resources().resource(key)?;
        let state = relationship_state(analysis, RelationshipSource::Xhtml(key));
        println!(
            "  {} [{}]",
            resource.address().display_value(),
            state.map_or("not analyzed".to_string(), coverage_label)
        );
        print_xhtml_facts(facts, "    ");

        let accessibility_count = analysis
            .accessibility_observations()
            .filter(|observation| {
                matches!(
                    observation,
                    AccessibilityObservationRef::Content { resource, .. }
                        if resource.key() == key
                )
            })
            .count();
        println!("    accessibility observations: {accessibility_count}");

        let references = analysis
            .references_from_resource(key)?
            .filter(|reference| matches!(reference.context(), ReferenceContext::Xhtml(_)))
            .collect::<Vec<_>>();
        if references.is_empty() {
            match state {
                Some(CoverageState::Complete) => println!("    references: none"),
                Some(_) => println!("    references: none recovered (coverage incomplete)"),
                None => println!("    references: not analyzed"),
            }
        } else {
            println!("    references: {}", references.len());
            for reference in references.iter().take(MAX_FACTS_PER_CATEGORY) {
                print_href(analysis, reference, "      ");
            }
            print_remaining(
                references.len(),
                MAX_FACTS_PER_CATEGORY,
                "      ",
                "references",
            );
        }
    }
    if !found {
        println!("  none");
    }
    Ok(())
}

fn print_xhtml_facts(facts: &XhtmlFacts, indent: &str) {
    println!(
        "{indent}summary: text={} code points / {} chunks, fragments={}, viewports={}, structure={}, media={}, forms={}, scripts={}",
        facts.text_stream().code_point_len(),
        facts.text().len(),
        facts.fragments().len(),
        facts.viewports().len(),
        facts.structure().len(),
        facts.media().len(),
        facts.forms().len(),
        facts.scripts().len(),
    );

    if !facts.viewports().is_empty() {
        println!("{indent}viewports:");
        for viewport in facts.viewports().iter().take(MAX_FACTS_PER_CATEGORY) {
            let directives = viewport
                .directives()
                .iter()
                .map(|directive| match directive.raw_value() {
                    Some(value) => format!("{}={}", directive.raw_name().trim(), value.trim()),
                    None => directive.raw_name().trim().to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("{indent}  {:?} -> [{}]", viewport.content(), directives);
        }
        print_remaining(
            facts.viewports().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "viewports",
        );
    }

    if !facts.text().is_empty() {
        println!("{indent}text chunks:");
        for chunk in facts.text().iter().take(MAX_FACTS_PER_CATEGORY) {
            let text = chunk
                .text(facts.text_stream())
                .map(compact_text)
                .unwrap_or_else(|_| "<range unavailable>".to_string());
            let range = chunk
                .stream_range()
                .map(|range| format!("{}..{}", range.start(), range.end()))
                .unwrap_or_else(|| "owned".to_string());
            println!(
                "{indent}  {} {range} fragment={:?} lang={:?} dir={:?}: {:?}",
                text_kind_label(chunk.kind()),
                chunk.fragment(),
                chunk.lang(),
                chunk.dir(),
                text,
            );
        }
        print_remaining(
            facts.text().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "text chunks",
        );
    }

    if !facts.structure().is_empty() {
        println!("{indent}structure:");
        for fact in facts.structure().iter().take(MAX_FACTS_PER_CATEGORY) {
            let semantics = fact
                .semantics()
                .iter()
                .map(|token| token.raw())
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{indent}  {} fragment={:?} label={:?} semantics=[{}]",
                structure_label(fact),
                fact.fragment(),
                fact.label().map(compact_text),
                semantics,
            );
        }
        print_remaining(
            facts.structure().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "structure facts",
        );
    }

    if !facts.fragments().is_empty() {
        println!("{indent}fragments:");
        for fragment in facts.fragments().iter().take(MAX_FACTS_PER_CATEGORY) {
            println!(
                "{indent}  #{} on <{}> via {:?}",
                fragment.id(),
                fragment.element(),
                fragment.attribute(),
            );
        }
        print_remaining(
            facts.fragments().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "fragments",
        );
    }

    if !facts.media().is_empty() {
        println!("{indent}media:");
        for fact in facts.media().iter().take(MAX_FACTS_PER_CATEGORY) {
            println!("{indent}  {}", media_label(fact));
        }
        print_remaining(
            facts.media().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "media facts",
        );
    }

    if !facts.forms().is_empty() {
        println!("{indent}forms:");
        for fact in facts.forms().iter().take(MAX_FACTS_PER_CATEGORY) {
            println!("{indent}  {}", form_label(fact));
        }
        print_remaining(
            facts.forms().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "form facts",
        );
    }

    if !facts.scripts().is_empty() {
        println!("{indent}scripts:");
        for fact in facts.scripts().iter().take(MAX_FACTS_PER_CATEGORY) {
            println!("{indent}  {}", script_label(fact));
        }
        print_remaining(
            facts.scripts().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "script facts",
        );
    }
}

fn relationship_state(
    analysis: &PublicationAnalysis,
    source: RelationshipSource,
) -> Option<&CoverageState> {
    analysis
        .coverage()
        .relationships()
        .iter()
        .find(|coverage| *coverage.source() == source)
        .map(|coverage| coverage.state())
}

fn text_kind_label(kind: TextChunkKind) -> String {
    match kind {
        TextChunkKind::Body => "body".to_string(),
        TextChunkKind::Heading { level } => format!("heading h{}", level.get()),
        TextChunkKind::PagebreakLabel => "page-break label".to_string(),
        TextChunkKind::FigureCaption => "figure caption".to_string(),
        TextChunkKind::TableCaption => "table caption".to_string(),
        TextChunkKind::AltText => "alternative text".to_string(),
    }
}

fn structure_label(fact: &StructureFact) -> String {
    match fact {
        StructureFact::Heading { level, .. } => format!("heading h{}", level.get()),
        StructureFact::Pagebreak { .. } => "page break".to_string(),
        StructureFact::Figure { .. } => "figure".to_string(),
        StructureFact::Table { .. } => "table".to_string(),
        StructureFact::Footnote { .. } => "footnote".to_string(),
        StructureFact::Endnote { .. } => "endnote".to_string(),
        StructureFact::Note { .. } => "note".to_string(),
        StructureFact::NavigationList { .. } => "navigation list".to_string(),
        StructureFact::PublicationSection { .. } => "publication section".to_string(),
    }
}

fn media_label(fact: &MediaFact) -> String {
    match fact {
        MediaFact::Image { element, alt, .. } => {
            format!("image <{element}> alt={alt:?}")
        }
        MediaFact::Audio => "audio".to_string(),
        MediaFact::Video => "video".to_string(),
        MediaFact::Source { context, .. } => format!("source for {context:?}"),
        MediaFact::Track {
            kind,
            srclang,
            label,
            ..
        } => format!("track kind={kind:?} lang={srclang:?} label={label:?}"),
        MediaFact::Poster => "video poster".to_string(),
    }
}

fn form_label(fact: &FormFact) -> String {
    match fact {
        FormFact::Form {
            fragment, method, ..
        } => format!("form fragment={fragment:?} method={method:?}"),
        FormFact::Control {
            element,
            fragment,
            control_type,
            name,
            value,
            ..
        } => format!(
            "control <{element}> fragment={fragment:?} type={control_type:?} name={name:?} value={value:?}"
        ),
    }
}

fn script_label(fact: &ScriptFact) -> String {
    match fact {
        ScriptFact::External {
            fragment,
            script_type,
            ..
        } => format!("external fragment={fragment:?} type={script_type:?}"),
        ScriptFact::Inline {
            fragment,
            script_type,
            has_text,
            ..
        } => format!("inline fragment={fragment:?} type={script_type:?} has_text={has_text}"),
        ScriptFact::DataBlock {
            fragment,
            script_type,
            has_text,
            ..
        } => format!("data block fragment={fragment:?} type={script_type:?} has_text={has_text}"),
        ScriptFact::EventHandler {
            element,
            fragment,
            attribute,
            ..
        } => format!("event handler <{element}>[{attribute}] fragment={fragment:?}"),
    }
}

fn compact_text(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 100 {
        normalized
    } else {
        format!("{}...", normalized.chars().take(97).collect::<String>())
    }
}

fn print_remaining(total: usize, shown: usize, indent: &str, label: &str) {
    if total > shown {
        println!("{indent}... {} more {label}", total - shown);
    }
}

fn print_broken_references(analysis: &PublicationAnalysis) {
    let broken = analysis.broken_references().collect::<Vec<_>>();
    println!("\nBroken references: {}", broken.len());
    for reference in broken {
        match reference {
            AuthoredReference::Href(reference) => print_href(analysis, reference, "  "),
            AuthoredReference::Manifest(reference) => println!(
                "  manifest {:?} {:?} -> {:?}",
                reference.role(),
                reference.declared().as_str(),
                reference.target()
            ),
        }
    }
}

fn print_stylesheets(analysis: &PublicationAnalysis) -> Result<(), Box<dyn std::error::Error>> {
    println!("\nStylesheets");
    let mut found = false;
    for facts in analysis.resource_facts() {
        if !matches!(
            facts.classification().value(),
            Some(ResourceClassification::Identified(SemanticFormat::Css))
        ) {
            continue;
        }
        found = true;
        let resource = analysis.resources().resource(facts.resource())?;
        let state = relationship_state(analysis, RelationshipSource::Css(facts.resource()));
        println!(
            "  {} [{}]",
            resource.address().display_value(),
            state.map_or("not analyzed".to_string(), coverage_label)
        );
        let references = analysis
            .references_from_resource(facts.resource())?
            .filter(|reference| matches!(reference.context(), ReferenceContext::Css(_)))
            .collect::<Vec<_>>();
        if references.is_empty() {
            match state {
                Some(CoverageState::Complete) => println!("    dependencies: none"),
                Some(_) => println!("    dependencies: none recovered (coverage incomplete)"),
                None => println!("    dependencies: not analyzed"),
            }
        } else {
            for reference in references {
                print_href(analysis, reference, "    ");
            }
        }
    }
    if !found {
        println!("  none");
    }
    Ok(())
}

fn print_svg_resources(analysis: &PublicationAnalysis) -> Result<(), Box<dyn std::error::Error>> {
    println!("\nSVG resources");
    let mut found = false;
    for resource_facts in analysis.resource_facts() {
        let Some(ContentFacts::Svg(facts)) = resource_facts.content().value() else {
            continue;
        };
        found = true;
        let key = resource_facts.resource();
        let resource = analysis.resources().resource(key)?;
        println!("  {}", resource.address().display_value());

        if let Some(inspection) = analysis.inspection_for(key)?.value()
            && let InspectionKind::SvgImage(svg) = inspection.kind()
        {
            println!(
                "    root: width={}, height={}, viewBox={}",
                svg.width().unwrap_or("not authored"),
                svg.height().unwrap_or("not authored"),
                svg.view_box().unwrap_or("not authored")
            );
            println!("    title: {:?}", svg.title().unwrap_or("not authored"));
            if let Some(description) = svg.description() {
                println!("    description: {description:?}");
            }
        }

        if !facts.text().is_empty() {
            println!("    source text:");
            for text in facts.text() {
                println!("      {:?} (fragment {:?})", text.text(), text.fragment());
            }
        }
        if !facts.fragments().is_empty() {
            println!("    fragments:");
            for fragment in facts.fragments() {
                println!(
                    "      #{} on <{}> via {:?}",
                    fragment.id(),
                    fragment.element(),
                    fragment.attribute()
                );
            }
        }
        if !facts.scripts().is_empty() {
            println!("    scripts:");
            for script in facts.scripts() {
                println!(
                    "      element=<{}>, executable={}, fragment={:?}, attribute={:?}",
                    script.element().unwrap_or("unknown"),
                    script.is_executable(),
                    script.fragment(),
                    script.attribute()
                );
            }
        }
        if !facts.foreign_objects().is_empty() {
            println!("    foreign objects:");
            for foreign in facts.foreign_objects() {
                println!("      fragment={:?}", foreign.fragment());
                print_xhtml_facts(foreign.xhtml(), "        ");
            }
        }

        let references = analysis
            .references_from_resource(key)?
            .filter(|reference| matches!(reference.context(), ReferenceContext::Svg(_)))
            .collect::<Vec<_>>();
        if !references.is_empty() {
            println!("    dependencies:");
            for reference in references {
                print_href(analysis, reference, "      ");
            }
        }

        for observation in analysis.accessibility_observations() {
            let AccessibilityObservationRef::Content {
                resource: source,
                fact,
            } = observation
            else {
                continue;
            };
            if source.key() != key {
                continue;
            }
            match fact {
                AccessibilityFact::SvgTitle(fact) if fact.subject_element() != "svg" => println!(
                    "    nested title for <{}>#{:?}: {:?}",
                    fact.subject_element(),
                    fact.subject_fragment(),
                    fact.value()
                ),
                AccessibilityFact::SvgDescription(fact) if fact.subject_element() != "svg" => {
                    println!(
                        "    nested description for <{}>#{:?}: {:?}",
                        fact.subject_element(),
                        fact.subject_fragment(),
                        fact.value()
                    );
                }
                _ => {}
            }
        }
    }
    if !found {
        println!("  none");
    }
    Ok(())
}

fn print_href(analysis: &PublicationAnalysis, reference: &HrefReference, indent: &str) {
    println!(
        "{indent}{} {} from {} -> {}",
        role_label(reference.role()),
        href_label(reference.declared().as_str()),
        context_label(reference.context()),
        target_label(analysis, reference.target())
    );
}

fn href_label(href: &str) -> String {
    if href.starts_with("data:") {
        format!("<data URL, {} characters>", href.chars().count())
    } else if href.chars().count() > 120 {
        let prefix = href.chars().take(117).collect::<String>();
        format!("{prefix:?}...")
    } else {
        format!("{href:?}")
    }
}

fn role_label(role: HrefRole) -> &'static str {
    match role {
        HrefRole::CssImport => "import",
        HrefRole::CssUrl => "URL",
        HrefRole::Font => "font",
        HrefRole::Hyperlink => "hyperlink",
        HrefRole::Stylesheet => "stylesheet",
        HrefRole::FormAction => "form action",
        HrefRole::Image => "image",
        HrefRole::Script => "script",
        HrefRole::Audio => "audio",
        HrefRole::Video => "video",
        HrefRole::Source => "media source",
        HrefRole::Track => "text track",
        HrefRole::Poster => "poster",
        HrefRole::Object => "object",
        HrefRole::Embed => "embed",
        HrefRole::Iframe => "iframe",
        HrefRole::Svg => "SVG dependency",
        _ => "reference",
    }
}

fn context_label(context: &ReferenceContext) -> String {
    match context {
        ReferenceContext::Css(context) => match (context.at_rule(), context.property()) {
            (Some(at_rule), Some(property)) => format!("@{at_rule} / {property}"),
            (Some(at_rule), None) => format!("@{at_rule}"),
            (None, Some(property)) => property.to_string(),
            (None, None) => "CSS".to_string(),
        },
        ReferenceContext::Svg(context) => {
            format!("<{}>[{}]", context.element(), context.attribute())
        }
        ReferenceContext::Xhtml(context) => {
            format!("<{}>[{}]", context.element(), context.attribute())
        }
        _ => format!("{context:?}"),
    }
}

fn target_label(analysis: &PublicationAnalysis, target: &HrefTarget) -> String {
    match target {
        HrefTarget::Resource { resource, query } => {
            let address = analysis
                .resources()
                .resource(*resource)
                .map(|resource| resource.address().display_value())
                .unwrap_or("unknown resource");
            query
                .as_ref()
                .map_or_else(|| address.to_string(), |query| format!("{address}?{query}"))
        }
        HrefTarget::Fragment {
            resource,
            query,
            fragment,
            exists,
        } => {
            let address = analysis
                .resources()
                .resource(*resource)
                .map(|resource| resource.address().display_value())
                .unwrap_or("unknown resource");
            let query = query
                .as_ref()
                .map(|query| format!("?{query}"))
                .unwrap_or_default();
            let existence = match exists {
                Some(true) => "exists",
                Some(false) => "missing fragment",
                None => "fragment existence unknown",
            };
            format!("{address}{query}#{fragment} ({existence})")
        }
        HrefTarget::Remote { href, .. } => format!("remote {href}"),
        HrefTarget::Data(_) => "embedded data URL".to_string(),
        HrefTarget::External(href) => format!("external {href}"),
        HrefTarget::MissingLocal(path) => format!("missing {}", path.as_str()),
        HrefTarget::Invalid(href) => format!("invalid {:?}", href.as_str()),
    }
}

fn coverage_label(state: &CoverageState) -> String {
    match state {
        CoverageState::Complete => "complete".to_string(),
        CoverageState::Partial(issue) => format!("partial: {issue:?}"),
        CoverageState::Unavailable(issue) => format!("unavailable: {issue:?}"),
    }
}

fn print_coverage(analysis: &PublicationAnalysis) {
    let coverage = analysis.coverage();
    let relationships_complete = coverage
        .relationships()
        .iter()
        .all(|entry| matches!(entry.state(), CoverageState::Complete));
    println!(
        "\nCoverage complete: relationships={relationships_complete}, classification={}, fragments={}, content={}, inspection={}, fingerprints={}",
        coverage.classification().is_complete(),
        coverage.fragments().is_complete(),
        coverage.content().is_complete(),
        coverage.inspection().is_complete(),
        coverage.fingerprints().is_complete()
    );
}
