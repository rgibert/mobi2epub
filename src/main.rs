//! mobi2epub: convert MOBI ebooks to EPUB.

mod epub;
mod html;
mod mobi;
mod telemetry;
mod zip;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: mobi2epub [-o OUTPUT.epub] INPUT.mobi [INPUT.mobi ...]

Convert MOBI (and PalmDOC .prc/.pdb) ebooks to EPUB.
By default each output is written next to its input with an .epub extension.

Options:
  -o, --output FILE   output path (only with a single input)
  -f, --force         overwrite existing output files
  -h, --help          show this help
  -V, --version       show version";

fn main() -> ExitCode {
    // Kept alive for the whole run; dropping it flushes pending reports.
    let _telemetry = telemetry::init();
    let mut inputs = Vec::new();
    let mut output: Option<PathBuf> = None;
    let mut force = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Some("-V" | "--version") => {
                println!("mobi2epub {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            Some("-f" | "--force") => force = true,
            Some("-o" | "--output") => match args.next() {
                Some(o) => output = Some(o.into()),
                None => {
                    eprintln!("error: {} needs a file name\n\n{USAGE}", arg.to_string_lossy());
                    return ExitCode::from(2);
                }
            },
            Some(s) if s.starts_with('-') && s.len() > 1 => {
                eprintln!("error: unknown option {s}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    if inputs.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if output.is_some() && inputs.len() > 1 {
        eprintln!("error: -o can only be used with a single input file");
        return ExitCode::from(2);
    }

    let mut failed = false;
    for input in &inputs {
        let out = output.clone().unwrap_or_else(|| input.with_extension("epub"));
        let ext = input.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        let trace = telemetry::Conversion::start(&ext);
        match convert(input, &out, force, &trace) {
            Ok(msg) => {
                println!("{} -> {} ({msg})", input.display(), out.display());
                trace.finish(telemetry::Outcome::Ok);
            }
            Err(e) => {
                eprintln!("{}: error: {}", input.display(), e.message());
                let outcome = match &e {
                    ConvertError::Book { message, size } => {
                        telemetry::report_book_error(message, &ext, *size);
                        telemetry::Outcome::BadBook
                    }
                    ConvertError::Usage(_) => telemetry::Outcome::Usage,
                    ConvertError::Io(_) => telemetry::Outcome::Io,
                };
                trace.finish(outcome);
                failed = true;
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// Why a conversion failed. Only `Book` errors (problems with the ebook
/// itself) are reported to Sentry; usage and file-system errors are not.
enum ConvertError {
    Usage(String),
    Io(String),
    Book { message: String, size: usize },
}

impl ConvertError {
    fn message(&self) -> &str {
        match self {
            ConvertError::Usage(m) | ConvertError::Io(m) => m,
            ConvertError::Book { message, .. } => message,
        }
    }
}

fn convert(input: &Path, output: &Path, force: bool, trace: &telemetry::Conversion) -> Result<String, ConvertError> {
    if output == input {
        return Err(ConvertError::Usage("output path is the same as the input".into()));
    }
    if output.exists() && !force {
        return Err(ConvertError::Usage(format!("{} already exists (use -f to overwrite)", output.display())));
    }
    let data = trace
        .step("file.read", "Read input", || std::fs::read(input))
        .map_err(|e| ConvertError::Io(format!("cannot read file: {e}")))?;
    let size = data.len();
    trace.set_data("input.size", size);
    let book = trace
        .step("mobi.parse", "Parse and decompress MOBI", || mobi::parse(data))
        .map_err(|message| ConvertError::Book { message, size })?;
    trace.set_data("text.size", book.text.len());
    let images = trace.step("mobi.images", "Collect images", || epub::collect_images(&book));
    let conv = trace.step("html.convert", "Convert HTML to XHTML", || {
        if book.is_html {
            let names: HashMap<usize, String> = images.iter().map(|(k, v)| (*k, v.file.clone())).collect();
            html::convert(&book.text, book.utf8, &names)
        } else {
            html::convert_plain(&book.text)
        }
    });
    let (bytes, stats) = trace.step("epub.build", "Build EPUB package", || epub::build(&book, &conv, &images));
    trace.set_data("output.size", bytes.len());
    trace.set_data("chapters", stats.chapters);
    trace.set_data("images", stats.images);
    trace
        .step("file.write", "Write output", || std::fs::write(output, &bytes))
        .map_err(|e| ConvertError::Io(format!("cannot write {}: {e}", output.display())))?;
    Ok(format!(
        "\"{}\", {} part{}, {} image{}",
        book.title.trim(),
        stats.chapters,
        if stats.chapters == 1 { "" } else { "s" },
        stats.images,
        if stats.images == 1 { "" } else { "s" }
    ))
}
