//! Command line front end.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use xdw_salvage::adapters::{html, pdf};
use xdw_salvage::application::verification::Expectation;
use xdw_salvage::domain::PageData;

use xdw_salvage::infrastructure::{self, LocalSalvageService};

const USAGE: &str = "\
xdw-salvage - recover what can be recovered from DocuWorks (.xdw) files

USAGE:
    xdw-salvage info    <FILE>...              show structure and page inventory
    xdw-salvage extract <FILE>... -o <DIR>     write out JPEG pages and originals
    xdw-salvage pdf     <FILE>... [-o <OUT>]   assemble recoverable pages
                                               (one file: -o names the PDF;
                                                several: -o names a directory)
    xdw-salvage html    <FILE>... [-o <OUT>]   same, as one self-contained page
    xdw-salvage codec   <FILE>... -o <DIR>     write out the streams this tool
                                               cannot decode, with a CSV index
    xdw-salvage triage  <PATH>... [-o <CSV>]   classify a whole archive
    xdw-salvage manifest <FILE>... [-o <CSV>]  what a faithful conversion must contain
    xdw-salvage verify  <FILE.xdw> --pages <N>  check a conversion for lost pages

OPTIONS:
    -o, --out <PATH>    output file or directory
        --skip-missing  leave unrecoverable pages out of the PDF entirely
                        (default: keep a placeholder so page numbers line up)
        --paper <SIZE>  put every page on one sheet: a4, letter, or WxH in mm
                        (default: each page keeps the size it declares, which
                        can vary a lot within one document)
        --previews      also emit the low-resolution preview entries
        --lang <ja|en>  language of the notes on unrecoverable pages
        --no-attach     do not carry embedded original files into the output
        --carry-source  attach the whole .xdw to the output, so the conversion
                        is a superset of the file it came from and nothing in
                        the vendor coding is left behind (roughly doubles size)
        --no-bookmarks  do not bookmark the pages that could not be recovered
        --pages <N>     page count of the converted file, for verify
    -h, --help          this text

Pages printed through the vendor's driver hold their image in a coding this
tool does not decode; they are reported, never guessed at. Protected documents
are refused.
";

/// Print a line, and give up quietly if the reader has gone away.
///
/// `xdw-salvage info *.xdw | head` closes the pipe partway through. The default
/// `println!` turns that into a panic with a backtrace, which is a poor way for
/// a tool that promises never to panic to end a perfectly ordinary command.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        if writeln!(lock, $($arg)*).is_err() {
            std::process::exit(0);
        }
    }};
}

