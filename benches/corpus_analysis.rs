use epub_stack::analysis::AnalysisIssue;
use epub_stack::analysis::coverage::{CoverageState, ResourceCoverage};
use epub_stack::{EpubZip, PublicationAnalysis};
use rayon::prelude::*;
use std::error::Error;
use std::fmt;
use std::fs;
use std::hint::black_box;
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
    resources: usize,
    references: usize,
    broken_references: usize,
    search_entries: usize,
    coverage_issues: Vec<CoverageIssueRecord>,
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
    if arguments
        .last()
        .is_some_and(|argument| argument == "--bench")
    {
        arguments.pop();
    }
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

    println!("corpus: {}", directory.display());
    println!("books: {}", paths.len());

    let mut mismatch = false;
    match mode {
        Mode::Sequential => print_run(run_sequential(&paths)),
        Mode::Parallel => {
            let threads = rayon::current_num_threads();
            println!("rayon threads: {threads}");
            print_run(run_parallel(&paths));
        }
        Mode::Both => {
            let threads = rayon::current_num_threads();
            println!("rayon threads: {threads}");
            let sequential = run_sequential(&paths);
            let parallel = run_parallel(&paths);
            mismatch = !compare_runs(&sequential, &parallel);
            print_run(sequential);
            print_run(parallel);
            println!(
                "correctness: {}",
                if mismatch { "MISMATCH" } else { "identical" }
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
        "usage: {} <sequential|parallel|both> <corpus-directory>",
        Path::new(program).display()
    )
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
    let open_started = Instant::now();
    let archive = match EpubZip::open(path) {
        Ok(archive) => archive,
        Err(error) => {
            return BookRecord {
                path: path.to_owned(),
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
        outcome: BookOutcome::Success {
            analysis: record,
            open_elapsed,
            analysis_elapsed,
        },
    }
}

fn summarize_analysis(analysis: &PublicationAnalysis) -> AnalysisRecord {
    let coverage = analysis.coverage();
    let mut coverage_issues = Vec::new();
    record_resource_coverage(
        &mut coverage_issues,
        "classification",
        coverage.classification(),
    );
    record_resource_coverage(&mut coverage_issues, "fragments", coverage.fragments());
    record_resource_coverage(&mut coverage_issues, "content", coverage.content());
    record_resource_coverage(&mut coverage_issues, "inspection", coverage.inspection());
    record_resource_coverage(
        &mut coverage_issues,
        "fingerprints",
        coverage.fingerprints(),
    );
    for relationship in coverage.relationships() {
        match relationship.state() {
            CoverageState::Complete => {}
            CoverageState::Partial(issue) => add_coverage_issue(
                &mut coverage_issues,
                "relationships",
                IncompleteKind::Partial,
                *issue,
            ),
            CoverageState::Unavailable(issue) => add_coverage_issue(
                &mut coverage_issues,
                "relationships",
                IncompleteKind::Unavailable,
                *issue,
            ),
        }
    }

    AnalysisRecord {
        resources: analysis.resources().len(),
        references: analysis.references().count(),
        broken_references: analysis.broken_references().count(),
        search_entries: analysis.search_entries().count(),
        coverage_issues,
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
    if left.path != right.path {
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
                resources += analysis.resources;
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
                    analysis.resources,
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
        self.coverage_issues.is_empty()
    }

    fn coverage_issues_label(&self) -> String {
        if self.coverage_issues.is_empty() {
            "-".to_owned()
        } else {
            self.coverage_issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }
    }
}
