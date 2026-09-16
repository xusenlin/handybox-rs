# HandyBox

<img src="assets/app-icon.png" alt="HandyBox mascot" width="88" height="88">

English | [简体中文](README.zh-CN.md)

A fast, offline-first desktop utility toolbox built with Rust and Slint.

HandyBox brings everyday utilities into a single native desktop workspace. This version ships an **English / Simplified Chinese UI**, a working **document-to-Markdown converter**, a working **JSON workbench** with jq-style queries, a working **Hash & encrypt** tool built on sha2 and age, a working **Text diff** built on similar, a working **Barcode reader** built on rxing, a working **Clipboard workspace** built on clipboard-rs, a working **Disk cleanup** tool, a working **Image studio**, and a working **Archives** tool built on zip and sevenz-rust2. Every tool in the catalog is implemented; the placeholder page remains for the next one. No account, API key, server, WebView, or document upload is required.

Use **English / 中文** at the bottom of the sidebar to switch languages immediately. The current tool, search query and converted document are retained. Tool search accepts either language and engine names. English is the default; the selection is saved locally for the next launch.

## Run

Install Rust with rustup, then run from this repository:

```sh
cargo run --locked

# Open a document immediately
cargo run --locked -- examples/sample.rtf

# An image opens in the barcode reader
cargo run --locked -- examples/sample-codes.png

# A .zip or .7z opens in Archives and is listed
cargo run --locked -- ~/Downloads/release-1.2.zip

# A folder opens in Disk cleanup and is scanned
cargo run --locked -- ~/Downloads

# Two paths are a comparison: they open side by side in Text diff
cargo run --locked -- old.txt new.txt

# Force the CPU renderer for compatibility testing
SLINT_BACKEND=winit-software cargo run --locked

# Optimized executable: target/release/handybox
cargo build --release --locked
```

The repository pins **Rust 1.88.0**, **Slint 1.13.1**, **anydoc 0.2.4**, the **jaq 3** crates, **age 0.12**, **similar 3**, **rxing 0.9**, **clipboard-rs 0.3**, **trash 5**, **oxipng 10**, **zip 8.6** and **sevenz-rust2 0.20**; commit `Cargo.lock` with dependency updates. Rustup installs the pinned toolchain if needed. Initial dependency downloads require a connection; the built application processes documents entirely locally.

On macOS, install Xcode Command Line Tools. On Windows, use the MSVC Rust toolchain and Visual Studio C++ Build Tools. On Debian/Ubuntu, install desktop build dependencies:

```sh
sudo apt-get install build-essential pkg-config libx11-dev libx11-xcb-dev \
  libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libgl1-mesa-dev \
  libfontconfig1-dev libwayland-dev
```

Linux currently uses X11 (or XWayland in a Wayland session). The native file picker uses `xdg-desktop-portal` and a desktop-specific portal backend. CPU rendering still requires a graphical desktop session.

## Release builds