fn service() -> LocalSalvageService {
    infrastructure::local_service()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let mut out: Option<PathBuf> = None;
    let mut skip_missing = false;
    let mut previews = false;
    let mut paper: Option<(f32, f32)> = None;
    let mut pages: Option<usize> = None;
    let mut lang = pdf::Lang::English;
    let mut attach_originals = true;
    let mut carry_source = false;
    let mut no_decode = false;
    let mut font_path: Option<String> = None;
    let mut bookmarks = true;
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => match it.next() {
                Some(v) => out = Some(PathBuf::from(v)),
                None => return fail("-o needs a path"),
            },
            "--skip-missing" => skip_missing = true,
            "--no-attach" => attach_originals = false,
            "--carry-source" => carry_source = true,
            "--no-decode" => no_decode = true,
            "--font" => match it.next() {
                Some(v) => font_path = Some(v),
                None => return fail("--font needs a path to a TrueType file"),
            },
            "--no-bookmarks" => bookmarks = false,
            "--lang" => match it.next().as_deref() {
                Some("ja") | Some("jp") => lang = pdf::Lang::Japanese,
                Some("en") => lang = pdf::Lang::English,
                _ => return fail("--lang wants ja or en"),
            },
            "--pages" => match it.next().as_deref().map(str::parse::<usize>) {
                Some(Ok(n)) => pages = Some(n),
                _ => return fail("--pages wants a whole number"),
            },
            "--paper" => match it.next().as_deref().map(parse_paper) {
                Some(Some(p)) => paper = Some(p),
                Some(None) => return fail("--paper wants a4, letter, or WxH in mm"),
                None => return fail("--paper needs a size"),
            },
            "--previews" => previews = true,
            _ => positional.push(a),
        }
    }
    if positional.is_empty() {
        return fail("no command given; try --help");
    }
    let command = positional.remove(0);
    if positional.is_empty() {
        return fail("no input given");
    }

    // A font is read once and shared by every page of every document.
    let font = match &font_path {
        None => None,
        Some(path) => match std::fs::read(path) {
            Err(e) => return fail(&format!("{path}: {e}")),
            Ok(bytes) => match xdw_salvage::infrastructure::ttf::Font::parse(bytes, path) {
                Some(f) => Some(std::sync::Arc::new(f)),
                None => {
                    return fail(&format!(
                        "{path}: not a TrueType font this tool can embed \
                         (a .ttf with glyph outlines; .ttc collections and CFF/.otf are not read)"
                    ))
                }
            },
        },
    };

    let opts = pdf::Options {
        missing: if skip_missing {
            pdf::Missing::Skip
        } else {
            pdf::Missing::Placeholder
        },
        decode: !no_decode,
        font,
        include_previews: previews,
        paper,
        embed_originals: attach_originals,
        lang,
        title: None,
        bookmark_gaps: bookmarks,
        carry_source,
    };

    match command.as_str() {
        "info" => run_info(&positional),
        "extract" => match out {
            Some(dir) => run_extract(&positional, &dir),
            None => fail("extract needs -o <DIR>"),
        },
        "pdf" => run_pdf(&positional, out.as_deref(), opts),
        "html" => run_html(
            &positional,
            out.as_deref(),
            html::Options {
                decode: !no_decode,
                lang,
                title: None,
                include_previews: previews,
                embed_originals: attach_originals,
                skip_missing,
                carry_source,
            },
        ),
        "codec" => match out {
            Some(dir) => run_codec(&positional, &dir),
            None => fail("codec needs -o <DIR>"),
        },
        "triage" => run_triage(&positional, out.as_deref()),
        "manifest" => run_manifest(&positional, out.as_deref()),
        "verify" => match pages {
            Some(n) => run_verify(&positional[0], n),
            None => fail("verify needs --pages <N>, the converted file's page count"),
        },
        other => fail(&format!("unknown command {other:?}; try --help")),
    }
}

/// `a4`, `letter`, or a millimetre pair such as `210x297`.
fn parse_paper(s: &str) -> Option<(f32, f32)> {
    match s.to_ascii_lowercase().as_str() {
        "a4" => return Some(pdf::A4),
        "letter" => return Some(pdf::LETTER),
        _ => {}
    }
    let (w, h) = s.split_once(['x', 'X'])?;
    let w: f32 = w.trim().parse().ok()?;
    let h: f32 = h.trim().parse().ok()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((w * 72.0 / 25.4, h * 72.0 / 25.4))
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("xdw-salvage: {msg}");
    ExitCode::FAILURE
}

