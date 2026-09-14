//! Checking that a conversion kept every page.
//!
//! A bulk migration's worst failure is the quiet one: a few pages go missing
//! and nobody notices until the original is gone. The software that did the
//! conversion cannot catch that, because it would be marking its own work.
//!
//! What this crate contributes is the half nobody else can supply: the page
//! table read straight out of the container, which says how many pages there
//! should be and how big each one is. Reading the PDF back is left to whatever
//! PDF tooling you already trust — `pdfinfo`, `qpdf --show-npages`, `pdftk`, a
//! PDF crate in your own pipeline. Hand-rolling a second PDF parser here would
//! only add a way for the audit itself to be wrong.

use crate::domain::Document;

/// What a converted document ought to look like.
#[derive(Debug, Clone, PartialEq)]
pub struct Expectation {
    /// Pages a faithful conversion should produce: the document's sheets.
    ///
    /// Thumbnails are not pages, and neither are the pictures a sheet is made
    /// of - a pamphlet cover holding three strips of artwork is one page, not
    /// four. Both are excluded here and counted separately, so a converter that
    /// emitted either is recognisable from its page total alone.
    pub pages: usize,
    /// Thumbnail entries, listed so a count that includes them is recognisable.
    pub previews: usize,
    /// Picture entries, likewise.
    pub pictures: usize,
    /// Paper size of each content page in points, where the container records
    /// one. `None` means the container does not say and nothing can be checked.
    pub sizes: Vec<Option<(f32, f32)>>,
}

impl Expectation {
    /// Read the expectation out of a parsed container.
    pub fn of(doc: &Document) -> Expectation {
        let cov = doc.coverage();
        let pages: Vec<_> = doc.sheets().collect();
        Expectation {
            pages: pages.len(),
            previews: cov.thumbnails,
            pictures: cov.pictures,
            sizes: pages.iter().map(|p| p.paper_points()).collect(),
        }
    }

    /// Page size in millimetres, for reporting.
    pub fn size_mm(&self, index: usize) -> Option<(f32, f32)> {
        self.sizes
            .get(index)
            .copied()
            .flatten()
            .map(|(w, h)| (w * 25.4 / 72.0, h * 25.4 / 72.0))
    }
}

/// One thing that does not line up.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    /// Pages are missing or extra. This is the one that matters.
    PageCount { expected: usize, observed: usize },
    /// The observed count matches the container's entry total rather than its
    /// page total, which means preview entries were converted as pages.
    PreviewsConverted { previews: usize },
    /// The observed count matches pages plus the pictures placed on them, which
    /// means the converter gave each piece of page artwork a sheet of its own.
    PicturesConverted { pictures: usize },
    /// A page came out a different size.
    PageSize {
        index: usize,
        expected: (f32, f32),
        observed: (f32, f32),
    },
}

impl Finding {
    /// Whether this means pages were lost, as against merely changed.
    pub fn is_loss(&self) -> bool {
        matches!(self, Finding::PageCount { expected, observed } if observed < expected)
    }

    pub fn describe(&self) -> String {
        let mm = |p: (f32, f32)| (p.0 * 25.4 / 72.0, p.1 * 25.4 / 72.0);
        match self {
            Finding::PageCount { expected, observed } if observed < expected => format!(
                "page count: container says {expected}, converted file has {observed} \
                 ({} missing)",
                expected - observed
            ),
            Finding::PageCount { expected, observed } => format!(
                "page count: container says {expected}, converted file has {observed} \
                 ({} extra)",
                observed - expected
            ),
            Finding::PicturesConverted { pictures } => format!(
                "the extra pages match the {pictures} picture entr(y/ies); the converter \
                 gave each picture on a page a page of its own"
            ),
            Finding::PreviewsConverted { previews } => format!(
                "the extra pages match the {previews} preview entries; the converter \
                 treated thumbnails as pages"
            ),
            Finding::PageSize {
                index,
                expected,
                observed,
            } => {
                let (ew, eh) = mm(*expected);
                let (ow, oh) = mm(*observed);
                format!(
                    "page {}: container {ew:.1} x {eh:.1} mm, converted {ow:.1} x {oh:.1} mm",
                    index + 1
                )
            }
        }
    }
}

/// The outcome of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub expected_pages: usize,
    pub observed_pages: usize,
    /// How many page sizes could actually be compared.
    pub compared_sizes: usize,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Nothing differs at all.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// No page went missing, whatever else differs.
    pub fn pages_all_present(&self) -> bool {
        !self.findings.iter().any(Finding::is_loss)
    }
}

/// How much two page sizes may differ before it counts, in points.
///
/// A point is about a third of a millimetre. Converters round paper to whole
/// millimetres or to a standard sheet, so a couple of points of drift is
/// normal and anything past that is a real change of size.
const SIZE_TOLERANCE: f32 = 2.0;

/// Compare the container's expectation against what a PDF tool reported.
///
/// `observed_sizes` is in points and may be empty, in which case only the page
/// count is checked. Supply it when your PDF tooling can give you media boxes.
pub fn compare(
    expected: &Expectation,
    observed_pages: usize,
    observed_sizes: &[(f32, f32)],
) -> Report {
    let mut findings = Vec::new();

    if observed_pages != expected.pages {
        findings.push(Finding::PageCount {
            expected: expected.pages,
            observed: observed_pages,
        });
        // A converter that emitted the thumbnails too is a specific, fixable
        // mistake rather than a mystery, so name it.
        if observed_pages == expected.pages + expected.previews && expected.previews > 0 {
            findings.push(Finding::PreviewsConverted {
                previews: expected.previews,
            });
        }
        if observed_pages == expected.pages + expected.pictures && expected.pictures > 0 {
            findings.push(Finding::PicturesConverted {
                pictures: expected.pictures,
            });
        }
    }

    let mut compared = 0usize;
    for (i, want) in expected.sizes.iter().enumerate() {
        let (Some(want), Some(got)) = (want, observed_sizes.get(i)) else {
            continue;
        };
        compared += 1;
        if !close(*want, *got) {
            findings.push(Finding::PageSize {
                index: i,
                expected: *want,
                observed: *got,
            });
        }
    }

    Report {
        expected_pages: expected.pages,
        observed_pages,
        compared_sizes: compared,
        findings,
    }
}

/// Sizes match if they agree either way round; a converter may have turned a
/// landscape page rather than resized it, which loses nothing.
fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    let same = |x: f32, y: f32| (x - y).abs() <= SIZE_TOLERANCE;
    (same(a.0, b.0) && same(a.1, b.1)) || (same(a.0, b.1) && same(a.1, b.0))
}
