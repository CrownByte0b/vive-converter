# ViveNotes OneNote converter

The CLI and shared library convert a OneNote package, notebook, or section into a portable
ViveNotes `.vive` notebook. They use the `onenote_parser` crate from crates.io.

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

`src/native.rs` exports one JNI function,
`com.vivenotes.data.onenote.OneNoteConverter.convert(input, fileName, seed, output)`. It converts the
file at `input` and writes the `.vive` to `output`, which the app then validates and installs like
any other. It returns JSON: the counts and warnings, or `{error, detail}`.

The app API differs from the CLI and the browser in two ways:

- **Identity.** The notebook's ids derive from the `seed` the app passes, a fresh one for each
  import, so importing the same file twice gives two notebooks. The CLI and the browser derive ids
  from the file name, so a second conversion of the same name updates the first copy when imported.
- **Names.** `fileName` is the user's name for the file, and `input` can be called anything.

Every entry point that takes a single file, the browser's included, recognises a package by its
cabinet signature and reads anything else as a section. A `.onetoc2` is refused by its extension,
or by the parser when the extension is missing.

## Linux and Windows (JVM)

The shared library exposes the same JNI method and JSON result on Android, Linux and Windows.
The desktop builds support these 64-bit targets:

| Platform       | Rust target                 | Library                |
| -------------- | --------------------------- | ---------------------- |
| Linux x86-64   | `x86_64-unknown-linux-gnu`  | `libvive_converter.so` |
| Linux ARM64    | `aarch64-unknown-linux-gnu` | `libvive_converter.so` |
| Windows x86-64 | `x86_64-pc-windows-msvc`    | `vive_converter.dll`   |
| Windows ARM64  | `aarch64-pc-windows-msvc`   | `vive_converter.dll`   |

```sh
make native-lib
cargo build --locked --profile native --lib

make native-lib NATIVE_TARGET=x86_64-unknown-linux-gnu
```

For an explicit target, install its Rust standard library with `rustup target add <target>`.
Cross-compiling also requires that target's C compiler and linker; CI builds each desktop library
on a native runner instead. The Linux release libraries are built on Ubuntu 22.04 and require
glibc 2.35 or newer. Match the library's architecture to the **JVM's** architecture.

On the Kotlin/JVM side, keep the package, class and static method names exactly as follows, and
put the extracted library in a directory on `java.library.path`:

```kotlin
package com.vivenotes.data.onenote

object OneNoteConverter {
    init { System.loadLibrary("vive_converter") }

    @JvmStatic
    external fun convert(input: String, fileName: String, seed: String, output: String): String
}
```

The JVM smoke test loads the compiled library and checks conversion, Unicode strings/paths and
error results. With a JDK installed, run it locally after `make native-lib`:

```sh
javac -d target/jni-smoke tests/jni/com/vivenotes/data/onenote/OneNoteConverter.java
java -cp target/jni-smoke com.vivenotes.data.onenote.OneNoteConverter \
  target/native/libvive_converter.so \
  "tests/samples/joplin/Simple notebook/Quick Notes.one"
```

Use `target/native/vive_converter.dll` on Windows. For an explicit Rust target, include its name
in the library path, for example `target/x86_64-unknown-linux-gnu/native/libvive_converter.so`.

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