fn run_info(files: &[String]) -> ExitCode {
    let service = service();
    let mut code = ExitCode::SUCCESS;
    for f in files {
        let asset = match service.open(Path::new(f)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{f}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let analysis = service.analyze(&asset);
        let data = &asset.data;
        let doc = &asset.document;
        let cov = analysis.coverage;
        outln!(
            "{}  {} bytes  generation {}  trailer 0x{:02x}",
            name(f),
            data.len(),
            doc.generation,
            doc.trailer_tag
        );
        outln!(
            "  {} sheet(s)  [{} entries = {} sheet + {} picture + {} thumbnail{}]   verdict {}",
            cov.sheets,
            doc.pages.len(),
            cov.sheets,
            cov.pictures,
            cov.thumbnails,
            if cov.data_tables > 0 {
                format!(" + {} data table", cov.data_tables)
            } else {
                String::new()
            },
            analysis.verdict.as_str()
        );
        outln!(
            "  sheets recovered {}/{}   pictures off those sheets {}/{} on {} sheet(s)",
            cov.sheets_recovered,
            cov.sheets,
            cov.pictures_recovered,
            cov.pictures,
            cov.sheets_with_pictures
        );
        if cov.sheets_blank > 0 {
            outln!(
                "  {} sheet(s) come out blank: neither the sheet nor any artwork on it recovers",
                cov.sheets_blank
            );
        }
        if let Some(why) = doc.rebuilt {
            outln!(
                "  ! {}; the pages below were found by scanning the file, and in a\n    document that has been saved over that includes earlier states",
                why.as_str()
            );
        }
        if doc.generations_present > 1 {
            outln!(
                "  saved over: {} document states present; earlier pages are still in the file",
                doc.generations_present
            );
        }
        // Tables of names that sit in the page list in the clear. Worth
        // printing: they are the only part of a printer-driven page this tool
        // can read at all, and they name what the page refers to.
        for p in doc.pages.iter() {
            if let PageData::Fields {
                offset,
                len,
                records,
            } = p.data
            {
                let names = xdw_salvage::infrastructure::xdw_page::field_names(data, offset, len);
                outln!(
                    "  data table at entry {}: {records} field(s) in the clear, {} name(s)",
                    p.index,
                    names.len()
                );
                if !names.is_empty() {
                    let shown: Vec<&str> = names.iter().take(16).map(|s| s.as_str()).collect();
                    outln!(
                        "    {}{}",
                        shown.join(" "),
                        if names.len() > shown.len() {
                            format!(" ... (+{})", names.len() - shown.len())
                        } else {
                            String::new()
                        }
                    );
                }
            }
        }
        if let Some((stored, expanded)) = doc.properties_len {
            outln!("  properties block: {stored} B stored, {expanded} B expanded (vendor coding)");
        }
        for p in doc.pages.iter().take(8) {
            outln!("    [{:3}] {}", p.index, p.describe());
        }
        if doc.pages.len() > 8 {
            outln!("    ...  {} more", doc.pages.len() - 8);
        }
        let unknown: Vec<String> = doc
            .pages
            .iter()
            .flat_map(|p| p.unknown_fields.iter())
            .map(|t| format!("0x{t:02x}"))
            .collect();
        if !unknown.is_empty() {
            let mut u = unknown;
            u.sort();
            u.dedup();
            outln!("  unrecognised page fields: {}", u.join(" "));
        }
        for (tag, at, len) in &doc.unknown_tags {
            outln!("  unrecognised document element 0x{tag:02x} at {at}, {len} B");
        }
        for a in &analysis.attachments {
            outln!(
                "  embedded {} at {}, {} B{}",
                a.kind.label(),
                a.offset,
                a.len,
                if a.length_is_estimate {
                    " (length estimated)"
                } else {
                    ""
                }
            );
        }
        outln!();
    }
    code
}

fn run_extract(files: &[String], dir: &Path) -> ExitCode {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(&format!("cannot create {}: {e}", dir.display()));
    }
    let service = service();
    let mut code = ExitCode::SUCCESS;
    let mut namer = Namer::default();
    for f in files {
        let asset = match service.open(Path::new(f)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{f}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let data = &asset.data;
        let doc = &asset.document;
        let stem = namer.stem(f);
        let mut written = 0usize;
        for p in doc.recoverable_pages() {
            if let PageData::Jpeg { offset, len } = p.data {
                let path = dir.join(format!("{stem}_p{:03}.jpg", p.index + 1));
                if let Err(e) = std::fs::write(&path, &data[offset..offset + len]) {
                    eprintln!("{}: {e}", path.display());
                    code = ExitCode::FAILURE;
                } else {
                    written += 1;
                }
            }
        }
        for (i, a) in service.attachments(data).iter().enumerate() {
            let path = dir.join(format!("{stem}_original{i}.{}", a.kind.extension()));
            if let Err(e) = std::fs::write(&path, a.bytes(data)) {
                eprintln!("{}: {e}", path.display());
                code = ExitCode::FAILURE;
            } else {
                written += 1;
            }
        }
        let cov = doc.coverage();
        let stuck = cov.printer_derived();
        outln!(
            "{}: wrote {written} file(s){}",
            name(f),
            if stuck > 0 {
                format!("; {stuck} sheet(s) left behind (vendor coding)")
            } else {
                String::new()
            }
        );
    }
    code
}

fn run_pdf(files: &[String], out: Option<&Path>, opts: pdf::Options) -> ExitCode {
    // One input: -o names the file. Several: -o names a directory, because a
    // migration converts thousands of documents in one pass.
    let many = files.len() > 1;
    if many {
        if let Some(dir) = out {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return fail(&format!("cannot create {}: {e}", dir.display()));
            }
        }
    }

    let service = service();
    let mut code = ExitCode::SUCCESS;
    let mut namer = Namer::default();
    let mut totals = (0usize, 0usize, 0usize, 0usize, 0usize);
    for file in files {
        let asset = match service.open(Path::new(file)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{file}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let data = &asset.data;
        let doc = &asset.document;
        let mut o = opts.clone();
        o.title.get_or_insert_with(|| name(file));
        let (bytes, report) = pdf::build_with(
            data,
            doc,
            o,
            service.decoder(),
            service.attachment_scanner(),
        );

        let target = match (out, many) {
            (Some(dir), true) => dir.join(format!("{}.pdf", namer.stem(file))),
            (Some(path), false) => path.to_path_buf(),
            (None, _) => Path::new(file).with_extension("pdf"),
        };
        if let Err(e) = std::fs::write(&target, &bytes) {
            eprintln!("{}: {e}", target.display());
            code = ExitCode::FAILURE;
            continue;
        }
        totals.0 += report.embedded + report.drawn;
        totals.1 += report.placeholders;
        totals.2 += report.pictures_placed;
        totals.3 += 1;
        totals.4 += report.glyphs;
        outln!(
            "{} -> {}  ({} page(s) reproduced, {} not, {} character(s) of text, \
             {} picture(s) salvaged, {} attachment(s), {} bookmark(s))",
            name(file),
            target.display(),
            report.embedded + report.drawn,
            report.placeholders,
            report.glyphs,
            report.pictures_placed,
            report.attachments,
            report.bookmarks
        );
    }
    if many {
        outln!(
            "{} file(s): {} page(s) reproduced, {} not, {} character(s) of text, \
             {} picture(s) salvaged",
            totals.3,
            totals.0,
            totals.1,
            totals.4,
            totals.2
        );
    }
    code
}

fn run_html(files: &[String], out: Option<&Path>, opts: html::Options) -> ExitCode {
    let many = files.len() > 1;
    if many {
        if let Some(dir) = out {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return fail(&format!("cannot create {}: {e}", dir.display()));
            }
        }
    }

    let service = service();
    let mut code = ExitCode::SUCCESS;
    let mut namer = Namer::default();
    let (mut pages, mut gaps, mut art, mut done) = (0usize, 0usize, 0usize, 0usize);
    for file in files {
        let asset = match service.open(Path::new(file)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{file}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let data = &asset.data;
        let doc = &asset.document;
        let mut o = opts.clone();
        o.title.get_or_insert_with(|| name(file));
        let (page, report) = html::build_with(
            data,
            doc,
            &o,
            service.decoder(),
            service.attachment_scanner(),
        );

        let target = match (out, many) {
            (Some(dir), true) => dir.join(format!("{}.html", namer.stem(file))),
            (Some(path), false) => path.to_path_buf(),
            (None, _) => Path::new(file).with_extension("html"),
        };
        if let Err(e) = std::fs::write(&target, page.as_bytes()) {
            eprintln!("{}: {e}", target.display());
            code = ExitCode::FAILURE;
            continue;
        }
        pages += report.embedded;
        gaps += report.gaps;
        art += report.pictures;
        done += 1;
        outln!(
            "{} -> {}  ({} page(s) reproduced, {} not, {} picture(s) salvaged off them, {} file(s) carried)",
            name(file),
            target.display(),
            report.embedded,
            report.gaps,
            report.pictures,
            report.attachments
        );
    }
    if many {
        outln!("{done} file(s): {pages} page(s) reproduced, {gaps} not, {art} picture(s) salvaged");
    }
    code
}

/// One stream that this tool cannot decode, ready to be written out.
struct Stream {
    role: &'static str,
    idx: usize,
    offset: usize,
    len: usize,
    /// The expanded length the container itself declares, where it declares
    /// one. This is the answer key for any decoder: a result of a different
    /// length is wrong.
    expanded: Option<u64>,
    kind: String,
    method: String,
    colour: String,
}

impl Stream {
    /// The fields a stream carrying no coding metadata leaves empty.
    fn plain() -> Stream {
        Stream {
            role: "",
            idx: 0,
            offset: 0,
            len: 0,
            expanded: None,
            kind: String::new(),
            method: String::new(),
            colour: String::new(),
        }
    }
}

/// Write out every stream this tool cannot decode, with an index.
///
/// The point is not that these are useful as they stand. It is that an archive
/// owner can see exactly how much of their material sits behind the vendor
/// coding, hand the streams to whatever can read them, and check the result
/// against the expanded length the container itself declares.
fn run_codec(files: &[String], dir: &Path) -> ExitCode {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(&format!("cannot create {}: {e}", dir.display()));
    }
    let mut csv = String::from(
        "path,entry,role,offset,stored,coder_header,expanded_declared,ratio,kind,method,colour\n",
    );
    let mut code = ExitCode::SUCCESS;
    let mut namer = Namer::default();
    let service = service();
    let (mut n, mut stored_total) = (0usize, 0u64);
    let mut leanest: Option<(f64, String, usize, u64)> = None;
    for file in files {
        let asset = match service.open(Path::new(file)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{file}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let data = &asset.data;
        let doc = &asset.document;
        let stem = namer.stem(file);
        let mut streams: Vec<Stream> = Vec::new();
        for p in &doc.pages {
            match &p.data {
                PageData::Encoded {
                    offset,
                    len,
                    kind_code,
                    aux_len,
                    method,
                    colour,
                } => streams.push(Stream {
                    role: "page",
                    idx: p.index,
                    offset: *offset,
                    len: *len,
                    expanded: *aux_len,
                    kind: kind_code.to_string(),
                    method: method.map(|v| v.to_string()).unwrap_or_default(),
                    colour: colour.map(|v| v.to_string()).unwrap_or_default(),
                }),
                PageData::Preview {
                    offset,
                    len,
                    pixels_at,
                    stored,
                    expanded,
                    ..
                } => streams.push(Stream {
                    role: "preview",
                    idx: p.index,
                    // The bitmap header and palette sit in the clear; only what
                    // follows the compression sub-header is coded.
                    offset: offset + pixels_at + 16,
                    len: (*stored as usize).min(len.saturating_sub(pixels_at + 16)),
                    expanded: Some(*expanded as u64),
                    kind: "preview".into(),
                    ..Stream::plain()
                }),
                PageData::Bare { offset, len } => streams.push(Stream {
                    role: "bare",
                    idx: p.index,
                    offset: *offset,
                    len: *len,
                    kind: "bare".into(),
                    ..Stream::plain()
                }),
                // Neither of these is undecodable: a JPEG is a JPEG, and a
                // field table is already in the clear.
                PageData::Jpeg { .. } | PageData::Fields { .. } => {}
            }
        }
        if let Some((offset, len)) = doc.properties {
            streams.push(Stream {
                role: "properties",
                idx: doc.pages.len(),
                offset,
                len,
                expanded: doc.properties_len.map(|(_, e)| e as u64),
                kind: "properties".into(),
                ..Stream::plain()
            });
        }

        for s in streams {
            if s.len == 0 || s.offset + s.len > data.len() {
                continue;
            }
            let out = dir.join(format!("{stem}.{:04}.{}.bin", s.idx, s.role));
            if let Err(e) = std::fs::write(&out, &data[s.offset..s.offset + s.len]) {
                eprintln!("{}: {e}", out.display());
                code = ExitCode::FAILURE;
                continue;
            }
            // The first 16 bits of every coded stream are a header whose value
            // tracks the stream's length; the code itself starts at bit 16.
            // Anyone attacking the coding needs it, both to line streams up by
            // length and to know where the code actually begins.
            let header = data
                .get(s.offset..s.offset + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]).to_string())
                .unwrap_or_default();
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_field(file),
                s.idx,
                s.role,
                s.offset,
                s.len,
                header,
                s.expanded.map(|e| e.to_string()).unwrap_or_default(),
                s.expanded
                    .map(|e| format!("{:.2}", e as f64 / s.len as f64))
                    .unwrap_or_default(),
                s.kind,
                s.method,
                s.colour,
            ));
            if let Some(e) = s.expanded {
                let ratio = e as f64 / s.len as f64;
                if leanest.as_ref().map_or(true, |(best, ..)| ratio > *best) {
                    leanest = Some((
                        ratio,
                        out.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        s.len,
                        e,
                    ));
                }
            }
            n += 1;
            stored_total += s.len as u64;
        }
    }
    // The stream that expands the most from the least is the one whose content
    // is closest to featureless, and that is the one worth attacking: a
    // thumbnail of a blank page expands to a known constant, which is the only
    // exact plaintext anyone gets without the vendor's software.
    if let Some((ratio, name, stored, expanded)) = leanest {
        outln!(
            "most compressible stream: {name}  {stored} -> {expanded} B ({ratio:.0}x)\n             \x20 a blank page's thumbnail would expand to one repeated value; \
             the closer to blank, the better a crib it makes\n              the first three code bits track the page's first pixel, so a stream \
             whose opening pixel you know is worth more than a short one"
        );
    }

    let index = dir.join("index.csv");
    if let Err(e) = std::fs::write(&index, csv.as_bytes()) {
        eprintln!("{}: {e}", index.display());
        code = ExitCode::FAILURE;
    }
    outln!(
        "{n} stream(s), {stored_total} B stored, index at {}",
        index.display()
    );
    code
}

