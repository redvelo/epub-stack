use epub_stack::analysis::coverage::{CoverageState, ResourceCoverage};
use epub_stack::analysis::{
    AnalysisIssue, AnalysisOutcome, ForegroundPreparationEligibility, ResourceClassification,
};
use epub_stack::content::{ContentFacts, ScriptFact, SvgFacts, XhtmlFacts};
use epub_stack::{EpubZip, PublicationAnalysis};
use rayon::prelude::*;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Mode {
    Sequential,
    Parallel,
    Both,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "sequential" => Ok(Self::Sequential),
            "parallel" => Ok(Self::Parallel),
            "both" => Ok(Self::Both),
            _ => Err(format!(
                "unknown mode {value:?}; expected sequential, parallel, or both"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnalysisRecord {
    physical_resources: usize,
    manifest_declarations: usize,
    reading_order_occurrences: usize,
    references: usize,
    broken_references: usize,
    search_entries: usize,
    distinct_searchable_resources: usize,
    executable_states: ExecutableStateCounts,
    foreground_preparation: ForegroundPreparationCounts,
    content_occurrences: ContentOccurrenceCounts,
    completeness: Vec<CompletenessFamilyRecord>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ExecutableStateCounts {
    detected: usize,
    absent: usize,
    unknown: usize,
    inapplicable: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ForegroundPreparationCounts {
    eligible: usize,
    ineligible: usize,
    unknown: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ContentOccurrenceCounts {
    forms: usize,
    scripts: usize,
    keydown_handlers: usize,
    keyup_handlers: usize,
    keypress_handlers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletenessFamilyRecord {
    family: &'static str,
    expected: usize,
    complete: usize,
    partial: usize,
    unavailable: usize,
    issues: Vec<CoverageIssueRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncompleteKind {
    Partial,
    Unavailable,
}

impl fmt::Display for IncompleteKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Partial => formatter.write_str("partial"),
            Self::Unavailable => formatter.write_str("unavailable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CoverageIssueRecord {
    area: &'static str,
    kind: IncompleteKind,
    issue: AnalysisIssue,
    count: usize,
}

impl fmt::Display for CoverageIssueRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{:?}={}",
            self.area, self.kind, self.issue, self.count
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureStage {
    Container,
    Publication,
}

impl fmt::Display for FailureStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => formatter.write_str("container"),
            Self::Publication => formatter.write_str("publication"),
        }
    }
}

enum BookOutcome {
    Success {
        analysis: AnalysisRecord,
        open_elapsed: Duration,
        analysis_elapsed: Duration,
    },
    Failure {
        stage: FailureStage,
        message: String,
        elapsed: Duration,
    },
}

struct BookRecord {
    path: PathBuf,
    archive_size_bytes: Option<u64>,
    outcome: BookOutcome,
}

struct Run {
    label: &'static str,
    wall_elapsed: Duration,
    books: Vec<BookRecord>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().collect::<Vec<_>>();
    let program = arguments
        .first()
        .cloned()
        .unwrap_or_else(|| "corpus_analysis".into());
    let json_output = remove_flag(&mut arguments, "--json")?;
    let _cargo_bench_flag = remove_flag(&mut arguments, "--bench")?;
    if arguments.len() != 3 {
        return Err(usage(&program).into());
    }

    let mode = &arguments[1];
    let directory = &arguments[2];

    let mode = Mode::parse(&mode.to_string_lossy())?;
    let directory = PathBuf::from(directory);
    let paths = discover_epubs(&directory)?;
    if paths.is_empty() {
        return Err(format!("no EPUB files found directly in {}", directory.display()).into());
    }

    let threads = (!matches!(mode, Mode::Sequential)).then(rayon::current_num_threads);
    let (runs, deterministic_record_equality) = match mode {
        Mode::Sequential => (vec![run_sequential(&paths)], None),
        Mode::Parallel => (vec![run_parallel(&paths)], None),
        Mode::Both => {
            let sequential = run_sequential(&paths);
            let parallel = run_parallel(&paths);
            let identical = compare_runs(&sequential, &parallel);
            (vec![sequential, parallel], Some(identical))
        }
    };
    let mismatch = deterministic_record_equality == Some(false);

    if json_output {
        print_json_document(
            &directory,
            mode,
            threads,
            deterministic_record_equality,
            &runs,
        )?;
    } else {
        println!("corpus: {}", directory.display());
        println!("books: {}", paths.len());
        if let Some(threads) = threads {
            println!("rayon threads: {threads}");
        }
        for run in runs {
            print_run(run);
        }
        if let Some(identical) = deterministic_record_equality {
            println!(
                "correctness: {}",
                if identical { "identical" } else { "MISMATCH" }
            );
        }
    }

    if mismatch {
        Err("sequential and parallel analysis records differ".into())
    } else {
        Ok(())
    }
}

fn usage(program: &std::ffi::OsStr) -> String {
    format!(
        "usage: {} <sequential|parallel|both> <corpus-directory> [--json]",
        Path::new(program).display()
    )
}

fn remove_flag(arguments: &mut Vec<std::ffi::OsString>, flag: &str) -> Result<bool, String> {
    let positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == flag).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err(format!("flag {flag} specified more than once"));
    }
    if let Some(index) = positions.first() {
        arguments.remove(*index);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn discover_epubs(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
        {
            paths.push(path);
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

fn run_sequential(paths: &[PathBuf]) -> Run {
    let started = Instant::now();
    let books = paths.iter().map(|path| analyze_book(path)).collect();
    Run {
        label: "sequential",
        wall_elapsed: started.elapsed(),
        books,
    }
}

fn run_parallel(paths: &[PathBuf]) -> Run {
    let started = Instant::now();
    let books = paths.par_iter().map(|path| analyze_book(path)).collect();
    Run {
        label: "parallel",
        wall_elapsed: started.elapsed(),
        books,
    }
}

fn analyze_book(path: &Path) -> BookRecord {
    let archive_size_bytes = fs::metadata(path).ok().map(|metadata| metadata.len());
    let open_started = Instant::now();
    let archive = match EpubZip::open(path) {
        Ok(archive) => archive,
        Err(error) => {
            return BookRecord {
                path: path.to_owned(),
                archive_size_bytes,
                outcome: BookOutcome::Failure {
                    stage: FailureStage::Container,
                    message: error.to_string(),
                    elapsed: open_started.elapsed(),
                },
            };
        }
    };
    let publication = match archive.default_rendition() {
        Ok(publication) => publication,
        Err(error) => {
            return BookRecord {
                path: path.to_owned(),
                archive_size_bytes,
                outcome: BookOutcome::Failure {
                    stage: FailureStage::Publication,
                    message: error.to_string(),
                    elapsed: open_started.elapsed(),
                },
            };
        }
    };
    let open_elapsed = open_started.elapsed();

    let analysis_started = Instant::now();
    let analysis = publication.analyze();
    black_box(&analysis);
    let record = summarize_analysis(&analysis);
    let analysis_elapsed = analysis_started.elapsed();

    BookRecord {
        path: path.to_owned(),
        archive_size_bytes,
        outcome: BookOutcome::Success {
            analysis: record,
            open_elapsed,
            analysis_elapsed,
        },
    }
}

fn summarize_analysis(analysis: &PublicationAnalysis) -> AnalysisRecord {
    let coverage = analysis.coverage();
    let mut completeness = vec![
        summarize_resource_coverage("classification", coverage.classification()),
        summarize_resource_coverage("fragments", coverage.fragments()),
        summarize_resource_coverage("content", coverage.content()),
        summarize_resource_coverage("inspection", coverage.inspection()),
        summarize_resource_coverage("fingerprints", coverage.fingerprints()),
    ];
    let mut relationship_family = CompletenessFamilyRecord {
        family: "relationships",
        expected: coverage.relationships().len(),
        complete: 0,
        partial: 0,
        unavailable: 0,
        issues: Vec::new(),
    };
    for relationship in coverage.relationships() {
        match relationship.state() {
            CoverageState::Complete => relationship_family.complete += 1,
            CoverageState::Partial(issue) => {
                relationship_family.partial += 1;
                add_coverage_issue(
                    &mut relationship_family.issues,
                    "relationships",
                    IncompleteKind::Partial,
                    *issue,
                );
            }
            CoverageState::Unavailable(issue) => {
                relationship_family.unavailable += 1;
                add_coverage_issue(
                    &mut relationship_family.issues,
                    "relationships",
                    IncompleteKind::Unavailable,
                    *issue,
                );
            }
        }
    }
    completeness.push(relationship_family);

    let mut executable_states = ExecutableStateCounts::default();
    let mut foreground_preparation = ForegroundPreparationCounts::default();
    let mut content_occurrences = ContentOccurrenceCounts::default();
    for facts in analysis.resource_facts() {
        record_executable_state(
            &mut executable_states,
            facts.content(),
            facts.classification(),
        );
        match facts.foreground_preparation_eligibility() {
            ForegroundPreparationEligibility::Eligible => foreground_preparation.eligible += 1,
            ForegroundPreparationEligibility::Ineligible => foreground_preparation.ineligible += 1,
            ForegroundPreparationEligibility::Unknown => foreground_preparation.unknown += 1,
        }
        if let Some(content) = facts.content().value() {
            record_content_occurrences(&mut content_occurrences, content);
        }
    }

    let search_entries = analysis.search_entries().collect::<Vec<_>>();
    let distinct_searchable_resources = search_entries
        .iter()
        .map(|entry| entry.facts().resource())
        .collect::<HashSet<_>>()
        .len();

    AnalysisRecord {
        physical_resources: analysis.resources().len(),
        manifest_declarations: analysis.resources().declarations().len(),
        reading_order_occurrences: analysis.resources().reading_order().len(),
        references: analysis.references().count(),
        broken_references: analysis.broken_references().count(),
        search_entries: search_entries.len(),
        distinct_searchable_resources,
        executable_states,
        foreground_preparation,
        content_occurrences,
        completeness,
    }
}

fn summarize_resource_coverage(
    family: &'static str,
    coverage: &ResourceCoverage,
) -> CompletenessFamilyRecord {
    let mut issues = Vec::new();
    record_resource_coverage(&mut issues, family, coverage);
    CompletenessFamilyRecord {
        family,
        expected: coverage.expected().len(),
        complete: coverage.completed().len(),
        partial: coverage.partial().len(),
        unavailable: coverage.unavailable().len(),
        issues,
    }
}

fn record_executable_state(
    counts: &mut ExecutableStateCounts,
    content: &AnalysisOutcome<ContentFacts>,
    classification: &AnalysisOutcome<ResourceClassification>,
) {
    match content {
        AnalysisOutcome::Complete(content) => match content.executable_content_detected() {
            Some(true) => counts.detected += 1,
            Some(false) => counts.absent += 1,
            None => counts.inapplicable += 1,
        },
        AnalysisOutcome::Partial { value, .. } => match value.executable_content_detected() {
            Some(true) => counts.detected += 1,
            Some(false) => counts.unknown += 1,
            None => counts.inapplicable += 1,
        },
        AnalysisOutcome::Unavailable(_) => counts.unknown += 1,
        AnalysisOutcome::NotApplicable => {
            if classification.is_complete() {
                counts.inapplicable += 1;
            } else {
                counts.unknown += 1;
            }
        }
    }
}

fn record_content_occurrences(counts: &mut ContentOccurrenceCounts, content: &ContentFacts) {
    match content {
        ContentFacts::Xhtml(facts) => record_xhtml_occurrences(counts, facts),
        ContentFacts::Svg(facts) => record_svg_occurrences(counts, facts),
        ContentFacts::Smil(_) => {}
    }
}

fn record_xhtml_occurrences(counts: &mut ContentOccurrenceCounts, facts: &XhtmlFacts) {
    counts.forms += facts.forms().len();
    record_scripts(counts, facts.scripts());
}

fn record_svg_occurrences(counts: &mut ContentOccurrenceCounts, facts: &SvgFacts) {
    record_scripts(counts, facts.scripts());
    for foreign_object in facts.foreign_objects() {
        record_xhtml_occurrences(counts, foreign_object.xhtml());
    }
}

fn record_scripts(counts: &mut ContentOccurrenceCounts, scripts: &[ScriptFact]) {
    counts.scripts += scripts.len();
    for attribute in scripts.iter().filter_map(ScriptFact::attribute) {
        match attribute.to_ascii_lowercase().as_str() {
            "onkeydown" => counts.keydown_handlers += 1,
            "onkeyup" => counts.keyup_handlers += 1,
            "onkeypress" => counts.keypress_handlers += 1,
            _ => {}
        }
    }
}

fn record_resource_coverage(
    records: &mut Vec<CoverageIssueRecord>,
    area: &'static str,
    coverage: &ResourceCoverage,
) {
    for incomplete in coverage.partial() {
        add_coverage_issue(records, area, IncompleteKind::Partial, incomplete.issue());
    }
    for incomplete in coverage.unavailable() {
        add_coverage_issue(
            records,
            area,
            IncompleteKind::Unavailable,
            incomplete.issue(),
        );
    }
}

fn add_coverage_issue(
    records: &mut Vec<CoverageIssueRecord>,
    area: &'static str,
    kind: IncompleteKind,
    issue: AnalysisIssue,
) {
    if let Some(record) = records
        .iter_mut()
        .find(|record| record.area == area && record.kind == kind && record.issue == issue)
    {
        record.count += 1;
    } else {
        records.push(CoverageIssueRecord {
            area,
            kind,
            issue,
            count: 1,
        });
    }
}

fn compare_runs(left: &Run, right: &Run) -> bool {
    left.books.len() == right.books.len()
        && left
            .books
            .iter()
            .zip(&right.books)
            .all(|(left, right)| equivalent_book_records(left, right))
}

fn equivalent_book_records(left: &BookRecord, right: &BookRecord) -> bool {
    if left.path != right.path || left.archive_size_bytes != right.archive_size_bytes {
        return false;
    }
    match (&left.outcome, &right.outcome) {
        (
            BookOutcome::Success { analysis: left, .. },
            BookOutcome::Success {
                analysis: right, ..
            },
        ) => left == right,
        (
            BookOutcome::Failure {
                stage: left_stage,
                message: left_message,
                ..
            },
            BookOutcome::Failure {
                stage: right_stage,
                message: right_message,
                ..
            },
        ) => left_stage == right_stage && left_message == right_message,
        (BookOutcome::Success { .. }, BookOutcome::Failure { .. })
        | (BookOutcome::Failure { .. }, BookOutcome::Success { .. }) => false,
    }
}

fn print_json_document(
    directory: &Path,
    mode: Mode,
    rayon_threads: Option<usize>,
    deterministic_record_equality: Option<bool>,
    runs: &[Run],
) -> Result<(), Box<dyn Error>> {
    let document = json!({
        "schema": "epub-stack.corpus-analysis.v1",
        "corpus": directory,
        "requested_mode": mode_label(mode),
        "book_count": runs.first().map_or(0, |run| run.books.len()),
        "rayon_threads": rayon_threads,
        "deterministic_record_equality": deterministic_record_equality,
        "runs": runs.iter().map(run_json).collect::<Vec<_>>(),
    });
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &document)?;
    writeln!(output)?;
    Ok(())
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Sequential => "sequential",
        Mode::Parallel => "parallel",
        Mode::Both => "both",
    }
}

fn run_json(run: &Run) -> Value {
    let summary = summarize_run(run);
    json!({
        "mode": run.label,
        "wall_us": duration_us(run.wall_elapsed),
        "bytes_read": null,
        "allocator_peak_bytes": null,
        "stage_timing_us": null,
        "summary": {
            "successful_books": summary.successful_books,
            "failed_books": summary.failed_books,
            "archive_size_bytes": summary.archive_size_bytes,
            "open_wall_us_sum": duration_us(summary.open_elapsed),
            "analysis_wall_us_sum": duration_us(summary.analysis_elapsed),
            "physical_resources": summary.physical_resources,
            "manifest_declarations": summary.manifest_declarations,
            "reading_order_occurrences": summary.reading_order_occurrences,
            "references": summary.references,
            "broken_references": summary.broken_references,
            "search_entries": summary.search_entries,
            "distinct_searchable_resources": summary.distinct_searchable_resources,
            "executable_states": executable_states_json(summary.executable_states),
            "foreground_preparation": foreground_preparation_json(summary.foreground_preparation),
            "content_occurrences": content_occurrences_json(summary.content_occurrences),
            "completeness": summary.completeness.iter().map(completeness_json).collect::<Vec<_>>(),
            "retained_projection_size_estimate_bytes": null,
        },
        "books": run.books.iter().map(book_json).collect::<Vec<_>>(),
    })
}

fn book_json(book: &BookRecord) -> Value {
    match &book.outcome {
        BookOutcome::Success {
            analysis,
            open_elapsed,
            analysis_elapsed,
        } => json!({
            "path": book.path,
            "archive_size_bytes": book.archive_size_bytes,
            "status": "ok",
            "failure": null,
            "open_wall_us": duration_us(*open_elapsed),
            "analysis_wall_us": duration_us(*analysis_elapsed),
            "bytes_read": null,
            "allocator_peak_bytes": null,
            "stage_timing_us": null,
            "retained_projection_size_estimate_bytes": null,
            "analysis": analysis_json(analysis),
        }),
        BookOutcome::Failure {
            stage,
            message,
            elapsed,
        } => json!({
            "path": book.path,
            "archive_size_bytes": book.archive_size_bytes,
            "status": "error",
            "failure": { "stage": stage.to_string(), "message": message },
            "open_wall_us": duration_us(*elapsed),
            "analysis_wall_us": null,
            "bytes_read": null,
            "allocator_peak_bytes": null,
            "stage_timing_us": null,
            "retained_projection_size_estimate_bytes": null,
            "analysis": null,
        }),
    }
}

fn analysis_json(analysis: &AnalysisRecord) -> Value {
    json!({
        "physical_resources": analysis.physical_resources,
        "manifest_declarations": analysis.manifest_declarations,
        "reading_order_occurrences": analysis.reading_order_occurrences,
        "references": analysis.references,
        "broken_references": analysis.broken_references,
        "search_entries": analysis.search_entries,
        "distinct_searchable_resources": analysis.distinct_searchable_resources,
        "executable_states": executable_states_json(analysis.executable_states),
        "foreground_preparation": foreground_preparation_json(analysis.foreground_preparation),
        "content_occurrences": content_occurrences_json(analysis.content_occurrences),
        "completeness": analysis.completeness.iter().map(completeness_json).collect::<Vec<_>>(),
    })
}

fn executable_states_json(counts: ExecutableStateCounts) -> Value {
    json!({
        "detected": counts.detected,
        "absent": counts.absent,
        "unknown": counts.unknown,
        "inapplicable": counts.inapplicable,
    })
}

fn foreground_preparation_json(counts: ForegroundPreparationCounts) -> Value {
    json!({
        "eligible": counts.eligible,
        "ineligible": counts.ineligible,
        "unknown": counts.unknown,
    })
}

fn content_occurrences_json(counts: ContentOccurrenceCounts) -> Value {
    json!({
        "forms": counts.forms,
        "scripts": counts.scripts,
        "direct_keydown_handlers": counts.keydown_handlers,
        "direct_keyup_handlers": counts.keyup_handlers,
        "direct_keypress_handlers": counts.keypress_handlers,
    })
}

fn completeness_json(family: &CompletenessFamilyRecord) -> Value {
    json!({
        "family": family.family,
        "expected": family.expected,
        "complete": family.complete,
        "partial": family.partial,
        "unavailable": family.unavailable,
        "issues": family.issues.iter().map(|issue| json!({
            "state": issue.kind.to_string(),
            "issue": analysis_issue_label(issue.issue),
            "count": issue.count,
        })).collect::<Vec<_>>(),
    })
}

fn analysis_issue_label(issue: AnalysisIssue) -> &'static str {
    match issue {
        AnalysisIssue::ResourceLimit => "resource_limit",
        AnalysisIssue::PerResourceAnalysisLimit => "per_resource_analysis_limit",
        AnalysisIssue::TotalAnalysisLimit => "total_analysis_limit",
        AnalysisIssue::TotalFingerprintLimit => "total_fingerprint_limit",
        AnalysisIssue::Missing => "missing",
        AnalysisIssue::Unreadable => "unreadable",
        AnalysisIssue::Unsupported => "unsupported",
        AnalysisIssue::Malformed => "malformed",
        AnalysisIssue::RandomAccessUnavailable => "random_access_unavailable",
        AnalysisIssue::ParserFailure => "parser_failure",
    }
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).expect("benchmark duration fits in JSON u64 microseconds")
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RunSummary {
    successful_books: usize,
    failed_books: usize,
    archive_size_bytes: Option<u64>,
    open_elapsed: Duration,
    analysis_elapsed: Duration,
    physical_resources: usize,
    manifest_declarations: usize,
    reading_order_occurrences: usize,
    references: usize,
    broken_references: usize,
    search_entries: usize,
    distinct_searchable_resources: usize,
    executable_states: ExecutableStateCounts,
    foreground_preparation: ForegroundPreparationCounts,
    content_occurrences: ContentOccurrenceCounts,
    completeness: Vec<CompletenessFamilyRecord>,
}

fn summarize_run(run: &Run) -> RunSummary {
    let mut summary = RunSummary {
        archive_size_bytes: Some(0),
        ..RunSummary::default()
    };
    for book in &run.books {
        summary.archive_size_bytes = summary
            .archive_size_bytes
            .zip(book.archive_size_bytes)
            .and_then(|(total, size)| total.checked_add(size));
        match &book.outcome {
            BookOutcome::Success {
                analysis,
                open_elapsed,
                analysis_elapsed,
            } => {
                summary.successful_books += 1;
                summary.open_elapsed += *open_elapsed;
                summary.analysis_elapsed += *analysis_elapsed;
                summary.physical_resources += analysis.physical_resources;
                summary.manifest_declarations += analysis.manifest_declarations;
                summary.reading_order_occurrences += analysis.reading_order_occurrences;
                summary.references += analysis.references;
                summary.broken_references += analysis.broken_references;
                summary.search_entries += analysis.search_entries;
                summary.distinct_searchable_resources += analysis.distinct_searchable_resources;
                add_executable_states(&mut summary.executable_states, analysis.executable_states);
                add_foreground_preparation(
                    &mut summary.foreground_preparation,
                    analysis.foreground_preparation,
                );
                add_content_occurrences(
                    &mut summary.content_occurrences,
                    analysis.content_occurrences,
                );
                add_completeness(&mut summary.completeness, &analysis.completeness);
            }
            BookOutcome::Failure { elapsed, .. } => {
                summary.failed_books += 1;
                summary.open_elapsed += *elapsed;
            }
        }
    }
    summary
}

fn add_executable_states(total: &mut ExecutableStateCounts, value: ExecutableStateCounts) {
    total.detected += value.detected;
    total.absent += value.absent;
    total.unknown += value.unknown;
    total.inapplicable += value.inapplicable;
}

fn add_foreground_preparation(
    total: &mut ForegroundPreparationCounts,
    value: ForegroundPreparationCounts,
) {
    total.eligible += value.eligible;
    total.ineligible += value.ineligible;
    total.unknown += value.unknown;
}

fn add_content_occurrences(total: &mut ContentOccurrenceCounts, value: ContentOccurrenceCounts) {
    total.forms += value.forms;
    total.scripts += value.scripts;
    total.keydown_handlers += value.keydown_handlers;
    total.keyup_handlers += value.keyup_handlers;
    total.keypress_handlers += value.keypress_handlers;
}

fn add_completeness(total: &mut Vec<CompletenessFamilyRecord>, value: &[CompletenessFamilyRecord]) {
    for family in value {
        let target = if let Some(target) = total
            .iter_mut()
            .find(|target| target.family == family.family)
        {
            target
        } else {
            total.push(CompletenessFamilyRecord {
                family: family.family,
                expected: 0,
                complete: 0,
                partial: 0,
                unavailable: 0,
                issues: Vec::new(),
            });
            total.last_mut().expect("a completeness family was added")
        };
        target.expected += family.expected;
        target.complete += family.complete;
        target.partial += family.partial;
        target.unavailable += family.unavailable;
        for issue in &family.issues {
            for _ in 0..issue.count {
                add_coverage_issue(&mut target.issues, family.family, issue.kind, issue.issue);
            }
        }
    }
}

fn print_run(run: Run) {
    println!("\n{}", run.label);
    println!(
        "status\topen_ms\tanalysis_ms\tresources\treferences\tbroken\tsearch\tcoverage\tcoverage_issues\tbook"
    );

    let mut successful = 0;
    let mut failed = 0;
    let mut resources = 0;
    let mut references = 0;
    let mut incomplete = 0;
    let mut open_elapsed = Duration::ZERO;
    let mut analysis_elapsed = Duration::ZERO;

    for book in &run.books {
        let name = book
            .path
            .file_name()
            .unwrap_or(book.path.as_os_str())
            .to_string_lossy();
        match &book.outcome {
            BookOutcome::Success {
                analysis,
                open_elapsed: book_open_elapsed,
                analysis_elapsed: book_analysis_elapsed,
            } => {
                successful += 1;
                resources += analysis.physical_resources;
                references += analysis.references;
                open_elapsed += *book_open_elapsed;
                analysis_elapsed += *book_analysis_elapsed;
                let complete = analysis.coverage_complete();
                incomplete += usize::from(!complete);
                let coverage_issues = analysis.coverage_issues_label();
                println!(
                    "ok\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{coverage_issues}\t{name}",
                    book_open_elapsed.as_millis(),
                    book_analysis_elapsed.as_millis(),
                    analysis.physical_resources,
                    analysis.references,
                    analysis.broken_references,
                    analysis.search_entries,
                    if complete { "complete" } else { "partial" },
                );
            }
            BookOutcome::Failure {
                stage,
                message,
                elapsed,
            } => {
                failed += 1;
                open_elapsed += *elapsed;
                println!(
                    "error:{stage}\t{}\t-\t-\t-\t-\t-\t-\t-\t{name}: {message}",
                    elapsed.as_millis()
                );
            }
        }
    }

    println!(
        "summary: wall={:.3}s open_sum={:.3}s analysis_sum={:.3}s successful={successful} failed={failed} resources={resources} references={references} incomplete={incomplete}",
        run.wall_elapsed.as_secs_f64(),
        open_elapsed.as_secs_f64(),
        analysis_elapsed.as_secs_f64(),
    );
}

impl AnalysisRecord {
    fn coverage_complete(&self) -> bool {
        self.completeness
            .iter()
            .all(|family| family.partial == 0 && family.unavailable == 0)
    }

    fn coverage_issues_label(&self) -> String {
        let issues = self
            .completeness
            .iter()
            .flat_map(|family| &family.issues)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if issues.is_empty() {
            "-".to_owned()
        } else {
            issues.join(",")
        }
    }
}
