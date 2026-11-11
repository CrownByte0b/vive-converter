mod bundle;
mod convert;
mod ink;
mod model;

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "windows",
    test
))]
mod app;
#[cfg(any(target_os = "android", target_os = "linux", target_os = "windows"))]
mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod wasm;

use anyhow::{Context, Result, bail};
use onenote_parser::Parser;
use onenote_parser::notebook::Notebook;
use onenote_parser::section::Section;
use std::fmt;
use std::io::Read;
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

/// Where a converted notebook's ids come from.
///
/// ViveNotes treats a `.vive` whose notebook id it already holds as a saved copy of that
/// notebook, and makes the notebook match it. So the identity decides what importing a second
/// conversion does.
#[derive(Clone, Copy, Debug)]
pub enum Identity<'a> {
    /// From the notebook's name. A source of the same name converts to the same ids, and
    /// importing it replaces the earlier import's contents.
    Named,
    /// From the caller's seed. Each seed is a notebook of its own, whatever it is called.
    Seeded(&'a str),
}

/// An input the converter will not take, as opposed to a fault while converting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    /// A `.onetoc2`. Its sections are separate files beside it, which one file cannot bring.
    TableOfContents,
    /// Not a OneNote package or section that the parser can read.
    Unreadable,
    /// A OneNote file without a section in it.
    NoSections,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            InputError::TableOfContents => {
                "a .onetoc2 file references separate section files; export the notebook as a .onepkg instead"
            }
            InputError::Unreadable => "the file is not a OneNote package or section this converter can read",
            InputError::NoSections => "the OneNote source contains no readable sections",
        })
    }
}

impl std::error::Error for InputError {}

pub fn convert_source(source: &Source, input: &Path) -> Result<ConvertedNotebook> {
    convert::convert(source, &notebook_name(input), Identity::Named)
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
    let file = SingleFile {
        name: single_file_name(file_name)?,
        contents: Contents::Bytes(input),
    };
    convert_single(file, Identity::Named)
}

/// Convert the `.onepkg` or `.one` at `input`, named for `file_name`.
///
/// `file_name` is what the user's file is called. `input` can be a staged copy whose own name
/// means nothing: the name gives the notebook its name and a nameless section its name, and the
/// contents decide how it is read.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn convert_file(input: &Path, file_name: &str, identity: Identity) -> Result<ConversionOutput> {
    let file = SingleFile {
        name: single_file_name(file_name)?,
        contents: Contents::Path(input),
    };
    convert_single(file, identity)
}

fn convert_single(file: SingleFile<'_>, identity: Identity) -> Result<ConversionOutput> {
    let source = parse_single(file)?;
    let converted = convert::convert(&source, &notebook_name(Path::new(file.name)), identity)?;
    let warnings = converted.warnings.clone();
    let (bytes, summary) = bundle::build_bundle(&converted)?;
    Ok(ConversionOutput {
        bytes,
        warnings,
        summary,
    })
}

/// A package is a cabinet, so its first bytes say so. Anything else is read as a section.
fn parse_single(file: SingleFile<'_>) -> Result<Source> {
    if extension(file.name).as_deref() == Some("onetoc2") {
        bail!(InputError::TableOfContents)
    }
    let typed = TypedPath::derive(file.name);
    let parser = Parser::new_with_fs(file);
    if file.starts_with(b"MSCF")? {
        return parser
            .parse_package(typed)
            .map(Source::Notebook)
            .context(InputError::Unreadable);
    }
    match parser.parse_section(typed) {
        Ok(section) => Ok(Source::Section(section)),
        // A table of contents with its extension lost reads as a store, but not a section one.
        // The parser's error kind is private, so this is matched on its message, which
        // `refuses_a_table_of_contents_by_its_contents` holds in place.
        Err(error) if error.to_string().starts_with("Not a section file") => {
            bail!(InputError::TableOfContents)
        }
        Err(error) => Err(error).context(InputError::Unreadable),
    }
}

fn single_file_name(file_name: &str) -> Result<&str> {
    Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .context("input file name is empty or not valid Unicode")
}

fn notebook_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
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

/// One file presented to the parser under the user's name for it.
#[derive(Clone, Copy)]
struct SingleFile<'a> {
    name: &'a str,
    contents: Contents<'a>,
}

#[derive(Clone, Copy)]
enum Contents<'a> {
    Bytes(&'a [u8]),
    /// Read when the parser asks, so the file is never held twice.
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    Path(&'a Path),
}

impl SingleFile<'_> {
    fn starts_with(&self, prefix: &[u8]) -> std::io::Result<bool> {
        match self.contents {
            Contents::Bytes(bytes) => Ok(bytes.starts_with(prefix)),
            Contents::Path(path) => {
                let mut head = Vec::with_capacity(prefix.len());
                std::fs::File::open(path)?
                    .take(prefix.len() as u64)
                    .read_to_end(&mut head)?;
                Ok(head == prefix)
            }
        }
    }
}