/// Hands out one output name per input file, and never the same name twice.
///
/// An archive being migrated has "Note.xdw" in nine different folders. Writing
/// them all into one output directory by file stem quietly loses eight of them,
/// which is exactly the kind of silent loss this tool exists to catch, so the
/// second and later claims on a stem get a suffix.
#[derive(Default)]
struct Namer {
    seen: std::collections::HashMap<String, usize>,
}

impl Namer {
    fn stem(&mut self, file: &str) -> String {
        let base = Path::new(file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".into());
        let n = self.seen.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            base
        } else {
            format!("{base}~{n}")
        }
    }
}

fn run_manifest(files: &[String], out: Option<&Path>) -> ExitCode {
    let mut csv = String::from("path,page,kind,width_mm,height_mm,recoverable\n");
    let service = service();
    let mut code = ExitCode::SUCCESS;
    for f in files {
        let asset = match service.open(Path::new(f)) {
            Ok(asset) => asset,
            Err(e) => {
                eprintln!("{f}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let doc = &asset.document;
        let exp = Expectation::of(doc);
        for (i, p) in doc.content_pages().enumerate() {
            let (w, h) = exp
                .size_mm(i)
                .map(|(w, h)| (format!("{w:.1}"), format!("{h:.1}")))
                .unwrap_or_default();
            csv.push_str(&format!(
                "{},{},{},{},{},{}\n",
                csv_field(f),
                i + 1,
                p.kind_name(),
                w,
                h,
                p.is_recoverable()
            ));
        }
        outln!(
            "{}: a faithful conversion has {} page(s)  ({} preview entr(y/ies) must not appear)",
            name(f),
            exp.pages,
            exp.previews
        );
    }
    if let Some(path) = out {
        if let Err(e) = std::fs::write(path, csv.as_bytes()) {
            return fail(&format!("{}: {e}", path.display()));
        }
        outln!("per-page detail -> {}", path.display());
    } else {
        print!("{csv}");
    }
    code
}

fn run_verify(xdw: &str, observed_pages: usize) -> ExitCode {
    let service = service();
    let asset = match service.open(Path::new(xdw)) {
        Ok(asset) => asset,
        Err(e) => return fail(&format!("{xdw}: {e}")),
    };
    let doc = &asset.document;
    let exp = Expectation::of(doc);
    let report = xdw_salvage::application::verification::compare(&exp, observed_pages, &[]);

    outln!(
        "{}: container {} page(s), converted file {} page(s)",
        name(xdw),
        report.expected_pages,
        report.observed_pages
    );
    for f in &report.findings {
        outln!("  ! {}", f.describe());
    }
    if report.is_clean() {
        outln!("  OK: page counts agree");
        return ExitCode::SUCCESS;
    }
    if report.pages_all_present() {
        outln!("  no page is missing; see the notes above");
        return ExitCode::SUCCESS;
    }
    ExitCode::FAILURE
}

fn run_triage(paths: &[String], out: Option<&Path>) -> ExitCode {
    let mut targets: Vec<PathBuf> = Vec::new();
    for p in paths {
        collect(Path::new(p), &mut targets);
    }
    targets.sort();

    let mut csv = String::from(
        "path,bytes,generation,entries,sheets,thumbnails,pictures,\
         sheets_in_vendor_coding,blank_pages,recovered,attachments,verdict,note\n",
    );
    let mut tally: Vec<(&'static str, usize, usize, usize)> = Vec::new();
    let mut failed = 0usize;
    let service = service();

    for t in &targets {
        let asset = match service.open(t) {
            Ok(asset) => asset,
            // A file whose extension lies is not a failure of the archive, it
            // is a fact about it, and one worth having in the inventory: an
            // archive sweep turns up .xdw files that are really PDFs.
            Err(xdw_salvage::Error::NotAContainer { looks_like }) => {
                let size = std::fs::metadata(t).map(|m| m.len()).unwrap_or(0);
                csv.push_str(&format!(
                    "{},{size},,,,,,,,,,OTHER,{}\n",
                    csv_field(&t.to_string_lossy()),
                    csv_field(looks_like),
                ));
                match tally.iter_mut().find(|(k, ..)| *k == "OTHER") {
                    Some(e) => e.1 += 1,
                    None => tally.push(("OTHER", 1, 0, 0)),
                }
                continue;
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", t.display());
                continue;
            }
        };
        let analysis = service.analyze(&asset);
        let data = &asset.data;
        let doc = &asset.document;
        let cov = analysis.coverage;
        let atts = &analysis.attachments;
        let v = analysis.verdict;
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_field(&t.to_string_lossy()),
            data.len(),
            doc.generation,
            doc.pages.len(),
            cov.sheets,
            cov.thumbnails,
            cov.pictures,
            cov.printer_derived(),
            cov.sheets_blank,
            cov.sheets_recovered + cov.pictures_recovered,
            atts.len(),
            v.as_str(),
            csv_field(v.explain()),
        ));
        match tally.iter_mut().find(|(k, ..)| *k == v.as_str()) {
            Some(e) => {
                e.1 += 1;
                e.2 += cov.sheets;
                e.3 += cov.sheets_blank;
            }
            None => tally.push((v.as_str(), 1, cov.sheets, cov.sheets_blank)),
        }
    }

    if let Some(path) = out {
        if let Err(e) = std::fs::write(path, csv.as_bytes()) {
            return fail(&format!("{}: {e}", path.display()));
        }
        outln!("{} file(s) -> {}", targets.len(), path.display());
    } else {
        print!("{csv}");
    }
    let (mut all_sheets, mut all_blank) = (0usize, 0usize);
    for (k, files, pages, blank) in &tally {
        all_sheets += pages;
        all_blank += blank;
        outln!("  {k:<9} {files:6} file(s) / {pages:7} sheet(s) / {blank:7} would come out blank");
    }
    // The blank count is the number a migration is actually sized by: these
    // pages carry neither a recoverable sheet nor any recoverable artwork, so
    // they arrive empty rather than merely degraded.
    if all_sheets > 0 {
        outln!(
            "  {all_blank} of {all_sheets} sheet(s) ({:.0}%) would come out blank",
            100.0 * all_blank as f64 / all_sheets as f64
        );
    }
    if failed > 0 {
        outln!("  {failed} file(s) could not be read");
    }
    ExitCode::SUCCESS
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                collect(&e.path(), out);
            }
        }
        return;
    }
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "xdw" || ext == "xbd" {
        out.push(p.to_path_buf());
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}
