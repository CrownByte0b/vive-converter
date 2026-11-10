# ViveNotes OneNote converter

This standalone CLI converts a OneNote package, notebook, or section into a portable ViveNotes
`.vive` notebook. It uses the adjacent `onenote.rs`.

> Docs on .vive format [here](https://github.com/AquilaIgnis/viveNotes/blob/master/docs/viveFormat.md)

```sh
cd converter/vive-converter
cargo run --release -- ../Calculus2.onepkg
```

The output defaults to the input name with a `.vive` extension. Use `--output PATH` to choose a
different destination, `--force` to replace an existing file.

Print the converter version with `-v` or `--version`:

```sh
cargo run --release -- -v
```

## Browser WebAssembly

The browser build performs parsing, SQLite generation, validation, and ZIP creation locally.
`wasm32-unknown-unknown` Rust target, Clang (used to compile SQLite), and `wasm-pack` , then run:

```sh
make wasm
```

This creates a web-targeted ES module in `pkg/`. A minimal file-picker integration looks like:

```js
import init, { convert, version } from './pkg/vive_converter.js';

await init();
console.log(`Vive converter ${version()}`);

async function convertUpload(file) {
  const input = new Uint8Array(await file.arrayBuffer());
  const result = convert(input, file.name);
  const outputFileName = result.outputFileName;
  const warnings = JSON.parse(result.warningsJson);
  const output = result.intoBytes();

  const url = URL.createObjectURL(
    new Blob([output], { type: 'application/vnd.vivenotes.notebook+zip' }),
  );
  const link = Object.assign(document.createElement('a'), {
    href: url,
    download: outputFileName,
  });
  link.click();
  URL.revokeObjectURL(url);
  return warnings;
}
```

## Android (ViveNotes)

ViveNotes runs the converter in-process: File ▸ Import ▸ Notebook hands it any file that is not a
`.vive`. The app's Gradle build compiles this crate with cargo-ndk, so it needs Rust, the
`aarch64-linux-android` and `x86_64-linux-android` targets, `cargo-ndk`, and the NDK. To build the
same library by hand:

```sh
make android-lib   # target/jniLibs/<abi>/libvive_converter.so
```

`src/android.rs` exports one JNI function,
`com.vivenotes.data.onenote.OneNoteConverter.convert(input, fileName, seed, output)`. It converts the
file at `input` and writes the `.vive` to `output`, which the app then validates and installs like
any other. It returns JSON: the counts and warnings, or `{error, detail}`.

Two things differ from the CLI and the browser:

- **Identity.** The notebook's ids derive from the `seed` the app passes, a fresh one for each
  import, so importing the same file twice gives two notebooks. The CLI and the browser derive ids
  from the file name, so a second conversion of the same name updates the first copy when imported.
- **Names.** `fileName` is the user's name for the file, and `input` can be called anything.

Every entry point that takes a single file, the browser's included, recognises a package by its
cabinet signature and reads anything else as a section. A `.onetoc2` is refused by its extension,
or by the parser when the extension is missing.

# Info

The browser API accepts `.onepkg` notebook exports and individual `.one` sections. A `.onetoc2`
file references other files beside it, so the single-upload browser API rejects it; export the
notebook as `.onepkg` instead. Conversion is synchronous and can be CPU- and memory-intensive for
large notebooks, so call it from a Web Worker in the website UI.

The converter preserves sections, pages, positioned rich text, web links, common inline equations,
lists, tables, images, and ink. Section groups become a slash-separated section path because `.vive`
v1 has no group entity. Embedded files, unrecognized source objects, and links to non-web
destinations (such as other OneNote pages or `mailto:` addresses, whose text is kept) are omitted
with an explicit warning.

Before publishing the output, the CLI validates SQLite integrity and foreign keys, page JSON,
AndroidX Ink gzip payloads, ZIP entry names, and every archive checksum. ViveNotes performs the
authoritative full validation, including native AndroidX Ink decoding, when the notebook is
imported.

OneNote stores explicit ink colours as Windows `COLORREF` values (`0x00BBGGRR`); the converter
normalizes them to ARGB and preserves highlighter transparency.

# SRC

- https://github.com/msiemens/onenote.rs