impl onenote_parser::FileSystem for SingleFile<'_> {
    fn is_directory(&self, _path: TypedPath<'_>) -> std::io::Result<bool> {
        Ok(false)
    }

    fn read_dir(&self, _path: TypedPath<'_>) -> std::io::Result<Vec<typed_path::TypedPathBuf>> {
        Ok(Vec::new())
    }

    fn read_file(&self, path: TypedPath<'_>) -> std::io::Result<Vec<u8>> {
        if path.to_string_lossy() != self.name {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("uploaded file not found: {}", path.to_string_lossy()),
            ));
        }
        match self.contents {
            Contents::Bytes(bytes) => Ok(bytes.to_vec()),
            Contents::Path(path) => std::fs::read(path),
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
        Ok(path.to_string_lossy() == self.name)
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
pub(crate) mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    pub(crate) const SAMPLES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../onenote.rs/crates/parser/tests/samples"
    );

    pub(crate) fn sample(name: &str) -> Vec<u8> {
        std::fs::read(format!("{SAMPLES}/{name}")).unwrap()
    }

    /// A `.onepkg` is a cabinet holding a `.onetoc2` and the sections it lists.
    pub(crate) fn package() -> Vec<u8> {
        let files = [
            "Open Notebook.onetoc2",
            "New Section 1.one",
            "New Section 2.one",
        ];
        let mut builder = cab::CabinetBuilder::new();
        let folder = builder.add_folder(cab::CompressionType::MsZip);
        for name in files {
            folder.add_file(name);
        }
        let mut writer = builder.build(Cursor::new(Vec::new())).unwrap();
        for name in files {
            let mut file = writer.next_file().unwrap().unwrap();
            file.write_all(&sample(&format!("New Section Group/{name}")))
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    pub(crate) fn notebook_id(bundle: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bundle)).unwrap();
        let mut manifest = String::new();
        archive
            .by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        manifest["sourceNotebookId"].as_str().unwrap().to_owned()
    }

    fn input_error(result: Result<ConversionOutput>) -> Option<InputError> {
        result.err()?.downcast_ref::<InputError>().copied()
    }

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

    #[test]
    fn reads_a_package_by_its_contents_whatever_it_is_called() {
        let package = package();
        for name in ["Group.onepkg", "Group", "Group.one"] {
            let output = convert_bytes(&package, name).unwrap();
            assert_eq!(output.summary.sections, 2, "{name}");
        }
    }

    #[test]
    fn reads_a_section_without_an_extension() {
        let output = convert_bytes(&sample("New Section 1.one"), "New Section 1").unwrap();
        assert_eq!(output.summary.sections, 1);
    }

    #[test]
    fn refuses_a_table_of_contents_by_its_name() {
        let toc = sample("Open Notebook.onetoc2");
        assert_eq!(
            input_error(convert_bytes(&toc, "Open Notebook.onetoc2")),
            Some(InputError::TableOfContents),
        );
    }

    #[test]
    fn refuses_a_table_of_contents_by_its_contents() {
        let toc = sample("Open Notebook.onetoc2");
        assert_eq!(
            input_error(convert_bytes(&toc, "Open Notebook")),
            Some(InputError::TableOfContents),
        );
    }

    #[test]
    fn refuses_what_is_not_onenote() {
        for (bytes, name) in [
            (b"PK\x03\x04 not a section".as_slice(), "notes.one"),
            (b"MSCF but not a cabinet".as_slice(), "notes.onepkg"),
            (b"".as_slice(), "empty"),
        ] {
            assert_eq!(
                input_error(convert_bytes(bytes, name)),
                Some(InputError::Unreadable),
                "{name}",
            );
        }
    }

    #[test]
    fn a_seed_makes_a_notebook_of_its_own() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("staged");
        std::fs::write(&staged, sample("New Section 1.one")).unwrap();
        let convert =
            |identity| notebook_id(&convert_file(&staged, "Notes.one", identity).unwrap().bytes);

        let named = convert(Identity::Named);
        assert_eq!(
            named,
            notebook_id(
                &convert_bytes(&sample("New Section 1.one"), "Notes.one")
                    .unwrap()
                    .bytes
            )
        );
        let first = convert(Identity::Seeded("first"));
        assert_eq!(first, convert(Identity::Seeded("first")));
        assert_ne!(first, convert(Identity::Seeded("second")));
        assert_ne!(first, named);
    }
}