[Task](https://taskfile.dev) wraps the common commands; `task --list` shows them all.

```sh
task run                        # cargo run --locked
task run -- examples/sample.rtf # open a document at startup
task check                      # fmt + clippy
task test                       # workspace tests

task build                      # all three platforms into dist/
task build:native               # host platform only
task build:cross                # Windows and Linux only
```

`task build` produces optimized, version-named archives in `dist/`, plus a `SHA256SUMS` manifest. Every platform ships as a zip, which roughly halves the download and keeps the three downloads consistent:

```text
dist/handybox-0.1.0-macos-arm64.zip    # HandyBox.app
dist/handybox-0.1.0-windows-x64.zip    # handybox-0.1.0-windows-x64.exe
dist/handybox-0.1.0-linux-x64.zip      # handybox-0.1.0-linux-x64
```

The version comes from `Cargo.toml`'s `[workspace.package]` and is never written a second time. Stale artifacts are removed first, so `dist/` never mixes versions. `dist/` is not committed.

The release profile trades build time for size: `lto = "fat"`, `codegen-units = 1` and `strip = true`. Fat LTO costs minutes on a cold build and is worth roughly 8% of the executable. `panic = "abort"` would remove another megabyte of unwinding tables but is deliberately not set: the worker relies on `catch_unwind` to survive a parser panic on a malformed document, and aborting would take the whole application down instead.

macOS ships as `HandyBox.app`, built by [`scripts/macos-bundle.sh`](scripts/macos-bundle.sh). A bare Mach-O executable opens Terminal when double-clicked in Finder and shows no icon; only a bundle launches as a normal application. The script writes `Info.plist`, derives `AppIcon.icns` with `sips`/`iconutil`, and archives with `ditto`, which preserves the bundle's executable bits and symlinks. It assembles under `target/`, so `dist/` only ever holds publishable archives. The Windows and Linux executables are zipped by [`scripts/zip-binary.sh`](scripts/zip-binary.sh); Info-ZIP records the Unix mode, so the Linux binary stays executable after extraction. Windows links as a GUI subsystem executable, so its `.exe` opens no console window.

The Windows and Linux binaries are cross-compiled in the build-only container defined by [`scripts/Dockerfile.cross`](scripts/Dockerfile.cross), which supplies the mingw-w64 and GNU cross linkers plus the Linux desktop development libraries that the winit backend links against. Nothing from that container ships inside the application. Host and cross builds use separate Cargo caches, so they never overwrite each other's artifacts. A running Docker daemon is required. The image is tagged with a hash of its Dockerfile, so editing the toolchain rebuilds it automatically instead of silently reusing a stale image.

`task build` requires a macOS host, because the Apple target cannot be cross-compiled in the container. On other systems use `task build:native` and `task build:cross` separately.

The artifacts carry an ad-hoc signature at most: there is no Developer ID signing and no notarization, so macOS Gatekeeper and Windows SmartScreen warn on first launch. Expand the macOS archive in Finder or with `ditto -x -k`; command-line `unzip` extracts the `._` companion files that macOS attaches to the bundle, which makes `codesign` report a missing sealed resource.

## Tools and implementation status

| Tool | Engine | Current status |
| --- | --- | --- |
| Document converter | `anydoc` | Implemented: choose/drop a file, convert, inspect Markdown source, copy all, export `.md` |
| JSON workbench | `jaq` | Implemented: validate, format, minify, query with jq syntax, copy and export `.json` |
| Hash & encrypt | `sha2` + `age` | Implemented: SHA-256/SHA-512 checksums and verification, age encryption and decryption by passphrase or key |
| Text diff | `similar` | Implemented: live line-by-line comparison of two texts or files, change summary, copy and export a unified diff |
| Image studio | `image` + `fast_image_resize` + `oxipng` + `nom-exif` | Implemented: list a folder, inspect a picture and its EXIF, resize and convert between PNG/JPEG/TIFF/BMP, optimize PNG losslessly, export a batch |
| Archives | `zip` + `sevenz-rust2` | Implemented: read what a ZIP or 7z holds, extract the ticked entries into a folder, pack a folder into a new ZIP or 7z |
| Barcode reader | `rxing` | Implemented: read every QR code and barcode in an image, see where each one sits, copy one or all |
| Disk cleanup | `sha2` + `trash` | Implemented: scan a folder for duplicates, empties and the biggest files, tick what may go, confirm, and move it to the system trash |
| Clipboard | `clipboard-rs` | Implemented: keep the text, pictures and file references you copy, collect automatically when asked, put any of them back, and save a picture as `.png` |

Planned engines are recorded in the catalog, not exposed as pretend operations or pulled into production dependencies prematurely — every entry above is now implemented, and the placeholder page stays for whichever tool is added next. `zip` was a development dependency for an actual DOCX test fixture before Archives made it a real one, and `rxing`'s barcode *writer* is still one: the barcode tests draw the codes they then read back. czkawka was the catalogued engine for Disk cleanup and is not used; the entry names what that tool actually uses, and [the section below](#disk-cleanup) says why. `sevenz-rust2` is pinned to its 0.20 line for the same reason — from 0.21 it needs a newer Rust than this workspace pins.

## Document converter

1. Select **Browse files**, drag one file onto the window, or pass a file path when launching.
2. Conversion starts automatically in a background worker. The source is read without modification.
3. Read the generated Markdown source. **Copy all** copies the complete result, and **Export .md** opens a native save dialog.

The converter supports the formats provided by anydoc: Word (`doc`, `docx`, `docm`), PowerPoint (`ppt`, `pps`, `pot`, `pptx`, `pptm`, `ppsx`, `ppsm`), Excel (`xls`, `xlsx`, `xlsm`, `xlsb`), OpenDocument (`odt`, `ods`, `odp`), PDF, EPUB, RTF and CSV. Content detection takes priority over the file extension; signature-less CSV uses its extension.

Behavior and current limits:

- Single-file processing, with a **64 MiB input limit** and a bounded read even if the file grows. For a multi-file drop, only the first file accepted while idle is processed; there is no batch queue.
- Text-based PDFs are supported. Scanned/image-only PDF pages produce an explicit OCR-needed message; OCR is not included. Password-protected or malformed documents report conversion errors.
- Markdown is displayed as **source**, not rendered. Preview is limited to **200,000 Unicode characters**; copy and export always use the complete result. The view renders one row per line and is virtualized, so only the rows on screen are laid out; the text is not selectable, which is what buys that. Use **Copy all** or **Export .md** to get the content out.
- Results stay in memory while switching tools. No conversion history is saved automatically. Choosing a new file clears the old result when processing starts; cancelling a file picker preserves it.
- Export uses a temporary file in the destination directory and an atomic persist. The native dialog owns overwrite confirmation. Export refuses to replace the input file and requires a `.md` extension. No destination path is silently changed after confirmation.
- Markdown-only export does not bundle extracted image assets or promise the source document's visual layout. Complex layouts depend on anydoc's extraction quality.
- One operation runs at a time. Navigation remains available during work, but parsing cannot currently be cancelled. The input-size cap is not a hard cap on decompressed data or parser peak memory; anydoc supplies its own parser limits. A future worker process can add hard memory/time limits and cancellation for large workloads.

Try the small, repository-owned files in [`examples/`](examples/).

## JSON workbench

1. Paste JSON into **Input**, choose **Open file**, or load the built-in **Sample**.
2. The document is validated as you pause typing. The line under the editor reports what it contains, or where the first syntax error is.
3. Write a **jq filter** and press **Run** (or Enter) for an indented result, or **Minify** for a compact one. An empty filter is jq's identity, so leaving it blank simply formats the document.
4. **Copy all** copies the complete result and **Export .json** opens a native save dialog.

The validation line runs under both cards, with the **Strict (one value)** switch at its right. That switch decides what counts as a document: off (the default) reads several top-level values as a JSON stream the way jq does; on, a second value is an error.

The engine is [jaq](https://github.com/01mf02/jaq), a Rust implementation of the jq language; its standard library is available, including `map`, `select`, `group_by`, `@base64`, regular expressions and date functions. There is no module path, so a filter cannot reach the file system or the network.

Behavior and current limits:

- **Up to 4 MiB of JSON.** The editor is a single text item that is laid out as a whole, so the cap is a rendering budget as much as a parsing one. Files are read with the same bound, and non-UTF-8 files are refused.
- A document is parsed once into jaq's own value model, so the validator, the formatter and the query engine always agree, and integers too large for a `f64` keep their digits.
- Several top-level values are accepted, as in jq: a JSON stream or JSON Lines file runs the filter over each value in turn. Strictly, that is not one JSON document, so the status line calls it a stream and counts the values instead of reporting a single valid document. The **Strict (one value)** switch on that line turns it into an error instead, which is RFC 8259's definition of a JSON text. The setting applies to validation and to runs, and is not remembered between sessions.
- Syntax errors report a **line and column**; jq errors are separated into filters that could not be understood and filters that stopped on the input, and both keep the engine's own wording under a **Details** label.
- A run stops after **20,000 outputs or 5 seconds** and says so, so an endless filter such as `repeat(.)` returns a usable prefix instead of occupying the worker. A filter that loops without emitting anything cannot be interrupted; `halt` ends the run instead of ending the application.
- The result view is virtualized like the converter's, with the same **200,000 character** preview limit; copy and export always use the complete result.
- Export writes through a temporary file and an atomic persist, requires a `.json` extension, and refuses to replace the document that was opened. The native dialog owns overwrite confirmation.
- Results and input stay in memory while switching tools. Nothing is written to disk unless you export.

Try [`examples/sample.json`](examples/sample.json) with a filter such as `.tools | map(.key)` or `.tools[] | select(.ready) | .engine`.

## Hash & encrypt

One source file drives all three operations, because every file can be hashed, encrypted or decrypted; the switch at the top decides what is asked for beside it. Choose a file, drop one onto the window while this page is showing, or pass a path at launch. A `.age` file opens here in **Decrypt** whichever page you were on.

**Checksum** reads the file once and reports both SHA-256 and SHA-512. Paste a checksum into the field to verify: its length says which algorithm it is, so there is nothing to pick, and a whole line of `shasum` output works because only the digest field is read. The verdict is a green or red line under the result — a mismatch is stated plainly rather than left for you to compare by eye.

**Encrypt** and **Decrypt** use [age](https://github.com/str4d/rage) in either of its two modes:

- **Passphrase** (scrypt). The work factor targets about a second on this device, each way. There is no recovery: a lost passphrase is a lost file, which is why the field can be unmasked with **Show** before you commit to it.
- **Public key** (X25519). Paste one or more `age1…` recipients, one per line or separated by spaces; **Generate key pair** adds a fresh one and appends its public half to the field. The secret key is shown once, in the result panel, and is never written anywhere — save it yourself, or nothing encrypted to that public key can be opened again. Decryption takes the matching `AGE-SECRET-KEY-1…`.

Behavior and current limits:

- **Up to 2 GiB per file.** Everything streams in fixed chunks and is never held whole, so the cap is a patience budget: an operation cannot be cancelled once the worker has started it.
- The header says which kind of secret a file wants, so offering a key to a passphrase-protected file (or the reverse) is reported as exactly that, instead of age's generic "no matching keys".
- Output is written through a temporary file and an atomic persist. Encrypted output requires the `.age` extension, and neither direction will overwrite the file it is reading. The native dialog owns overwrite confirmation.
- Recipients and keys are parsed before anything is read or written, so a typo costs nothing but the message.
- Secrets live only in the page's fields and the worker command. Nothing is written to disk except the file you chose a destination for.

## Text diff

1. Paste or type into **Original** and **Changed**, choose **Open** on either side, or drop a file onto the window while this page is showing — it fills the empty side, and the changed side once both are taken.
2. The comparison runs on its own, shortly after you stop typing. There is no Compare button, because there is nothing to ask for.
3. Read the result line by line: each row carries its line number on the side it belongs to, a `+` or `-` marker and a tinted background.
4. **Copy diff** and **Export .diff** produce a standard unified diff with three lines of context.

Two switches decide what counts as a change, and both re-run the comparison immediately. **Ignore whitespace** treats lines that differ only in spacing as the same line — indentation and trailing spaces are rarely the change anyone is looking for. **Only changes** leaves out the untouched runs, replacing each with a row saying how many lines were skipped. **Swap sides** reads the same comparison the other way round, and takes each side's file with it.

The summary under the panel counts lines added and removed, the number of separate changes, and how similar the two sides are. When nothing differs it says so in green, and there is no diff to copy or export.

Behavior and current limits:

- **Up to 2 MiB per side.** Each side is an editor holding one text item, so the cap is a rendering budget as much as a comparison one. Files are read with the same bound, and non-UTF-8 files are refused.
- The comparison is **line-based**. Line endings are dropped before comparing, so the same content written on Windows and on Unix is identical rather than every line changed. Differences *within* a line are not highlighted: Slint 1.13 has no rich text, so there is nowhere to put the highlight.
- Myers' algorithm is quadratic in the worst case, so it stops refining after **5 seconds** and says so; the answer is still a correct diff, just not the smallest one. The panel shows at most **20,000 rows**, and copy and export always use the complete result.
- The two sides are compared as they are on screen, not as they are on disk: editing after opening a file compares what you edited. The file behind a side is remembered only to name it in the diff header, to suggest an export name, and to refuse to overwrite it.
- Export writes through a temporary file and an atomic persist, requires a `.diff` or `.patch` extension, and refuses to replace either file being compared. The native dialog owns overwrite confirmation.
- Both texts stay in memory while switching tools. Nothing is written to disk unless you export.

## Barcode reader

1. Select **Browse images**, drop an image onto the window, or pass one when launching. Any image opens here whichever page you were on, because an extension is enough to know what it is.
2. Reading starts on its own — there is nothing to configure about a barcode, so there is no Scan button.
3. The picture appears on the left with every code found in it boxed and numbered, and what they say on the right, in reading order. Clicking a row or a box highlights the same code in both.
4. **Copy** takes one code's text; **Copy all** takes every code, one per line.

The engine is [rxing](https://github.com/rxing-core/rxing), a Rust port of ZXing. It recognizes QR Code, Micro QR, Data Matrix, Aztec, PDF417, MaxiCode and the common 1D symbologies (EAN-8/13, UPC-A/E, Code 39, Code 93, Code 128, Codabar, ITF, GS1 DataBar). Images are read through the `image` crate: PNG, JPEG, GIF, BMP, WebP, TIFF, ICO, TGA, QOI and the PNM family.

Behavior and current limits:

- **Up to 32 MiB and 40 megapixels per image.** Recognition walks the whole picture several times, so the pixel count is the real budget — a small file can still decode into an enormous image, and the header is checked before anything is allocated.
- A picture with no code in it is an **answer, not an error**: the page says so on its status line and keeps the image on screen. The failures this tool can report are all about the file — missing, unreadable, or not an image.
- Recognition runs **try-harder** and also looks for codes printed light-on-dark. When the multi-code pass finds nothing, a second pass rescales and filters the image, which is what saves a photograph holding a single code. A code that is blurred, cropped or too small to resolve is still not found.
- Positions are reported as a **fraction of the image**, so the boxes stay correct however large the preview is drawn. A 1D code is located as a line across its bars, so its box can be flat; every marker gets a minimum size and stays centred on the code.
- The preview is the picture the codes were read from, scaled down to **900px on its longest edge** and handed to the page as pixels. The image file is opened once, on the worker, and never again on the event loop.
- Decoded text is shown in the UI font rather than the monospace one: a QR code holding Chinese is the ordinary case, and the monospace family has no glyphs for it.
- The result stays in memory while switching tools. Nothing is written to disk: this tool only reads.

Try [`examples/sample-codes.png`](examples/sample-codes.png), which holds a QR code, a Code 128 and an EAN-13 on one sheet.

## Clipboard

1. Select **Read clipboard** to keep whatever is on the clipboard right now: a text, a picture, or the files a file manager copied.
2. Switch **Collect automatically** on, and everything you copy from then on is kept here without asking. Switching it off leaves what has already been collected where it is.
3. Choose an item to see it in full — the whole text, the paths a file reference stands for, or the picture itself.
4. **Copy** puts that item back on the system clipboard, ready to paste. A picture also offers **Export .png**, because a picture is the one kind that cannot leave through the clipboard alone — pasting it needs somewhere that takes pictures, while a text or a path pastes anywhere.
5. **Remove** drops one item and **Clear** empties the workspace; neither touches the system clipboard.

Behavior and current limits:

- **In memory only.** Nothing is written to disk, there is no history file, and closing the window empties the workspace. Items survive switching tools, like every other tool's result.
- **Collecting automatically is opt-in.** A clipboard carries passwords and bank details as often as it carries a paragraph, so nothing is watched until the switch is on, and the switch is the only thing that starts it.
- Watching runs on a thread of its own, because the platforms disagree about what a clipboard change is: an event on Windows and X11, a `changeCount` to poll on macOS. That thread only reports *that* something changed; the read that follows is an ordinary job on the worker, in turn with everything else.
- The same content copied twice is **one item**, moved back to the top rather than kept again — including the copy the workspace itself makes when you put an item back.
- **Up to 60 items, 4 MiB per text, 32 MiB per picture and 128 MiB in total.** The oldest leaves when the workspace runs out of room, and what was just captured always stays, even when it is larger than the whole budget on its own.
- A picture is kept as **PNG**, which is what goes back on the clipboard, with a thumbnail of at most 480px on its longest edge for the panel to draw. The size written beside an item is the item's own; the thumbnail counts towards the budget but is not part of it, so the total under the list is the sum of the rows.
- **Export .png** writes the collected PNG itself, not the preview drawn beside it, through a temporary file in the destination's own folder and an atomic persist. A path without a `.png` extension is refused rather than corrected, because the native dialog has already asked about overwriting the path as it was typed.
- File references are decoded from whatever the platform hands over — a `file://` URI on one system, a plain path on another, percent-encoded either way. Copying them back puts the references themselves back, so pasting in a file manager pastes the files.
- Rich text and HTML are collected as the plain text that accompanies them. A clipboard holding only a format the workspace cannot keep is refused with a line saying so, and while collecting automatically it is passed over in silence, because no one asked for that read.
- The panel lays out at most **200,000 characters** of a text item, like every other preview here; copying back always uses the whole item. Text is shown in the display font rather than the monospace one, because what someone copied is as likely to be a Chinese sentence as a command. Paths keep the monospace family, where they line up.

## Image studio

1. Select **Choose folder**, drop a folder onto the window, or drop a single picture — a picture is taken as the folder it sits in, with that one on show, because that is what dropping one of forty photographs means.
2. The folder's pictures are listed on the left. **Click a row** to look at it: the picture appears on the right in one card with its size, format and colour, and what the camera recorded underneath it — a photograph and its caption, not two panels. **Tick a row** to include it in an export. The two are deliberately separate, so you can look through a folder without changing what you are about to write.
3. Choose an output format, and a width if you want one — the height follows. JPEG offers a quality; PNG offers the lossless optimizer.
4. **Export ticked…** asks for a folder first and only then starts: a cancelled dialog leaves the page exactly as it was. Once a folder is chosen it writes every ticked picture there with the same settings and says where it has got to; **Stop** ends it between pictures. When the batch finishes, a longer-lived global notice reports the requested, successful, skipped and failed file counts, plus the aggregate size before and after conversion.

Behavior and current limits:

- **Nothing is replaced, ever.** A batch has nobody to ask, so a name that is already taken in the destination is skipped and counted, and the picture that was read is never the picture that is written over. Exporting into the source folder is therefore fine when the format differs, and a no-op when it does not.
- Only the chosen folder is read — never what is inside the folders in it. A batch should act on what someone can see in the list.
- **Up to 1,000 pictures in a list**, and **64 MiB and 50 megapixels per picture**. Listing reads headers only, so a folder of five hundred photographs is a matter of milliseconds; working on one means holding it uncompressed, which is why the per-picture caps are what they are.
- A batch reads, renders and writes **one picture at a time**, so its memory stays flat whatever the folder holds, and it can be stopped between pictures.
- Reads PNG, JPEG, GIF, WebP, TIFF, BMP, ICO, TGA, QOI and PNM; **writes PNG, JPEG, TIFF and BMP**. WebP is missing from the output list because a pure-Rust encoder for it does not exist: this build can read it and cannot write it.
- **An exported picture carries no EXIF at all** — no camera, no serial number, no coordinates. The encoders write pixels only. What the original carries is listed under it, because someone about to share a photograph should know what is in it.
- A photograph taken sideways is turned the right way up as it is opened, so the thumbnail, a resize and the exported file all agree with what the camera showed.
- Resizing keeps the proportions, so a width is the only thing to choose. It goes through RGBA with a high-quality filter; for PNG output the optimizer puts a grayscale or paletted picture back the way it was, so the detour costs nothing where it would have shown.
- The PNG optimizer runs at **level 2**, and its result is kept only when it is smaller. The optimizer's own default walks far more filter and compression combinations for a few per cent, and this runs while someone waits.
- The picture being shown is decoded once for its preview and metadata. Batch export reads, transforms and writes each ticked picture in turn.
- Two tools take a picture and two take a folder, so the page you are on decides: an image or a folder dropped while this page is showing opens here, and otherwise goes to the barcode reader or to Disk cleanup.

## Archives

1. **Open an archive** — select **Browse archives**, drop a `.zip` or `.7z` onto the window, or pass one when launching. Reading starts by itself and costs the table of contents, not the content: a 2 GiB archive opens as fast as an empty one.
2. Everything it holds is listed by name, with what each entry unpacks to and the time the archive recorded. Every entry that may be written is ticked to begin with, because unpacking adds files rather than replacing them. **Extract ticked…** asks for a folder and only then starts; **Stop** ends it between entries.
3. **Create one** — choose a folder, or drop one on this page. Its files are listed under the name they would carry inside the archive, choose **ZIP** or **7z**, and **Create archive…** asks where to save it.

Each direction keeps its own list and its own selection, so switching between them does not re-read anything.

Behavior and current limits:

- **A name inside an archive is not a path.** It is a string somebody else wrote, and the whole risk of unpacking anything is that the string says `../../.ssh/authorized_keys`. An absolute name, a `..` component, a drive letter and a symlink entry are refused outright — shown in red, impossible to tick, never written — and what would be written is checked again after the folders exist, so a link standing anywhere above the destination cannot redirect it either. Such names are counted in the notice rather than quietly repaired: a file that lands somewhere other than where the archive said is its own kind of surprise.
- **A name is read the way it was written.** A ZIP stores a name as bytes plus one flag saying whether they are UTF-8, and a great many archivers — `zip` and Info-ZIP among them — write UTF-8 without setting it. The format's own fallback for the unflagged case is CP437, the code page of MS-DOS, which turns `使用说明.md` into `Σ╜┐τö¿Φ»┤µÿÄ.md`; that is not a display problem, because this name is also what gets written to disk. So valid UTF-8 is read as UTF-8 whatever the flag says, and a name that is not UTF-8 — which predates it and carries nothing to say which code page it is — is read as GBK: a guess, and the one that leaves a Chinese name readable in a build that ships in Chinese. A 7z records UTF-16 and needs none of this.
- Entries are unpacked **by their position in the archive**, not by name, so two entries recording the same name are two entries: the first is written and the second finds the name taken.
- **Nothing is replaced, ever.** A name already taken in the destination is skipped, which is what makes extracting the same archive twice harmless. The notice names the first such entry rather than only counting it: a file that was skipped is a file that is already there, and a bare count reads as a failure — the usual cause is extracting beside the folder the archive was made from, where every path lands on its own original.
- **Up to 2 GiB and 20,000 entries per archive**, and **one extraction writes at most 16 GiB**. The declared total is refused before a byte is written, and the writing itself stops at the same budget, because a header can lie about how much is inside — that lie is the whole of a decompression bomb. A file cut off at the budget is deleted rather than left as a plausible one.
- The format is decided by the **first bytes, not the extension**: a 7z named `.zip` opens as a 7z.
- Reads and writes **Stored, Deflate and LZMA** inside a ZIP, and **Copy, LZMA and LZMA2** inside a 7z; new archives are written as Deflate (ZIP) or LZMA2 (7z). A 7z using BZip2, PPMd, Zstd or Brotli reports the codec rather than failing vaguely: those decoders are cargo features that would add a codec stack for formats almost nothing in the wild uses.
- **Password-protected archives are not supported** in either direction. One is reported as encrypted rather than half-opened, and nothing written here is encrypted; [Hash & encrypt](#hash--encrypt) is where a file gets a passphrase.
- Packing leads every entry with **the chosen folder's own name**, so unpacking gives the folder back whole instead of scattering its contents. Links are never followed — what a link points at belongs to someone else — and an **empty folder is recorded as itself**, because it is the one thing a list of files cannot say. Writing the archive into the folder being packed is refused; it would ask the archive to contain itself.
- A new archive is written through a temporary file beside its destination and moved into place at the end, so an interrupted run leaves no half-written archive where a real one is expected. The native save dialog owns overwrite confirmation, and the extension must match the chosen format.
- On Unix the **executable bit travels both ways**, and only that: an archived program that comes out unable to run is a broken archive, while the rest of a recorded mode — a set-user-id bit above all — is not something to restore on trust. Times are shown as the archive recorded them and are not converted: a ZIP keeps local time with no zone, a 7z keeps UTC, and this build carries no timezone database. New ZIP entries are therefore written as UTC.
- Extraction and packing each run as **one job that reports where it has got to** and can be stopped between entries. Reading a 7z decodes whole blocks, so an entry that was not ticked is still read past — in a solid archive the data of one file depends on the file before it.
- A `.zip` or `.7z` dropped anywhere opens here. A folder opens here only while this page is showing, since a folder means something to three tools.

## Disk cleanup

1. Select **Choose folder**, drop a folder onto the window, or pass one when launching. The scan starts by itself: choosing a folder for this tool is asking what is in it.
2. Pick what to look for — **Duplicates**, **Empty** or **Biggest**. Switching reads the folder again for the new question: a result belongs to the mode it was read with, so the old one goes rather than being relabelled. **Scan again** re-reads the folder after something changed on disk.
3. Tick what may go. **Keep one of each** ticks every copy but the first in each group, and **Select all** does the obvious thing for empty files and folders. Neither is offered for the biggest files: those are large rather than unwanted, and only a person can say which of them may go.
4. **Move to trash…** turns the line under the list into a sentence naming exactly what would go and where it would go to. **Yes, move them** carries out that sentence; **Cancel** leaves everything alone.

Behavior and current limits:

- **Nothing is unlinked.** What goes, goes to the platform's own trash — the macOS Bin, the Windows Recycle Bin, the freedesktop trash on Linux — so a wrong click is recoverable where people already look. The whole selection moves in **one operation**: one undo step, and one Finder trash sound rather than one per file.
- Duplicates are decided by **content alone**. Files of the same size are separated by a SHA-256 of their first 64 KiB, and only what still collides is read in full and compared. Names, folders and timestamps mean nothing to it, and a file that cannot be read is left out rather than grouped with the files it failed to match.
- **A duplicate group always keeps a copy.** Selecting every copy of a group is refused with a line saying so: a tool for removing extra copies must not be the thing that removes the last one.
- Every path is checked again immediately before it is trashed — still inside the folder that was scanned, still a real file rather than a link, still the size the scan recorded, and an empty folder still empty. Anything that changed in between is skipped and counted in the notice instead of being removed on an old assumption.
- **Links are never followed, compared or removed.** A link is not a copy of what it points at, and one pointing back into its own parent would otherwise be walked forever.
- Version control and system folders are skipped: `.git`, `.hg`, `.svn`, `.Trash`, `.Spotlight-V100`, `.fseventsd`, `$RECYCLE.BIN` and `System Volume Information`. A repository's copies are deliberate and mean something.
- **Up to 200,000 files, 16 GiB read and 60 seconds per scan**, and 64 levels deep. A scan that reaches a bound hands back what it found and says the answer is partial rather than pretending to be complete; the largest files are compared first, so a scan that runs out of budget has spent it where the space actually is. The biggest-files view lists the top 100.
- Empty folders are reported at the top of their tree. Removing one takes what is under it, and listing every level would ask for the same folder three times.
- Nothing is selected by default, and the selection survives a language change. Removing from a result drops exactly what went to the trash instead of reading the folder again, so a group left with one copy stops being a finding.
- A scan can be **stopped by asking something else**: switching the question while one is running raises a flag the scan reads between files and between the blocks of a file it is hashing, so the answer to the new question starts after one block rather than after the whole folder. The tabs stay usable during a scan for that reason; everything else on the page waits, as it does for any operation. Results stay in memory while switching tools and are never written to disk.
- **czkawka was the plan and is not used.** Its current versions need Rust 1.92 or 1.94 while this workspace pins 1.88, and the newest version that does build brings an audio, video and RAW-image stack — around fifty crates, with its own logger, localization and cache files — for three kinds of scan that are a few hundred lines of `tools/cleanup.rs`. Similar-image detection was the part of that tree worth having, and it is not in this version.

## Architecture

The workspace separates native UI concerns from reusable tool operations:

```text
crates/
  handybox-core/                 # No Slint or OS dialogs
    src/catalog.rs              # ToolId + metadata: single source of truth
    src/tools/documents.rs      # Validation, anydoc adapter, result, export
    src/tools/json.rs           # Parsing, formatting, jaq adapter, bounded runs
    src/tools/crypto.rs         # Streaming digests, age encryption and decryption
    src/tools/diff.rs           # Line comparison, diff rows and unified output
    src/tools/codes.rs          # Image decoding, rxing adapter, symbols and preview
    src/tools/clipboard.rs      # Captured items, their limits and the session history
    src/tools/cleanup.rs        # Folder walk, duplicate grouping, checked removal
    src/tools/images.rs         # Decoding, EXIF, resizing, encoding and PNG optimization
    src/tools/archives.rs       # ZIP and 7z listing, checked extraction and packing
    tests/catalog.rs            # Stable routes and implementation status
    tests/documents.rs          # Conversion and file-safety integration tests
    tests/json.rs               # Formatting, queries, limits and export safety
    tests/crypto.rs             # Digests, verification and age round trips
    tests/diff.rs               # Rows, options, unified output and export safety
    tests/codes.rs              # Recognition, positions, the preview and file limits
    tests/clipboard.rs          # Duplicates, eviction, summaries and file references
    tests/cleanup.rs            # Content grouping, skipped folders, links and removal rules
    tests/images.rs             # Formats written, resizes, optimization and export safety
    tests/archives.rs           # Round trips, hostile names, selection and stopping
  handybox-desktop/
    build.rs                    # Compiles the Slint component tree
    src/main.rs                 # Startup, monospace font, optional initial paths
    src/macos.rs                # macOS Dock icon and light title bar
    src/locale.rs               # Tool translations, typed messages, UI font per language
    src/settings.rs             # Atomic, local language preference
    src/renderer.rs             # Winit GPU preference / CPU fallback
    src/controllers/mod.rs      # Shell: routing, language, notices, event pump
    src/controllers/documents.rs# Converter state, callbacks and events
    src/controllers/json.rs     # Workbench state, validation clock and events
    src/controllers/crypto.rs   # Source file, the three operations and their report
    src/controllers/diff.rs     # Two sides, their files and the comparison clock
    src/controllers/codes.rs    # The picture, its codes and what is highlighted
    src/controllers/clipboard.rs# The workspace, the selection and the collecting switch
    src/controllers/cleanup.rs  # The folder, the findings, the selection and the confirmation
    src/controllers/images.rs   # The picture list, preview, selection and export recipe
    src/controllers/archives.rs # The two directions, their lists and their selections
    src/worker/mod.rs           # Bounded background command/event bridge
    src/worker/documents.rs     # Pickers, conversion and Markdown export
    src/worker/json.rs          # Validation, jq runs, file open and export
    src/worker/crypto.rs        # Pickers, digests, age encryption and decryption
    src/worker/diff.rs          # Pickers, comparisons and unified-diff export
    src/worker/codes.rs         # Image picker and recognition passes
    src/worker/clipboard.rs     # Clipboard reads, writes and the watching thread
    src/worker/cleanup.rs       # Folder picker, scans and trash moves
    src/worker/images.rs        # Image picker, decoding, rendering and export
    src/worker/archives.rs      # Archive and folder pickers, extraction and packing
    ui/app.slint                # Shell only: sidebar + active page + status
    ui/theme.slint              # Shared palette, fonts, spacing, radius
    ui/icons.slint              # Embedded SVG asset catalog
    ui/i18n.slint               # Reactive English / Chinese component copy
    ui/assets/                 # Original SVG line icons and empty-state artwork
    ui/types.slint              # UI-facing data models
    ui/state/documents.slint    # Converter properties and callbacks
    ui/state/json.slint         # Workbench properties and callbacks
    ui/state/crypto.slint       # Hash & encrypt properties and callbacks
    ui/state/diff.slint         # Text diff properties and callbacks
    ui/state/codes.slint        # Barcode reader properties and callbacks
    ui/state/clipboard.slint    # Clipboard workspace properties and callbacks
    ui/state/cleanup.slint      # Disk cleanup properties and callbacks
    ui/state/images.slint       # Image studio properties and callbacks
    ui/state/archives.slint     # Archives properties and callbacks
    ui/components/              # Reusable visual building blocks
    ui/pages/documents.slint    # Composes the converter page
    ui/pages/json.slint         # Composes the workbench page
    ui/pages/crypto.slint       # Composes the hash & encrypt page
    ui/pages/diff.slint         # Composes the text diff page
    ui/pages/codes.slint        # Composes the barcode reader page
    ui/pages/clipboard.slint    # Composes the clipboard workspace page
    ui/pages/cleanup.slint      # Composes the disk cleanup page
    ui/pages/images.slint       # Composes the image studio page
    ui/pages/archives.slint     # Composes the archives page
    ui/pages/placeholder.slint  # Metadata-driven planned-tool page
assets/app-icon.png              # Rounded icon: sidebar and window
assets/macos-icon.png            # Icon on Apple's grid: Dock and .icns
Taskfile.yml                     # Run, check, test and release-build entry points
scripts/macos-bundle.sh          # Packages HandyBox.app into a zip
scripts/zip-binary.sh            # Zips the Windows and Linux executables
scripts/Dockerfile.cross         # Build-only Windows/Linux cross toolchain
```

```text
Slint components → callbacks → controller → bounded command channel
                                               ↓
                                       background worker
                                               ↓
                                core tool / OS dialog / clipboard
                                               ↓
Slint properties ← controller ← 50 ms event pump ← result channel
```

**Core:** public operations use Rust inputs/results and know nothing about windows. The catalog gives every tool a stable enum ID and string route; navigation does not depend on menu indexes. Engines are adapters inside `tools/`, so a future CLI or tests can reuse them. Each tool exposes its own stable issue enum (`DocumentIssue`, `JsonIssue`, `CryptoIssue`, `DiffIssue`, `CodesIssue`, `ClipboardIssue`, `CleanupIssue`, `ImageIssue`, `ArchiveIssue`) instead of error strings the desktop would have to match on. Geometry leaves the core normalized rather than in pixels, so a barcode's position survives being drawn over a scaled preview. The rules that decide what may be deleted live there too, beside the scan that produced the list, rather than in the page that shows it.

**Desktop controllers:** `controllers/mod.rs` is the shell. It translates catalog entries into Slint models, filters navigation case-insensitively by tool name or engine, owns the language and the single notice surface, and pumps worker events to whichever tool they belong to. The controllers travel together as one `Tools` value, so routing a dropped file, relabelling after a language change and dispatching an event each take a field rather than another parameter. A dropped folder goes to whichever of the three tools that take one is showing — the image studio lists the pictures in it, Archives packs it — and to Disk cleanup otherwise. A dropped file goes to the tool whose page is showing when that tool takes any file at all (crypto, diff) or when it is a picture and the image studio is showing, then by extension claim (`.age`, `.zip`/`.7z`, images, `.json`), and otherwise to the converter. A tool controller owns its own retained state, registers its own callbacks and re-renders its own text on a language change; it reaches the shell only through `submit`, `dispatch`, `finish` and `notify`. Weak component handles avoid UI ownership cycles. Only the main thread touches Slint properties.

**Worker:** one named thread processes a bounded channel with nonblocking submission from the UI. File pickers, parsing, clipboard access and export run there, grouped per tool in `worker/`, with the clipboard context shared between them. Watching the clipboard is the one thing that cannot: it blocks until it is stopped, so it gets a thread of its own that reports a change back through the same command channel — never blocking in it, because a button press needs the same slot — and the read that follows runs on the worker like any other job. That thread exists only while the switch is on; dropping its handle ends it. Commands own their inputs and events own their results. Recoverable errors and parser panics clear the busy state. `submit` takes the busy state for a user action and refuses a second one; `dispatch` is for background work such as the workbench's validation, which never blocks the interface and simply waits for a free turn. The event timer is owned by the application for the entire window lifetime. There is no server or networking service.

**Adding a tool:** mark it `available` in the catalog, add a module under `tools/`, a command/event module under `worker/`, a controller under `controllers/`, a state global under `ui/state/` and a page under `ui/pages/`. The shell routes it; nothing else has to change.

Outcomes appear as a `Toast`: a floating, self-expiring notice near the bottom of the window. It is a sibling of the layout rather than a row inside it, so showing and hiding it can never reflow the page or take height from the result panel. It is never created or destroyed either, which lets it fade both in and out; a replacement notice restarts the countdown instead of inheriting it. Errors linger longer than confirmations, and a click dismisses either. `StatusBar` is deliberately static: it is shared by every tool, so a transient message there would have to be cleared on navigation.

**UI composition:** pages bind to their tool's state global and emit its callbacks; they do not perform file I/O or reference engine APIs. Shell-wide properties (`busy`, `dragging`) still flow down from `app.slint`, which only composes the shell and routes. A global per tool keeps the window's own surface to what every tool shares, instead of growing a property per field. `Sidebar` composes `NavItem` and `TextField`; `DocumentsPage` composes `PageHeader`, `FileCard`, format `Badge`s and `SourcePanel`; `JsonPage` composes `PageHeader`, `TextField`, `JsonInput`, the same `SourcePanel`, and a validation row of its own with a `Toggle`; `SourcePanel` composes `Action`, `CodeView` and `EmptyState`, and `JsonInput` composes `TextEditor`; `DiffPage` composes `PageHeader`, two `Toggle`s, an `Action`, two `DiffInput`s and a `DiffPanel`, which is a result card of its own because a diff row is a gutter, a marker and a line rather than a string. `ImagesPage` composes `PageHeader`, the same `FileCard`, an `ImageList` and — stacked on the right — a `PicturePanel` over a `DetailList`, with the output settings and the two presses as rows of the page: the size a render reported has to sit beside the controls that produced it. Its rows carry two selections, ticked and shown, because what a batch would write and what the page is displaying are different questions. `CleanupPage` composes `PageHeader`, a `Segmented`, the same `FileCard` and a `CleanupList`, with the confirmation as a row of the page rather than a dialog: the sentence and the buttons that act on it sit where the selection is being made. `ArchivesPage` composes `PageHeader`, a `Segmented` for the direction, the same `FileCard` and one `ArchiveList` that serves both — a second list would be the same list with different words on it, so the rows carry what they mean and the page relabels around them. A refused row has no box to tick at all, because what may be written is a property of the name rather than of a checkbox. `ClipboardPage` composes `PageHeader`, a capture card of its own with a `Toggle` and two `Action`s, a `ClipList` and a `ClipDetail`; the detail card reuses `CodeView` through a `family` property, because a clipboard item is someone's own words as often as it is a path, and its export action exists only for a picture. A row clips its one-line preview rather than eliding it: eliding cuts back to the last word boundary, and a Chinese sentence has none, so an elided row would show only the words before its first space. `CodesPage` composes `PageHeader`, the same `FileCard`, a `CodePreview` and a `SymbolList`: the preview sizes a stage to the picture's own aspect ratio so that a fraction of the image is a fraction of the stage, and each marker is placed on it by centre with a minimum size, because a 1D code is located as a line rather than a box. Which code is highlighted is view state and lives in the state global, so clicking a row or a marker points both cards at the same code without a round trip through Rust. The workbench's two cards, and the diff tool's two sides, each sit in a plain `Rectangle` and size themselves to it: a layout distributes width by its children's preferred sizes, so a card holding a sentence would keep growing at its neighbour's expense. That is also why the validation line is a row of the page rather than part of a card. `CodeView` and `DiffPanel` put their lines in a `ListView`: a `for` directly inside one compiles to a virtualized repeater, which is the only reason a large document stays responsive — a single `Text` holding it all froze the window for about ten seconds on a 26k-character Chinese document, because FemtoVG shapes the whole string to measure it and its 1000-entry shaped-word cache thrashes on text without word breaks. `TextEditor` is the exception: editing needs one cursor and one selection, so the workbench's input and each side of a comparison are a single `TextInput`, and the 4 MiB and 2 MiB caps are what keep that affordable. `StatusBar` is shared across tools. A visual change belongs in the smallest relevant component, with shared tokens in `Theme`.

The workspace deliberately uses a small typed command/event bridge rather than a dynamic plugin ABI or a generic JSON dispatcher: a new tool adds variants the compiler checks, not a registry it trusts.

### Branding and icons

The brand artwork ships as two prepared icons. The theme's navy accent and soft cream surface follow their palette.

| File | Canvas | Artwork | Used by |
| --- | --- | --- | --- |
| `assets/app-icon.png` | 256px | Fills the canvas | Sidebar `Brand`, Slint window icon |
| `assets/macos-icon.png` | 512px | 80.5%, transparent margin | Runtime Dock icon, bundled `AppIcon.icns` |

Both are rounded at the 22.37% radius macOS uses for application icons. The macOS variant additionally follows Apple's icon grid, where the artwork covers 824 of 1024 points and the rest is transparent; without that margin the Dock tile renders noticeably larger than every neighbouring application. The sidebar and window icons size their own box, so the same margin there would only shrink the mark, which is why there are two.

Both canvases are deliberately small, because each file is compiled into the executable: 256px covers a 46pt sidebar mark on a 2x display and 512px covers the largest Dock tile. Every doubling roughly quadruples the embedded bytes — at 1024px the two icons alone added 1.7 MB to every binary. Replacing the artwork means preparing both files at those sizes.

`Brand` owns the sidebar identity. `Icon` displays and tints the bundled SVGs, while `ToolIcon` maps stable tool routes to icons in the desktop layer. `Action` accepts an optional image and retains its text label and keyboard/accessibility behavior. `TextField`, status indicators, file actions and placeholders reuse these components. The SVGs are original project assets with a consistent 24 × 24 view box, 1.7px rounded strokes and no external fonts or network requests. The one exception is `github.svg`, GitHub's own mark, included under their brand guidance solely to label the link to this repository; it is a filled silhouette rather than a stroked outline. The larger document empty state has its own SVG illustration.

`Theme`'s palette is light only. On macOS that would leave a near-black title bar above a light window in Dark Mode, so `macos.rs` pins the application appearance to Aqua and sets the window theme to light. Slint's own `set_color_scheme` cannot do the second part: its winit backend compiles the theme call out on Apple targets (`use_winit_theme`) and leaves the decoration following the system. The window must exist first, so `main` shows the component, sets the title bar, and then runs the event loop.

To add an icon, put an SVG in `ui/assets/icons/`, register it in `ui/icons.slint`, and use `Icon` or the `Action.icon` property. Add the tool-route mapping in `ToolIcon` for a new menu item. Keep icon assets and visual mappings out of the core library. Both renderers use the same embedded assets.

### Localization

`SettingsMenu` is a reusable sidebar component: a gear button beside the wordmark that opens a `PopupWindow` with the two languages. A row of language buttons cost a whole line of sidebar height for a setting that is changed rarely. The labels stay self-named (`English`, `中文`) so they are recognizable whichever locale is active. `ui/i18n.slint` centralizes static component text, with reactive bindings to `I18n.chinese`. Rust's `locale.rs` owns translated catalog metadata, status messages and application error descriptions. Core errors expose stable `DocumentIssue`, `JsonIssue`, `CryptoIssue`, `DiffIssue`, `CodesIssue`, `ClipboardIssue`, `CleanupIssue`, `ImageIssue` and `ArchiveIssue` categories; translation never relies on matching English error strings. A category may carry data — a JSON syntax error carries its line and column — so the position survives a language change too. Worker events carry typed messages/results and are translated using the current language when displayed, including after switching during a conversion. File paths, engine names and document content are never translated.

The proportional UI font follows the selected language (`Language::font_family`): PingFang SC, Microsoft YaHei or Noto Sans CJK SC for Chinese, and the previous Latin families for English. This is a performance requirement, not only a typographic one. When the primary font lacks a single glyph, the FemtoVG renderer re-queries the system fallback list for that text item on every layout and every frame — measured at ~0.6 ms per item on macOS, plus a large one-time cost to load a CJK face and rasterize its glyphs into the atlas. UI copy must therefore stay within its font's coverage; `DOCUMENT TO MARKDOWN` avoids `→`, which Helvetica Neue does not provide.

Native file-dialog titles and filter labels use the selected language when opened; native buttons and OS dialogs still follow the operating system's language. Low-level OS/parser diagnostics retain their original text under a translated **Details** label. Startup technical diagnostics and CLI help remain in English.

The language code (`en` or `zh-CN`) is saved atomically to a `language` file in the platform config directory (`directories::ProjectDirs`): `~/Library/Application Support/dev.handybox.HandyBox/` on macOS, `%APPDATA%\\handybox\\HandyBox\\config\\` on Windows, and `$XDG_CONFIG_HOME/handybox/` or `~/.config/handybox/` on Linux. Missing/invalid settings default to English. No document contents are stored there. If saving fails, the selected language still works for the current session and the status bar reports that it could not be remembered.

For a new tool, add both translations in `tool_text`; for new static UI text, add a property in `ui/i18n.slint`. Text shown in the monospace family — jq expressions and their placeholder — stays ASCII, because that family has no CJK coverage. Add new runtime messages as typed `Message`/`FailureKind` variants with both translations. Do not insert translated output into worker events or recreate the window to change language.

## Add a tool

1. Add a stable `ToolId` and `ToolDescriptor` in `handybox-core/src/catalog.rs` (or use the existing placeholder). Define the name, description, engine, planned capabilities and availability. A new unavailable entry automatically gets navigation and a placeholder page.
2. Add `tools/<tool>.rs` with typed inputs, outputs and a public operation. Keep Slint and native dialogs out of the core. Add the engine dependency only when the implementation actually uses it.
3. Add `worker/<tool>.rs` with the tool's command and event types and an `execute` entry point. Keep queue admission bounded; expensive work must not run in UI callbacks. Bound anything that can run away — outputs, time, input size — and report the bound in the result instead of hanging. Add progress/cancellation deliberately for scans and batch operations.
4. Add `controllers/<tool>.rs` to bind callbacks and retain the tool's state, and register it in the shell's event pump. Always validate a route against the catalog. Tool state must survive page switches.
5. Create `ui/state/<tool>.slint` for the tool's properties and callbacks and `ui/pages/<tool>.slint` for its layout, reuse the shared components, and add a route in `ui/app.slint`. Keep page-specific layout inside that page. Extend `ui/types.slint` only for data the UI displays.
6. Test real input/output and failure behavior. For destructive tools, require a concrete result preview and an explicit action before modifying files. For archives, validate extracted paths and expanded size. Clipboard monitoring must be opt-in.
7. Mark the catalog entry available, update the workspace tool count if it changes, and update this README's status table. An available entry must have a working page and controller.

## Rendering

The renderer setup is fallback-capable and verified against the Slint 1.13.1 backend source. Both **FemtoVG/OpenGL** and **software** renderers are compiled. Winit prefers FemtoVG; if window/surface creation cannot initialize it, the backend tries the software renderer. Keeping this fallback at window creation matters: merely catching an error during backend selection is too early.

`SLINT_BACKEND=winit-software` requests CPU rendering, and `SLINT_BACKEND=winit-femtovg` requests GPU rendering with the same backend fallback mechanism. `winit` or an unset variable uses the default. Unsupported backend names return a startup error. The direct `i-slint-backend-winit` dependency is version-coupled to Slint and must be upgraded together with `slint-build`.

The UI uses basic rectangles, borders and text instead of effects that depend on GPU-only rendering. Automatic startup fallback cannot recover every mid-session driver failure. GPU and forced-CPU sessions should both be checked on target machines.

References: [Slint renderers](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/), [pinned Winit backend](https://docs.rs/i-slint-backend-winit/1.13.1/i_slint_backend_winit/), [anydoc API](https://docs.rs/anydoc/0.2.4/anydoc/).

## Verification

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Integration tests cover CSV Unicode/table output, content-detected RTF, a real DOCX ZIP container, invalid/empty/missing/oversized input, full-result export, source-overwrite protection, symlink protection on Unix, confirmed destination replacement, Unicode-safe preview truncation and catalog consistency. The JSON tests cover formatting and minification, jq filters over the standard library, oversized integers, JSON streams, syntax positions, the three error categories, the output bound, `halt`, document summaries and export safety. The diff tests cover per-side line numbering, identical sides and line endings, relaxed whitespace matching, the context gaps of a changes-only view, unified output including empty ranges, the row and size bounds, file loading and export safety. The barcode tests draw their own codes and read them back: a QR holding non-ASCII text and where it sits, several symbologies on one sheet in reading order, a code printed light-on-dark, an image with nothing in it, the scaled preview beside unmoved positions, the extension claim, and the file, format and pixel limits. The clipboard tests cover the same content arriving twice, eviction by item count and by bytes, the item that always stays, the room a removal gives back, the size an item is shown by against the memory it holds, empty and oversized captures, one-line summaries of text, files and pictures, file references decoded from URIs including a Windows drive letter and a per cent sign that is not an escape, and saving a picture: the bytes that were collected, the refused extension that leaves nothing behind, and a replaced file. The cleanup tests build folders and read them back: duplicates found by content across names and folders, same-sized files that are not copies, long files that begin alike and end differently, skipped version-control folders, empty trees reported at their top, the biggest files in order, and the rules that stand between a selection and the trash — nothing selected, every copy of a group selected, a file that changed or vanished since the scan, a path outside the folder, a folder that is no longer empty, and a link standing where a file was. They never call `remove`: moving a file to the trash would leave it in the trash of whoever ran the tests. The archive tests assemble ZIPs byte by byte to pin the names down — UTF-8 with the flag, the same UTF-8 without it, a legacy GBK name and plain ASCII all come back readable, and two entries recording one name stay two entries — and pack the folders they read back, in both formats: what went in comes out byte for byte, an empty folder survives the round trip, extracting twice replaces nothing, only the ticked entries are written, and a program comes back out a program. The hostile names are written by hand — `../escaped.txt`, `nested/../../escaped.txt`, an absolute path and a drive letter, plus a symlink entry — and are refused with nothing written beside the chosen folder; a unit test gives one entry a budget smaller than its content and checks that the truncated file is taken back rather than left as a plausible one. The image tests list a folder and check that only the pictures in it are offered, by name and without descending into it, that a batch writes each picture under its own name and refuses a name that is taken or the picture it came from, render a picture into every format this build writes and read each one back, check that a resize keeps the proportions and that a width of zero is refused, that the optimizer never adds bytes and never changes a pixel, that quality is what a JPEG costs, and that an export refuses the wrong extension and the picture it came from.

Localization tests cover both languages for every tool, bilingual/case-insensitive search, delayed-message translation with unchanged paths, localized error categories including a JSON position, the split between a diff's input and operation failures, the barcode reader's file-level failures, the clipboard's split between what was on it and reaching it at all, the archive failures a password and a decompression bomb produce together with what an extraction and a new archive report, and preference round-trips/defaults. Manually verify switching before and after conversion, switching on a placeholder, and restarting after choosing a language.

The CI workflow runs these checks on macOS, Windows and Linux. CI does not perform native window interaction. For manual UI verification, check all nine navigation items, search (including no matches), file picker cancellation, conversion after an error, copy, export/overwrite confirmation, long results, page switching and forced CPU rendering. For the workbench, check the validation line while typing, the strict switch on a stream, a failing filter, a filter that outlives its bound, and that a result survives navigating away and back. For the barcode reader, check a sheet holding several codes, a photograph holding one, an image holding none, a portrait and a landscape picture in the same window, and that clicking a row moves the highlight in the preview. For Archives, check an archive holding a folder, a cancelled destination dialog, extracting the same archive twice into one folder, stopping a long run, and that switching between the two directions keeps each list and each selection. This implementation is built and tested locally on macOS; other-platform CI results must be checked when the workflow runs.
