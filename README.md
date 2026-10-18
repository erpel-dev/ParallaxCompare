# ParallaxCompare

A fast side-by-side folder and file compare tool for Windows and Linux, in the spirit of Beyond Compare. Folders, archives and Git revisions are all *sources*, and any source can be compared with any other:

- a folder against a folder
- a `.zip` / `.7z` / `.tar.gz` against a folder, or against another archive
- branch `main` against branch `feature/x`, read straight from the repository with no checkout
- a Git revision against the working tree
- archives nested inside folders, inside other archives, or stored in a Git commit, which expand like folders

![Folder compare: Git branch against Git branch, with a zip archive expanded inline](docs/screenshots/folder-compare.png)

![Text compare with syntax highlighting and changed characters marked within each line](docs/screenshots/text-compare.png)

The design rationale, the GUI mockups and a comparison with existing tools are in [docs/concept.html](docs/concept.html).

## Features

| | |
|---|---|
| **Sources** | Local folders and files · zip and zip-based formats (jar, war, nupkg, vsix, whl, apk …) · 7z · tar, tar.gz, tar.xz, tar.zst, tar.bz2 · Git branches, tags, commits and `HEAD~n` · Git working tree (honours `.gitignore`) |
| **Folder compare** | Aligned two-pane tree with missing-side placeholders · filters: all, differences, same, orphans · **Flat**: all files in one list sorted by name, each with its folder; a file found on only one side each is matched with the file of the same name in the most alike folder, compared and marked ↷, and opening, copying (over its match) and deleting work on the pair · compare by content (default) or by size and date · **≈ similar** status for files whose bytes differ but whose content is equal (CRLF vs LF, re-saved documents with the same text, re-encoded images with the same pixels, Java classes recompiled with only debug info changed) · exclude globs · select several rows (Ctrl/Shift+click) to copy left↔right or delete at once, with progress and Cancel · copying asks first when it replaces files, and names those newer than the copy · deleting moves files to the Recycle Bin / Trash (inside archives it is for good, and the dialog says so) · rows update by themselves when files change on disk |
| **Git** | Compare any two revisions without a checkout · *merge-base* mode shows only what a branch changed since it branched off · identical folders are recognised by their tree id and skipped without being walked |
| **Content views** | Each file type is shown in the form that makes its changes readable, with raw text or a hex dump always one click away. See the table below. |
| **Text compare** | Line diff (Myers, or histogram as an option) with aligned rows and placeholders · changed words marked within each line · whitespace and case rules · overview strip · previous/next difference (F7/F8) · find in both files (Ctrl+F; match case, whole words, regular expressions; Enter/F3 for the next match, matches marked in the overview strip) · line-detail pane · encoding (UTF-8/16, legacy code pages) and line-ending detection · binary detection |
| **Editing** | Copy a difference to the other side with the arrows next to it or Alt+←/→ · double-click a line to edit it (Enter splits, Backspace/Delete join, multi-line paste) · select lines with Shift+click or Shift+↑/↓ (Ctrl+A all), then Ctrl+C / Ctrl+X to copy or cut them, Delete to remove them, Ctrl+V to replace them; with one line current, Ctrl+V inserts above it · undo/redo (Ctrl+Z/Ctrl+Y) · save a side with the 💾 button next to its "● modified" mark, or all changed sides with Ctrl+S, also into archives · the file's encoding, BOM and line endings are kept; files that would not save back byte for byte are not editable · the ECUC values view of ARXML files can be edited and saved as well (written back into the ARXML) · unsaved changes are kept through whitespace and syntax changes, and closing asks first |
| **Syntax highlighting** | ~190 languages via the syntect grammars bundled with `bat`, plus A2L, DBC, Intel HEX, S-record, Conan files, Xpand, Xtend and Check (oAW) and FreeMarker templates (over HTML for `.ftl`/`.ftlh`, over XML for `.ftlx`, also inside attributes); ARXML, ODX, CDD, MWE workflows and other XML dialects are recognised as XML. Detected by file name, extension or shebang line, with a manual override |
| **CLI** | `parallax` for scripts and CI: tree or unified-diff output, JSON, `diff`-style exit codes |

### Content views

