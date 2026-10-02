use epub_stack::accessibility::{AccessibilityObservation, AccessibilityObservationRef};
use epub_stack::analysis::AnalysisLimits;
use epub_stack::analysis::coverage::{Completeness, RelationshipSource};
use epub_stack::analysis::inspection::InspectionData;
use epub_stack::analysis::reference::{
    AuthoredReference, HrefReference, HrefRole, HrefTarget, ReferenceContext,
};
use epub_stack::content::text::TextRole;
use epub_stack::content::{
    ContentFacts, FormFact, FragmentFact, MediaFact, ScriptFact, StructureFact, StructureRole,
    XhtmlFacts,
};
use epub_stack::{EpubZip, PublicationAnalysis};

const MAX_FACTS_PER_CATEGORY: usize = 8;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: analysis <book.epub>")?;
    let zip = EpubZip::open(path)?;
    let book = zip.default_rendition()?;
    let analysis = book.analyze_with_limits(AnalysisLimits::default());

    println!("Resources: {}", analysis.resources().resources().len());
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
    for resource_facts in analysis.analyzed_resources() {
        let Some(facts) = resource_facts
            .content()
            .value()
            .and_then(ContentFacts::as_xhtml)
        else {
            continue;
        };
        found = true;
        let key = resource_facts.resource().ordinal();
        let resource = resource_facts.resource();
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
                        if resource.ordinal() == key
                )
            })
            .count();
        println!("    accessibility observations: {accessibility_count}");

        let references = analysis
            .resource(key)
            .expect("resource belongs to this analysis")
            .references()
            .filter(|reference| matches!(reference.context(), ReferenceContext::Element(_)))
            .collect::<Vec<_>>();
        if references.is_empty() {
            match state {
                Some(Completeness::Complete) => println!("    references: none"),
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
        "{indent}summary: text={} code points / {} spans, fragments={}, viewports={}, structure={}, media={}, forms={}, scripts={}",
        facts.text_stream().code_point_len(),
        facts.text_stream().spans().len(),
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

    if !facts.text_stream().text().is_empty() {
        println!("{indent}text spans:");
        for chunk in facts.text_stream().spans().take(MAX_FACTS_PER_CATEGORY) {
            let text = compact_text(chunk.text());
            let range = format!("{}..{}", chunk.range().start(), chunk.range().end());
            println!(
                "{indent}  {} {range} fragment={:?} lang={:?} dir={:?}: {:?}",
                text_role_label(chunk.role()),
                chunk.origin().fragment().map(FragmentFact::id),
                chunk.origin().lang(),
                chunk.origin().dir(),
                text,
            );
        }
        print_remaining(
            facts.text_stream().spans().len(),
            MAX_FACTS_PER_CATEGORY,
            indent,
            "text spans",
        );
    }

    if !facts.structure().is_empty() {
        println!("{indent}structure:");
        for fact in facts.structure().iter().take(MAX_FACTS_PER_CATEGORY) {
            let semantics = fact
                .semantics()
                .iter()
                .map(|token| token.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{indent}  {} fragment={:?} label={:?} semantics=[{}]",
                structure_label(fact),
                fact.fragment().map(FragmentFact::id),
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
) -> Option<Completeness> {
    analysis
        .coverage()
        .relationships()
        .iter()
        .find(|coverage| coverage.source == source)
        .map(|coverage| coverage.completeness)
}

fn text_role_label(role: TextRole) -> String {
    match role {
        TextRole::Element => "element".to_string(),
        TextRole::Body => "body".to_string(),
        TextRole::Heading { level } => format!("heading h{}", level.get()),
        TextRole::Pagebreak => "page break".to_string(),
        TextRole::FigureCaption => "figure caption".to_string(),
        TextRole::TableCaption => "table caption".to_string(),
    }
}

fn structure_label(fact: &StructureFact) -> String {
    match fact.role() {
        StructureRole::Heading(level) => format!("heading h{}", level.get()),
        StructureRole::Pagebreak => "page break".to_string(),
        StructureRole::Figure => "figure".to_string(),
        StructureRole::Table => "table".to_string(),
        StructureRole::Footnote => "footnote".to_string(),
        StructureRole::Endnote => "endnote".to_string(),
        StructureRole::Note => "note".to_string(),
        StructureRole::NavigationList => "navigation list".to_string(),
        StructureRole::PublicationSection => "publication section".to_string(),
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
    for facts in analysis.analyzed_resources() {
        if !matches!(facts.content().value(), Some(ContentFacts::Css)) {
            continue;
        }
        found = true;
        let resource = facts.resource();
        let state = relationship_state(
            analysis,
            RelationshipSource::Css(facts.resource().ordinal()),
        );
        println!(
            "  {} [{}]",
            resource.address().display_value(),
            state.map_or("not analyzed".to_string(), coverage_label)
        );
        let references = facts
            .references()
            .filter(|reference| matches!(reference.context(), ReferenceContext::Css(_)))
            .collect::<Vec<_>>();
        if references.is_empty() {
            match state {
                Some(Completeness::Complete) => println!("    dependencies: none"),
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
    for resource_facts in analysis.analyzed_resources() {
        let Some(facts) = resource_facts
            .content()
            .value()
            .and_then(ContentFacts::as_svg)
        else {
            continue;
        };
        found = true;
        let key = resource_facts.resource().ordinal();
        let resource = resource_facts.resource();
        println!("  {}", resource.address().display_value());

        if let Some(inspection) = analysis
            .resource(key)
            .expect("resource belongs to this analysis")
            .inspection()
            .value()
            && let InspectionData::Svg(svg) = inspection.data()
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
                    script.element(),
                    script.is_executable(),
                    script.fragment().map(FragmentFact::id),
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
            .resource(key)
            .expect("resource belongs to this analysis")
            .references()
            .filter(|reference| matches!(reference.context(), ReferenceContext::Element(_)))
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
            if source.ordinal() != key {
                continue;
            }
            match fact.observation() {
                AccessibilityObservation::SvgTitle { value, .. } if fact.element() != "svg" => {
                    println!(
                        "    nested title for <{}>#{:?}: {value:?}",
                        fact.element(),
                        fact.fragment().map(FragmentFact::id),
                    );
                }
                AccessibilityObservation::SvgDescription { value, .. }
                    if fact.element() != "svg" =>
                {
                    println!(
                        "    nested description for <{}>#{:?}: {value:?}",
                        fact.element(),
                        fact.fragment().map(FragmentFact::id),
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
        ReferenceContext::Element(context) => {
            format!("<{}>[{}]", context.element(), context.attribute())
        }
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

fn coverage_label(state: Completeness) -> String {
    match state {
        Completeness::Complete => "complete".to_string(),
        Completeness::Partial(issue) => format!("partial: {issue:?}"),
        Completeness::Unavailable(issue) => format!("unavailable: {issue:?}"),
    }
}

fn print_coverage(analysis: &PublicationAnalysis) {
    let coverage = analysis.coverage();
    let relationships_complete = coverage
        .relationships()
        .iter()
        .all(|entry| entry.completeness.is_complete());
    println!(
        "\nCoverage complete: relationships={relationships_complete}, fragments={}, content={}, inspection={}, fingerprints={}",
        coverage.fragments().is_complete(),
        coverage.content().is_complete(),
        coverage.inspection().is_complete(),
        coverage.fingerprints().is_complete()
    );
}
