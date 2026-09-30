mod bundle;
mod convert;
mod ink;
mod model;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod wasm;

use anyhow::{Context, Result, bail};
use onenote_parser::Parser;
use onenote_parser::notebook::Notebook;
use onenote_parser::section::Section;
use std::path::Path;
use typed_path::TypedPath;

pub use bundle::BundleSummary;
pub use model::ConvertedNotebook;

/// User-facing converter version shared by the CLI and browser module.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub enum Source {
    Notebook(Notebook),
    Section(Section),
}

pub struct ConversionOutput {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
    pub summary: BundleSummary,
}

pub fn convert_source(source: &Source, input: &Path) -> Result<ConvertedNotebook> {
    convert::convert(source, input)
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn parse_input(input: &Path) -> Result<Source> {
    let input_text = input.to_str().context("input path is not valid Unicode")?;
    let typed = TypedPath::derive(input_text);
    let parser = Parser::new();
    match extension(input_text).as_deref() {
        Some("onepkg") => parser
            .parse_package(typed)
            .map(Source::Notebook)
            .context("parsing OneNote package"),
        Some("onetoc2") => parser
            .parse_notebook(typed)
            .map(Source::Notebook)
            .context("parsing OneNote notebook"),
        Some("one") => parser
            .parse_section(typed)
            .map(Source::Section)
            .context("parsing OneNote section"),
        _ => bail!("input must have a .onepkg, .onetoc2, or .one extension"),
    }
}

/// Convert one uploaded `.onepkg` or `.one` file entirely in memory.
///
/// A standalone `.onetoc2` cannot be converted by this API because its referenced
/// section files are separate uploads. Export the notebook as `.onepkg` for the web.
pub fn convert_bytes(input: &[u8], file_name: &str) -> Result<ConversionOutput> {
    let file_name = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .context("input file name is empty or not valid Unicode")?;
    let typed = TypedPath::derive(file_name);
    let parser = Parser::new_with_fs(MemoryFileSystem {
        file_name,
        bytes: input,
    });
    let source = match extension(file_name).as_deref() {
        Some("onepkg") => parser
            .parse_package(typed)
            .map(Source::Notebook)
            .context("parsing OneNote package")?,
        Some("one") => parser
            .parse_section(typed)
            .map(Source::Section)
            .context("parsing OneNote section")?,
        Some("onetoc2") => bail!(
            "a .onetoc2 file references separate section files; upload an exported .onepkg instead"
        ),
        _ => bail!("browser input must have a .onepkg or .one extension"),
    };
    let converted = convert::convert(&source, Path::new(file_name))?;
    let warnings = converted.warnings.clone();
    let (bytes, summary) = bundle::build_bundle(&converted)?;
    Ok(ConversionOutput {
        bytes,
        warnings,
        summary,
    })
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn write_bundle(
    notebook: &ConvertedNotebook,
    output: &Path,
    force: bool,
) -> Result<BundleSummary> {
    bundle::write_bundle(notebook, output, force)
}

fn extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

#[derive(Clone, Copy)]
struct MemoryFileSystem<'a> {
    file_name: &'a str,
    bytes: &'a [u8],
}

impl onenote_parser::FileSystem for MemoryFileSystem<'_> {
    fn is_directory(&self, _path: TypedPath<'_>) -> std::io::Result<bool> {
        Ok(false)
    }

    fn read_dir(&self, _path: TypedPath<'_>) -> std::io::Result<Vec<typed_path::TypedPathBuf>> {
        Ok(Vec::new())
    }

    fn read_file(&self, path: TypedPath<'_>) -> std::io::Result<Vec<u8>> {
        if path.to_string_lossy() == self.file_name {
            Ok(self.bytes.to_vec())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("uploaded file not found: {}", path.to_string_lossy()),
            ))
        }
    }

    fn write_file(&self, _path: TypedPath<'_>, _data: &[u8]) -> std::io::Result<()> {
        Err(read_only_error())
    }

    fn stream_to_file(
        &self,
        _path: TypedPath<'_>,
        _reader: &mut dyn std::io::Read,
    ) -> std::io::Result<()> {
        Err(read_only_error())
    }

    fn make_dir(&self, _path: TypedPath<'_>) -> std::io::Result<()> {
        Err(read_only_error())
    }

    fn canonicalize(&self, path: TypedPath<'_>) -> std::io::Result<typed_path::TypedPathBuf> {
        Ok(path.to_path_buf())
    }

    fn exists(&self, path: TypedPath<'_>) -> std::io::Result<bool> {
        Ok(path.to_string_lossy() == self.file_name)
    }

    fn is_windows(&self) -> bool {
        false
    }
}

fn read_only_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the uploaded-file filesystem is read-only",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn converts_an_uploaded_section_entirely_in_memory() {
        let input = include_bytes!(
            "../../onenote.rs/crates/parser/tests/samples/joplin/Simple notebook/Quick Notes.one"
        );
        let output = convert_bytes(input, "Quick Notes.one").unwrap();

        assert!(output.warnings.is_empty());
        assert_eq!(output.summary.sections, 1);
        assert_eq!(output.summary.pages, 1);

        let mut archive = zip::ZipArchive::new(Cursor::new(output.bytes)).unwrap();
        let mut database = Vec::new();
        archive
            .by_name("notebook.sqlite")
            .unwrap()
            .read_to_end(&mut database)
            .unwrap();
        assert!(database.starts_with(b"SQLite format 3\0"));
    }
}