| Files | Default view | What it shows |
|---|---|---|
| `.docx` `.xlsx` `.pptx`, `.odt` `.ods` `.odp`, `.pdf` | Document text | Paragraphs (with headers, footers, footnotes, comments); one line per spreadsheet row, cells tab-separated, formulas as `{=…}`; slide text and notes; PDF text per page |
| `.doc` `.xls` `.ppt` | Document text | Converted with LibreOffice when it is installed (about 1 s per file) |
| Java `.class` files (also inside `.jar`, `.war`, `.ear`) | Class outline | Read directly, no JDK needed; about 0.4 ms per class. **Class outline**: version (with the Java release), declaration with generics, annotations, sealed/record/enum, nested classes, fields with constant values, methods with parameter names and `throws`, each with a checksum and count of its instructions, so a changed method body shows in the outline · **Bytecode**: every method's instructions with references resolved (`invokevirtual java.util.List.size(): int` instead of `#23`), jump targets as labels (`L1`) instead of byte offsets, locals by name, lambdas and string concatenation in short form · **Bytecode with line numbers**. Members are sorted, javac's class-wide lambda numbers are renumbered per method, and line numbers are left out of the plain bytecode view, so recompiling or editing one method does not change the lines of others. Classes whose code is equal apart from debug info (line numbers, local names, source file) are ≈ similar in folder compares |
| WebAssembly `.wasm` modules | Module outline | Read directly with `wasmparser`. **Module outline**: imports, exports, memories, tables, globals, data and element segments, custom sections with sizes, producers, and every function's signature with a checksum and count of its instructions · **WebAssembly text**: every function's instructions with functions, globals and locals by name (name section, else export and import names; Rust names demangled), blocks indented. Functions are sorted by name and checksums hash names instead of indexes, so adding a function does not change other lines. Modules whose sections are equal apart from custom sections (names, producers, DWARF) are ≈ similar |
| .NET assemblies (`.dll`, `.exe` with a CLI header) | Assembly outline | ECMA-335 metadata read directly, no .NET needed. **Assembly outline**: name, version, culture, public key token, references, assembly attributes, every type (C# style declaration with generics, constraints, base and interfaces) with fields, constants, properties, events and methods, each method with a checksum and count of its IL instructions, and the build identity (MVID, PDB path) · **IL**: every method's IL with tokens resolved to names, branch targets as labels, short and long forms folded · **Executable summary** as for native files. Rebuilds that differ only in MVID, timestamp, PDB path or layout are ≈ similar |
| Android binary XML (`AndroidManifest.xml`, `res/**/*.xml` inside `.apk`) | Android XML | Decoded to XML text, one attribute per line, typed values as aapt writes them (`16.0dp`, `#ff0080ff`, `true`), resource references as `@0x7f…` |
| ELF, PE, Mach-O | Executable summary | Header, build id, sections with address, size and CRC32, symbols sorted by name, imports |
| `.dbc` CAN databases | CAN database | Messages sorted by id (hex, standard/extended, DLC, sender, cycle time) with their signals sorted by multiplexer value and start bit (layout, byte order, sign, factor/offset, range, unit, receivers, value descriptions, comments, attributes with enum values resolved), nodes, value tables, attribute definitions, environment variables; unknown statements kept. Same database in another order or number format is ≈ similar |
| `.a2l` (ASAM MCD-2 MC) | A2L objects | Streaming parser, about 14 ms per MB. Every object as a block, sorted by kind and name: positional parameters labelled (address normalized to `0x0001A2B4`, so a rebuild shows as changed `Address:` lines), keywords, `AXIS_DESCR`, reference lists one per line, `IF_DATA` summarized; comments dropped. Same objects in another order or formatting are ≈ similar |
| `.map` linker maps (GNU ld, LLVM lld) | Memory map | Memory regions with use, output sections by address, size by object (total and per section), symbols sorted by name with address, size and object. Other linkers' maps and JSON source maps open as text |
| ELF with DWARF debug info | Debug symbols | After the executable summary: global and static variables with resolved types, size and address, and functions with address range; units read in parallel (112 MB of `.debug_info` in about 1 s) |
| `.mf4` `.mdf` measurements (MDF 4 and 3) | Measurement summary | Only the block tree is read, never the samples: header and metadata, file history, channel groups (cycles, record size, source) with channels (unit, data type, bit position, conversion with tables, source), attachments |
| `.cdfx` calibration data (ASAM CDF 2) | Calibration values | Every instance sorted by name: category, unit, values one per line (`[1][2] = 3.5`), axes, history; revision in the header. Same values in another order or formatting are ≈ similar |
| `.hex` `.s19` `.srec` `.mot` … | Firmware address map | Records decoded into a 16-byte aligned memory map, so different record lengths or order still line up; checksum errors are reported |
| `.png` `.jpg` `.gif` `.bmp` `.webp` `.tif` `.ico` | Image | Side by side, difference overlay or blink; count and bounding box of differing pixels |
| `.png` `.jpg` `.webp` `.gif` `.tif`, `.heic` `.avif` | Image metadata (second view; first for HEIF/AVIF) | Read from the file's metadata blocks without decoding pixels: format, dimensions, colour type and bit depth, density, animation; **EXIF** by tag name in groups (image, photo, GPS with the position in decimal degrees, thumbnail) with values as cameras show them (`1/250 s`, `f/2.8`); **XMP** as a sorted list of properties (`dc:title[x-default]`, `dc:subject[2]`), not the XML; **IPTC**; the **ICC profile** (description, class, colour space, version); PNG text chunks and JPEG/GIF comments. Maker notes and other blobs as size and checksum, strip and thumbnail offsets left out |
| `.mp4` `.mov` `.m4a` `.3gp`, `.mkv` `.webm`, `.mp3`, `.flac`, `.ogg` `.opus`, `.wav`, `.aiff`, `.avi` | Media summary | Read from the container headers only (MP4 boxes and Matroska elements are skipped by size, MP3 frames are not walked), so a multi-gigabyte video opens in milliseconds: container and brand, duration, overall bitrate; per track the codec with profile and level, resolution, frame rate, sample rate, channels, language, title, bitrate; tags sorted by name (iTunes `ilst`, Matroska tags per track, ID3v2/ID3v1/APE, Vorbis comments, RIFF INFO, `bext`); chapters; attachments and cover pictures with size and checksum. MP3, FLAC, WAV and AIFF files whose audio is equal and only tags differ are ≈ similar; FLAC files also when the MD5 of the decoded samples is equal |
| `.svg` | Text (XML) | Image view renders it |
| XML/JSON with very long lines | Pretty-printed | Minified files re-indented, one element per line |
| `.gguf` models (llama.cpp, Ollama, LM Studio) | Model summary | Read from the file header only, so a 70 GB model opens as fast as a small one. Views: **Model summary** (architecture, parameters, context length, heads and KV heads, RoPE, tokenizer, file type, bits per weight) · **Chat template** (Jinja source; templates stored as one line are broken at tags) · **Chat template, rendered** (the template applied to sample conversations, including a tool call, so the prompts the model sees are compared rather than the template text) · **Metadata** (every key, sorted) · **Tokenizer** (special tokens, then every token by id) · **Tensors and quantization** (recipe per type, a layer map of types per weight matrix, every tensor with shape, type and size) |
| SQLite databases (`.db` `.sqlite` `.sqlite3` `.db3`, or any name: detected by content) | Database schema | Read directly from the file format, no SQLite library; only the pages a view needs are touched. **Database schema**: header settings, then per table one line per column and table constraint, its indexes and triggers, then the views, sorted by name, SQL with whitespace and comments normalized · **Database rows**: per table the columns and row count, then the rows in rowid or primary-key order, one tuple per line (text over 200 characters and blobs over 32 bytes shortened with a SHA-256), up to 1 000 rows per table and 10 000 in all. Columns added with `ALTER TABLE` show their default in old rows, so an edit shows as the changed rows only. Databases with the same schema and rows are ≈ similar in folder compares (e.g. after `VACUUM`, or another page size or text encoding) |
| Parquet, Arrow IPC / Feather v2 (`.arrow` `.feather`) | Data file summary | Decoded from the footer only (Thrift resp. FlatBuffers, read by hand), so a gigabyte file opens in milliseconds. Parquet: schema with logical types and nesting, rows, key/value metadata, per column codec, sizes and null counts, per row group every column chunk with encodings and min/max statistics decoded by type (dates, timestamps, decimals, strings). Arrow: schema, dictionary encoding, metadata, null counts per field and the record batches. There is no rows view: decoding the compressed data pages would take most of a Parquet implementation |
| `.safetensors` models | Model summary | Read from the JSON header only. **Model summary** (parameters, tensors, layers, bits per weight, data types with their share, `__metadata__`) · **Tensors** (data types, a layer map of dtypes per weight matrix for `….layers.N.…` names, every tensor in natural order with shape, dtype, size and, for tensor data up to 256 MiB, a CRC32). Files with the same tensors and only other `__metadata__` are ≈ similar (checked up to 4 GiB of tensor data) |
| `.onnx` models | Model graph | Protobuf decoded by hand. IR version, producer, opsets, metadata props, inputs and outputs with element types and (symbolic) shapes, initializers sorted by name with dtype, shape, size and CRC32 (tiny ones with their values), every node in graph order as `Op(inputs) -> outputs {attributes}  // name`, with If/Loop bodies indented below their node, functions and value infos |
| Jupyter `.ipynb` | Notebook | **Notebook**: each cell's type and source, outputs in short form (stream text, `text/plain` results, errors as one line, images and HTML as `[image/png 12 KB, crc …]`); execution counts, ids and metadata left out · **Notebook, code only**. Notebooks with the same cells are ≈ similar |
| Lockfiles: `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock` (v1, Berry), `pnpm-lock.yaml`, `Cargo.lock`, `poetry.lock`, `uv.lock`, `Pipfile.lock`, `composer.lock`, `Gemfile.lock`, `go.sum`, Conan `conan.lock` (Conan 2 and 1) | Dependencies | One line per package and version, sorted and deduplicated, with the source only when it is not the default registry; Conan packages with their recipe revision (and package id for Conan 1), tool, Python and config requirements marked, timestamps dropped; ≈ similar when the packages and versions are the same |
| reStructuredText `.rst` `.rest` | Text (reST highlighting) | **Text, reflowed**: each paragraph and list item on one line, heading underlines as long as the title, literal blocks, code, tables and directive options kept as they are; files that differ only in wrapping are ≈ similar · **Needs** (when the file has [Sphinx-Needs](https://sphinx-needs.readthedocs.io)): every need sorted by id, options in a fixed order, link and tag lists sorted, text reflowed, nested needs listed with their parent, `needextend` at the end; needs of the default types and any directive with an `:id:` count. Files with the same needs in another order are ≈ similar |
| Sphinx-Needs `needs.json` | Needs | The same per-need form as for reST, per version, without empty defaults, line numbers, timestamps and the tool version; two builds of the same sources are ≈ similar |
| FreeMarker `.ftl` `.ftlh` `.ftlx` | Text (FreeMarker over HTML or XML) | **Template outline**: the directives (`<#if>`, `<#list>`, `<#macro>` …), user-defined directive calls (`<@ui.card>`) and `${…}` interpolations without the markup, indented by nesting, `<#else>` level with its directive; both tag syntaxes, `<#…>` and `[#…]`. Templates that differ only in `<#-- … -->` comments are ≈ similar. Mozilla Fluent files, which share `.ftl`, stay plain text |
| AUTOSAR EcucValues `.arxml` (files with ECUC module configuration values) | ECUC values | **ECUC values**: simplified JSON. Each module is an object with its `@def` and `@variant`. Inside it, parameters and references are keyed by the last segment of their definition, and sub-containers are grouped by definition and keyed by short name (`@index` where the file has one). Keys are sorted; numbers are decimal (`0x1F` → `31`, octal and binary too) and booleans are `true`/`false` (`1`/`0` too); references are written `"→ Mcu/McuModuleConfiguration/Clk"` without the package of the file's modules, and instance references as `"→ context ▸ target"`. UUIDs and admin data are left out. Files that differ only in order, UUIDs, admin data, number spelling or package name are ≈ similar. **Editable**: copy differences between the sides or edit by hand, then save. The ARXML is patched in place: only the values, containers and modules that changed are rewritten, everything else keeps its bytes, and numbers keep their style (hex stays hex, `1`/`0` booleans stay `1`/`0`). A new parameter or container is written like the same definition in the file or in the file on the other side, with its full definition path and `DEST`. Commas are optional, so copied lines never break the syntax. Before writing, the result is read back and must give exactly the edited text; a syntax error or an unknown parameter is reported with its line and nothing is saved. Checked on 23 real EcucValues files and 300 pairs generated from the AUTOSAR module definitions: 2 196 copies and edits written, all valid against the AUTOSAR 4.4 schema and, for whole files, equal to the source in every definition path, `DEST` and value by an independent reader. A 28 MB file saves in 2.5 s |
| Xpand templates `.xpt` (openArchitectureWare, Eclipse M2T) | Text (Xpand highlighting) | **Template outline**: `«IMPORT»` and `«EXTENSION»` sorted, then every `«DEFINE»` and `«AROUND»` sorted by name with a checksum of its body and the `«FILE»` and `«EXPAND»` statements it contains. Templates that differ only in `«REM»` comments, `//` and `/* */` comments inside statements, spacing inside `«…»`, the quotes of strings or text outside definitions (which Xpand never writes) are ≈ similar; spacing in the output text and `-»` still count. ISO-8859-1 templates keep their guillemets. Checked against the Xpand 2.2 parser on 12 600 mutants of the templates, extensions and checks shipped with Xpand 2.2, the Xtext 2.8 generator and GMF: no wrong ≈, none missed |
| Xtend extensions `.ext`, Check constraints `.chk` | Text (Xtend/Check highlighting) | **Code outline**: imports sorted; extensions with their signature and a checksum of the body (`JAVA` extensions in full); checks as `context Type if … ERROR "…"` with a checksum of the constraint, sorted. Comments and layout do not count. Workflows `.mwe` and `.oaw` are XML (Pretty-printed XML view). SAS `.xpt` transport files and CHKDSK `.chk` fragments stay binary |
| Rust `.rs` | Text | **Code outline**: per module the `use` declarations, constants, structs and enums with their fields and variants, traits and `impl` blocks with their members, functions with signature and a checksum of the body, and macros, sorted by kind and name; attributes shown, doc comments not. Compared by tokens without comments (doc comments too), so what rustfmt changes is ≈ similar: layout, trailing commas (not that of a 1-tuple), the order of `use`/`mod`/`extern crate` items and of names in `use` braces, commas between match arms and the braces around a single-expression arm or closure body, `;` after `return`/`break`/`continue` at the end of a block and after block-like statements, `>>` versus `> >`, `extern` versus `extern "C"`, nested parentheses; inside `macro_rules!` every token counts. Checked on 2 500 files from 699 crates: all 752 that rustfmt changes are similar (2 differ only inside `macro_rules!`), and 31 000 mutants judged by `syn`'s syntax tree gave no wrong ≈ outside trailing commas and doc comments in macro arguments, which count as formatting by design |
| JavaScript `.js` `.mjs` `.cjs` `.jsx`, TypeScript `.ts` `.mts` `.cts` `.tsx` `.d.ts`, `node` scripts | Text | **Code outline**: imports (sorted), interfaces, types and enums with their members, classes with constructor, fields and methods, functions and function-valued variables with signature and a checksum of the body, other variables; other code in one checksum. Compared by tokens with semicolons inserted where JavaScript inserts them itself, strings and numbers by value and JSX text as React reads it, so what Prettier changes is ≈ similar: layout, semicolons, quotes, trailing commas (not array holes), parentheses that precedence makes redundant (`(a && b) \|\| c`) or that wrap a value, a JSX element or a single arrow parameter, `new Foo` versus `new Foo()`, quoted object keys, `,` versus `;` between interface and type members, a leading `\|` in unions, `{" "}` in JSX and `attr={"x"}`. Checked on 2 540 files: 98 % of what Prettier rewrites is similar, all 138 TSX files; 68 600 mutants judged by the TypeScript compiler's syntax tree gave no wrong ≈. Files over 16 MB are compared by bytes |
| Python `.py` `.pyi` `.pyw`, scripts with a `python` shebang | Text | **Code outline**: the module docstring, imports (sorted), module and class level assignments, classes with their members, and every function with its signature and a checksum of its body (`# code 1a2b3c4d, 57 tokens`), sorted by name, so moving a function changes nothing and editing one changes its line; other module code in one checksum. Built from tokens with formatting removed, so files that differ only in comments, blank lines, line breaks, indentation width, quote style, re-indented docstrings, trailing commas and the parentheses Black adds or removes are ≈ similar. Checked against Python's `tokenize` on 1 982 files (identical tokens), on 1 166 files and their Black output (all similar but one, where Black changes the meaning of `(x): int`), and against 19 000 mutants judged by Python's AST (no wrong ≈) |
| Perl `.pl` `.pm` `.t` `.psgi`, scripts with a `perl` shebang | Text | **Code outline**: per package the modules used (sorted), `our` variables and constants, subs and `BEGIN`/`END` blocks with prototype or signature and a checksum of the body, sorted by name; other code in one checksum, `__END__`/`__DATA__` as a checksum. The lexer knows Perl's quoting (`q qq qw qx m qr s tr y` with any delimiters, here-documents, `<FH>`), regex versus division and `%hash` versus modulo; files that differ only in comments, POD, layout, the quotes of strings that interpolate nothing, a trailing comma or the last semicolon of a block are ≈ similar. Checked against B::Deparse on 27 500 mutants of 2 095 Perl files (spaces, line breaks, comments and POD inserted at random places, including inside strings, regexes and here-documents): no wrong ≈ |
| Conan graph JSON (`conan graph info`, `install`, `create` with `--format=json`) | Conan graph | Every package by reference and context with package type, package id, package revision, binary status, settings, options and direct requirements (requires, tool requires, test requires) by full reference; resolved version ranges and overrides; no paths, timestamps, recipe metadata or node numbers. Graphs that differ only in those, in binary status (cache, download, build) or remotes are ≈ similar; in the folder compare for `graph.json`, `install.json`, `create.json`, `build.json`, `deps.json` and `conan*.json` |
| Conan `conanfile.txt`, profiles, `conaninfo.txt`, `conanbuildinfo.txt` | Text (Conan highlighting) | **Normalized**: comments, blank lines and spacing around `=` dropped, sections in alphabetical order with repeated ones merged, generators sorted; other entries keep their order, since later ones override or append; profiles with Jinja keep their section order. Same entries are ≈ similar. Profiles (no extension) are recognised by an `include()` or a Conan section as the first entry. `conanmanifest.txt` that differs only in its time line is ≈ similar; `conanfile.py` is highlighted as Python and `conandata.yml` gets the YAML views |
| `.yaml` `.yml`, `.toml` | Text | **Normalized YAML/TOML**: keys sorted recursively, one layout, comments dropped, anchors and merge keys expanded, duplicate keys reported; files with the same data are ≈ similar |
| `.csv` `.tsv` | Text | **Table, sorted by key**: delimiter sniffed, quoted fields, rows sorted by the first unique column, cells joined by ` │ ` without padding (first 200 000 rows); same rows in another order are ≈ similar |
| Certificates and keys: PEM, DER (`.cer` `.crt` `.der`), PKCS#12 (`.p12` `.pfx`) | Certificate | Subject, issuer, serial, validity, key type and size, signature algorithm, SANs, key usages, basic constraints, key ids, SHA-256 fingerprint; CSRs and CRLs; keys by type and size only. PKCS#12 contents encrypted with PBES2 are opened with the empty password. The same certificates as PEM and DER are ≈ similar |
| E-mails `.eml`, Outlook `.msg` | E-mail | From, To, Cc, Subject, Date, Message-ID, In-Reply-To; the body as text (HTML converted when there is no plain part); attachments with type, size and SHA-256. Messages differing only in transport headers or encoding are ≈ similar |
| Other binary files | Hex dump | Up to 16 MB |

![Word documents compared by their text](docs/screenshots/document-compare.png)

![Intel HEX files compared as an address map](docs/screenshots/firmware-compare.png)

![Images compared pixel by pixel](docs/screenshots/image-compare.png)

## Writing to archives

Copy and delete work inside zip, 7z and tar archives, including archives nested in other archives; Git revisions are read-only. Each operation rewrites the archive once: the new archive is written to a temporary file next to the old one and then renamed over it, so a failed write leaves the old archive intact.

- **zip:** unchanged entries are copied as stored, without recompressing. A change to a 60 MB zip with 23 000 files takes 0.2–0.3 s.
- **7z:** unchanged blocks are copied as stored, so adding files is quick (about 1 s for a 27 MB archive). Replacing or deleting a file in a *solid* block means decompressing that block and compressing the rest of it again, as 7-Zip does: about 22 s on 4 cores for a block holding 247 MB.
- **tar, .tar.gz, .tar.xz, .tar.bz2, .tar.zst:** the whole archive is compressed again.

New entries are compressed with deflate (zip) or LZMA2 at 7-Zip's default level (7z, .tar.xz). File permissions are not carried over: new tar entries get mode 644 (folders 755).

## Watching for changes

Local folders, archives and files are watched while they are compared. A change outside the app updates only the rows it affects: a changed file is compared again on its own, a new file is inserted into its folder, and a burst of changes (a build, a checkout) is collected for a moment and merged into the few folders it touched, or one full compare when it reaches the top. A file open in a text tab is reloaded, or, with unsaved edits, a banner offers to reload it. Excluded folders and the app's own writes do not count.

On Linux, where each folder needs its own watch, only the folders the compare shows are watched: folders hidden by `.gitignore` (`target/`, `node_modules/` …) and excluded folders are left out, and new folders are added as they appear. Watching the 3 500 folders of the test tree takes about 0.15 s, in the background; 5 000 files written in 0.15 s arrive as one update 0.35 s later.

## Performance

All of the work happens in a multi-threaded Rust core; the UI only renders the rows that are visible.

- **Git vs. Git:** tree ids are compared before anything is read. An unchanged folder costs a single id comparison, however large it is. Changed files are decided by their blob ids, without inflating the objects.
- **Archives:** zip and 7z entries are compared with the CRC32 stored in the archive headers, so nothing is decompressed for listing.
- **Git vs. local file:** the local file's Git blob id is computed and compared, instead of reading the Git object.
- **Local files:** parallel walking (rayon), files are memory-mapped, and size mismatches short-circuit.
- **Text views:** rows, intra-line marks and syntax colours are produced only for the visible window. Highlighter state is checkpointed every 128 lines, so jumping to line 900 000 does not re-parse the file from the top.

Measured with the CLI on two Linux kernel releases (v6.11 and v6.12, ~87 000 files, 1.6 GB per tree). The machine is a 4-core cloud VM, the OS file cache was warm, and each time is the full wall-clock run including output:

| Comparison | 1 thread | 4 threads |
|---|---|---|
| Git tag vs. Git tag (read from the object database) | 1.9 s | 0.51 s |
| Folder vs. folder, by content | 2.0 s | 0.59 s |
| Folder vs. folder, nearly identical (all 1.6 GB read) | 1.8 s | 0.58 s |
| Folder vs. folder, quick (size and date) | 0.62 s | 0.23 s |
| Worktree vs. `HEAD` (one file edited) | 2.2 s | 0.69 s |
| Text diff, 1 million lines (55 MB), 2 500 changed lines | 0.41 s | — |

Opening files with the content views (same machine): a 12 MB ARXML (242 000 lines) diffs in about 35 ms, and highlighting anywhere in it takes under 10 ms. A 5 MB Latin-1 A2L with 10 000 changed lines diffs in 0.1 s; with histogram instead of Myers it takes 0.9 s, which is why Myers is the default. A 300-page PDF extracts in 0.18 s, and a 100 000-row XLSX in 0.2 s.

In the desktop app (release build, same kind of VM), two 1-million-line files (63 MB each) are on screen about 2 seconds after start, and two 3-million-line files (200 MB each) after about 4. Jumping anywhere, with F7/F8, Home/End or the overview strip, shows the rows within 0.1 s. While the scrollbar is dragged, rows not loaded yet show as grey bars and fill in when it stops. Views taller than a browser can lay out (WebKit stops painting past about 16.7 million pixels, or 880 000 lines) scroll through a scaled scroll area, so the end of any file stays reachable.

Folder compares use every core: with 4 threads they run 3.5–3.8× faster, at about 370 % CPU. A text diff of a single file runs on one core; the desktop app only renders and highlights the visible rows.

## Usage

### Desktop app

Start `parallax-compare` and pick the two sides in the *New comparison* dialog. You can also pass both sides on the command line:

```sh
parallax-compare ~/src/shop@main ~/src/shop@feature/checkout --merge-base
parallax-compare dist/ release-1.4.7z
parallax-compare old.txt new.txt
```

As a Git difftool:

```sh
git config --global difftool.parallax.cmd 'parallax-compare "$LOCAL" "$REMOTE"'
git config --global diff.tool parallax
git difftool --dir-diff main feature/x     # or simply: parallax-compare .@main .@feature/x
```

To compare against something else without starting over, change a side from its path bar above the panes:

- click the path, type another one (any form from the table below, e.g. `repo@v1.4`) and press Enter; Esc cancels. While typing, recently compared paths and the entries of the folder typed so far are suggested: Tab completes (after `repo@`, branch and tag names), ↑/↓ choose
- 📂 opens the file or folder dialog (⎇ the branch list for a Git side)
- ▾ opens a menu: *Change side…* (folder, file, Git ref or worktree), *Git history of this file…* (pick the working tree or any commit that changed the file for this side), *Open in default app*, *Show in file manager*, *Copy path*
- drop a file or folder from your file manager onto the left or right half of the window; drop two to replace both sides, or onto a *New comparison* tab to fill it in

The tab keeps its options and the other side. If the new path cannot be opened, the old comparison stays. In a file opened from a folder comparison, changing one side compares that file with any other file, also one with a different name.

Each tab remembers what it compared before: ← and → in the toolbar (or the mouse's back and forward buttons) go back and forth, and ↑ goes one level up: from a file opened in a folder comparison back to that row, otherwise to both sides' parent folders. To compare two different rows of a folder comparison, such as a file renamed between the sides, Ctrl+click both and choose *Compare these*. *Swap* exchanges left and right, in folder and file comparisons.

Right-click a row (or press Shift+F10) for what you can do with it: open, compare two selected rows, copy or delete on either side, open in the default app, show in the file manager, copy the path. Settings that change less often (compare rule, line endings, excluded files; whitespace, case, diff algorithm, syntax) are under *Options* at the right of the toolbar; a dot there means they differ from the defaults.

**Command palette:** Ctrl+Shift+P, Ctrl+K or F1 lists every action of the app and the current tab, with its shortcut. Type a few letters (in order is enough, e.g. `expall` for *Expand all folders*) and press Enter.

The *New comparison* tab lists recent comparisons: click one to compare again with the same options, ☆ pins it to the top, ✎ puts it into the form to change it first. Errors and confirmations appear briefly in the lower right corner.

Keyboard: arrow keys, Enter (open or expand), ←/→ (collapse/expand), F5 (refresh), F7/F8 (previous/next difference), Ctrl+F (find), F3/Shift+F3 (next/previous match), Ctrl+Shift+P / Ctrl+K / F1 (command palette), Shift+F10 (context menu), Ctrl+T (new tab), Ctrl+W (close tab), Ctrl+Tab / Ctrl+Shift+Tab (next/previous tab).

### Source syntax (CLI and path fields)

| Text | Meaning |
|---|---|
| `path/to/folder`, `file.txt` | a local folder or file |
| `release.7z` | an archive, compared as a folder |
| `release.7z!/bin` | a folder inside an archive |
| `repo@main`, `repo@v1.4.0`, `repo@HEAD~3` | a Git revision of the repository at `repo` |
| `repo@main:src/lib` | a folder (or file) inside a Git revision |

A text that names an existing path is always read as a path, so folders with `@` in their name keep working.

### CLI

```console
$ parallax ~/src/shop@main ~/src/shop@feature/checkout
  ≠ assets/
  ≠ assets/fonts.zip/
  ≠ assets/fonts.zip/Inter-Bold.woff2
  → assets/fonts.zip/Inter-Italic.woff2
  ≠ src/
  ≠ src/checkout/
  ≠ src/checkout/cart.ts
  → src/checkout/discount.ts
  ← src/checkout/legacyPayment.ts
  ≠ src/checkout/types.ts
  ≠ package.json

9 files compared: 2 same, 4 different, 1 left only, 2 right only  (1 ms, 1 identical folders skipped via Git tree ids)
```

Files are compared through their content view, so `parallax old.docx new.docx` prints a diff of the document text, and two images report their differing pixels. `--raw` compares the plain bytes as text instead, and `--bytes` turns off the ≈ similar status in folder compares.

`parallax --help` lists all options (`--merge-base`, `--quick`, `--strict-eol`, `-x/--exclude`, `--json`, `--whitespace`, `--histogram`, `-U` …). Exit status: 0 identical or only similar, 1 different, 2 error.

## Building

Requirements: Rust 1.88+ and Node.js 20+. On Linux you also need the WebKitGTK development packages:

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev
```

```sh
cargo test                       # core and CLI tests
cargo build --release -p parallax-cli

cd app
npm ci
npm run tauri dev                # desktop app with hot reload
npm run tauri build              # installers: MSI/NSIS on Windows, deb/rpm/AppImage on Linux
```

CI runs formatting, clippy, the Svelte type check and all tests on Ubuntu and Windows. Pushing a `v*` tag builds installers and the CLI as a draft GitHub release.

## Architecture

```
app/                      Tauri 2 desktop app
  src/                    Svelte 5 UI: tabs, new-comparison dialog, folder view, text view
  src-tauri/              commands: sessions, compare, expand, copy/delete, file diff rows
crates/parallax-core/     UI-agnostic engine (also used by the CLI)
  vfs/                    every source as a tree: local, archive, git, archive mount layer
  compare.rs              parallel folder compare with tree-id / CRC / blob-id shortcuts
  textdiff.rs             imara-diff line diff → aligned rows, intra-line ranges
  convert/                content views: office/PDF text, executables, firmware, pretty XML/JSON, hex, images
  syntaxes/               extra grammars (A2L, DBC, Intel HEX, S-record, FreeMarker, Conan, Xpand/Xtend/Check, view output)
  highlight.rs            syntect highlighting with checkpoints
  source.rs               source syntax and opening, merge-base
crates/parallax-cli/      the `parallax` command
```

The key abstraction is `trait Vfs { list, stat, read }`. Each entry can carry a `QuickId` (Git object id or CRC32) that the compare engine uses before falling back to reading content.

## Status and roadmap

This is the first version of the MVP described in the concept. Not done yet:

- 3-way merge (as a `git mergetool`)
- byte-aligned hex compare (the hex dump view is line-based, so an inserted byte shifts the rest of the file)
- table compare for CSV/XLSX with key columns, so moved rows are recognised
- RAR archives, Git LFS pointer resolution, submodule contents
- saved sessions and settings, report export
- the planned tree-sitter highlighting and structural diff (syntect is used for now)
- remote sources (SFTP, S3)
