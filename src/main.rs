//! mobi2epub: convert MOBI ebooks to EPUB.

mod epub;
mod html;
mod mobi;
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
        match convert(input, &out, force) {
            Ok(msg) => println!("{} -> {} ({msg})", input.display(), out.display()),
            Err(e) => {
                eprintln!("{}: error: {e}", input.display());
                failed = true;
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn convert(input: &Path, output: &Path, force: bool) -> Result<String, String> {
    if output == input {
        return Err("output path is the same as the input".into());
    }
    if output.exists() && !force {
        return Err(format!("{} already exists (use -f to overwrite)", output.display()));
    }
    let data = std::fs::read(input).map_err(|e| format!("cannot read file: {e}"))?;
    let book = mobi::parse(data)?;
    let images = epub::collect_images(&book);
    let conv = if book.is_html {
        let names: HashMap<usize, String> = images.iter().map(|(k, v)| (*k, v.file.clone())).collect();
        html::convert(&book.text, book.utf8, &names)
    } else {
        html::convert_plain(&book.text)
    };
    let (bytes, stats) = epub::build(&book, &conv, &images);
    std::fs::write(output, bytes).map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    Ok(format!(
        "\"{}\", {} part{}, {} image{}",
        book.title.trim(),
        stats.chapters,
        if stats.chapters == 1 { "" } else { "s" },
        stats.images,
        if stats.images == 1 { "" } else { "s" }
    ))
}
