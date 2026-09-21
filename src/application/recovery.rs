//! Recovery-oriented use cases.

use crate::application::ports::PageDecoder;
use crate::domain::coverage::Coverage;
use crate::domain::page::Page;
use crate::domain::rendering::{Metafile, RasterOp, Source};
use crate::domain::{DisplayPage, Document};

/// Decode one page through the supplied port.
///
/// Empty drawing models are treated as a failed recovery.  This is the same
/// business rule used by both the PDF and HTML adapters, so they cannot drift
/// apart and report different page counts for the same source.
pub fn decode_page<D: PageDecoder + ?Sized>(
    data: &[u8],
    page: &Page,
    decoder: &D,
) -> Option<Metafile> {
    decoder.decode(data, page).filter(|page| !page.is_empty())
}

/// Decode a page with the document context needed by legacy page-sized
/// previews whose adjacent thumbnail contains the rest of the page.
pub fn decode_page_for_document<D: PageDecoder + ?Sized>(
    data: &[u8],
    page: &Page,
    document: &Document,
    decoder: &D,
) -> Option<Metafile> {
    decoder
        .decode_with_document(data, page, document)
        .filter(|page| !page.is_empty())
}

/// ページ本体またはページに重ねる描画を、実際のデコーダーで回収できるか調べる。
///
/// `Page::is_recoverable` はコンテナ上の形式だけを判定するため、EMF/WMFを
/// 内包する符号化ページは構造上は未回収に見える。出力アダプターと同じ判定を
/// 監査にも使い、形式判定だけによる偽陰性を避ける。
pub fn page_is_recoverable<D: PageDecoder + ?Sized>(
    data: &[u8],
    page: &Page,
    document: &Document,
    decoder: &D,
) -> bool {
    page.is_recoverable()
        || decode_page_for_document(data, page, document, decoder).is_some()
        || page.overlays.iter().any(|overlay| {
            decode_overlay(decoder, overlay, page.paper.unwrap_or((21000, 29700))).is_some()
        })
}

/// ページ重ね描画の結果を、空のモデルを除いて返す。
pub fn decode_overlay<D: PageDecoder + ?Sized>(
    decoder: &D,
    overlay: &crate::domain::page::Overlay,
    paper: (u32, u32),
) -> Option<Metafile> {
    decoder
        .decode_overlay(overlay, paper)
        .filter(|drawing| !drawing.is_empty())
}

/// 表示ページが、本文・重ね描画・明示的な空白ページのいずれかとして回収できるか調べる。
pub fn display_page_is_recoverable<D: PageDecoder + ?Sized>(
    data: &[u8],
    display: &DisplayPage,
    document: &Document,
    decoder: &D,
) -> bool {
    display.members.iter().any(|member| {
        document
            .pages
            .get(member.page_index)
            .is_some_and(|page| page_is_recoverable(data, page, document, decoder))
    }) || display.overlays.iter().any(|overlay| {
        decode_overlay(decoder, overlay, display.paper.unwrap_or((21000, 29700))).is_some()
    }) || display_page_is_explicit_blank(display, decoder)
        || (display.members.is_empty() && display.overlays.is_empty())
}

/// Whether a properties-only display page contains a valid drawing which is
/// intentionally empty.  A few drivers store a blank page as a full-page EMF
/// containing only its header/background setup; it must not become a missing
/// page merely because there is no visible primitive to paint.
pub fn display_page_is_explicit_blank<D: PageDecoder + ?Sized>(
    display: &DisplayPage,
    decoder: &D,
) -> bool {
    if !display.members.is_empty() || display.overlays.is_empty() {
        return false;
    }
    let paper = display.paper.unwrap_or((21000, 29700));
    display.overlays.iter().all(|overlay| {
        decoder
            .decode_overlay(overlay, paper)
            .is_some_and(|drawing| drawing.is_empty())
    })
}

/// A small standalone preview is only a thumbnail when a full-sheet text
/// overlay supplies the page's selectable vector content.  Keeping both
/// layers makes the thumbnail's coarse glyph pixels visible beside the clean
/// overlay; in that specific composition the preview is redundant.
pub fn preview_replaced_by_text_overlay<D: PageDecoder + ?Sized>(
    page: &Page,
    overlays: &[crate::domain::page::Overlay],
    paper: (u32, u32),
    decoder: &D,
) -> bool {
    if !matches!(page.data, crate::domain::page::PageData::Preview { .. })
        || page.is_full_size_preview()
    {
        return false;
    }
    overlays.iter().any(|overlay| {
        let Some((x, y, w, h)) = overlay.area else {
            return false;
        };
        if x > paper.0 / 20
            || y > paper.1 / 20
            || w < paper.0.saturating_mul(9) / 10
            || h < paper.1.saturating_mul(9) / 10
        {
            return false;
        }
        decoder.decode_overlay(overlay, paper).is_some_and(|meta| {
            !meta.text.is_empty()
                && meta.images.is_empty()
                && meta.rasters.is_empty()
                && meta.fills.is_empty()
                && meta.shapes.is_empty()
                && meta.paths.is_empty()
        })
    })
}

/// A few printer exports keep a thumbnail-sized raster as the page body while
/// the properties stream carries the same page as a full-sheet vector
/// overlay.  Painting both makes the raster thumbnail appear as a blurry,
/// duplicated page beneath the sharp overlay.  The vector overlay is the
/// authoritative rendering in that composition.
pub fn page_body_replaced_by_vector_overlay<D: PageDecoder + ?Sized>(
    body: &Metafile,
    overlays: &[crate::domain::page::Overlay],
    paper: (u32, u32),
    decoder: &D,
) -> bool {
    if body.images.len() != 1
        || body.rasters.len() != 1
        || !body.text.is_empty()
        || !body.fills.is_empty()
        || !body.shapes.is_empty()
        || !body.paths.is_empty()
    {
        return false;
    }
    let image = body.images[0];
    if image.raster_op != RasterOp::Copy
        || image.clip.is_some()
        || image.clip_path.is_some()
        || image.left.abs() > 1.0
        || image.top.abs() > 1.0
    {
        return false;
    }
    let Source::Inline(index) = image.source else {
        return false;
    };
    let Some(raster) = body.rasters.get(index) else {
        return false;
    };
    let device_w = body.device.0.unsigned_abs() as f32;
    let device_h = body.device.1.unsigned_abs() as f32;
    if device_w <= 0.0
        || device_h <= 0.0
        || image.right < device_w * 0.95
        || image.bottom < device_h * 0.95
        || raster.width == 0
        || raster.height == 0
    {
        return false;
    }
    // Treat only genuinely thumbnail-like page bodies as redundant.  A
    // normal 72-dpi-or-better bitmap may be the intended page artwork even if
    // a vector annotation is also present.
    let paper_points = (
        paper.0 as f32 * 72.0 / 2540.0,
        paper.1 as f32 * 72.0 / 2540.0,
    );
    let dpi_x = raster.width as f32 * 72.0 / paper_points.0.max(1.0);
    let dpi_y = raster.height as f32 * 72.0 / paper_points.1.max(1.0);
    if dpi_x >= 72.0 || dpi_y >= 72.0 {
        return false;
    }

    overlays.iter().any(|overlay| {
        let covers_paper = overlay.area.is_none_or(|(x, y, w, h)| {
            x <= paper.0 / 20
                && y <= paper.1 / 20
                && w >= paper.0.saturating_mul(9) / 10
                && h >= paper.1.saturating_mul(9) / 10
        });
        if !covers_paper {
            return false;
        }
        let Some(meta) = decoder.decode_overlay(overlay, paper) else {
            return false;
        };
        (meta.images.is_empty() && meta.rasters.is_empty())
            && (!meta.text.is_empty() || !meta.fills.is_empty() || !meta.shapes.is_empty())
    })
}

/// Calculate coverage using the structural facts in `Document` and the
/// injected page decoder for pages held in a coded representation.
pub fn coverage<D: PageDecoder + ?Sized>(
    data: &[u8],
    document: &Document,
    decoder: &D,
) -> Coverage {
    let mut result = document.coverage();
    let mut decoded_sheets = 0usize;
    let mut blank_sheets = 0usize;
    for page in document.sheets() {
        if page.is_recoverable() {
            continue;
        }
        // Decode once per sheet. Besides avoiding duplicate work, this keeps
        // the use case deterministic for a stateful custom decoder.
        if decode_page_for_document(data, page, document, decoder).is_some() {
            decoded_sheets += 1;
        } else if page.overlays.iter().any(|overlay| {
            decode_overlay(decoder, overlay, page.paper.unwrap_or((21000, 29700))).is_some()
        }) {
            // Older containers sometimes lose the page offset table while
            // retaining a complete page drawing in document properties.
            // Such a page is recoverable through its overlay even though its
            // page body itself cannot be decoded.
            decoded_sheets += 1;
        } else if !document.pictures_on(page.index).any(|p| p.is_recoverable()) {
            blank_sheets += 1;
        }
    }
    result.sheets_recovered += decoded_sheets;
    result.sheets_blank = blank_sheets;
    result
}

/// A reusable view that gives output adapters the same recovery decision and
/// decoded drawing model.
#[derive(Debug)]
pub struct RecoveryView<'a, D: PageDecoder + ?Sized> {
    data: &'a [u8],
    document: &'a Document,
    decoder: &'a D,
}

impl<'a, D: PageDecoder + ?Sized> RecoveryView<'a, D> {
    pub fn new(data: &'a [u8], document: &'a Document, decoder: &'a D) -> Self {
        Self {
            data,
            document,
            decoder,
        }
    }

    pub fn document(&self) -> &Document {
        self.document
    }

    pub fn decode(&self, page: &Page) -> Option<Metafile> {
        decode_page_for_document(self.data, page, self.document, self.decoder)
    }

    pub fn coverage(&self) -> Coverage {
        coverage(self.data, self.document, self.decoder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::PageDecoder;
    use crate::domain::page::Overlay;
    use crate::domain::rendering::{FontKind, Image, Raster, Text};

    struct VectorOverlay;

    impl PageDecoder for VectorOverlay {
        fn decode(&self, _data: &[u8], _page: &Page) -> Option<Metafile> {
            None
        }

        fn decode_overlay(&self, _overlay: &Overlay, _paper: (u32, u32)) -> Option<Metafile> {
            Some(Metafile {
                text: vec![Text {
                    xs: vec![0.0],
                    y: 1.0,
                    chars: vec!['x'],
                    font_kind: FontKind::Latin,
                    size: 1.0,
                    escapement: 0,
                    rgb: (0, 0, 0),
                    order: 0,
                    bold: false,
                    underline: false,
                }],
                ..Metafile::default()
            })
        }
    }

    fn thumbnail_body(width: u32, height: u32) -> Metafile {
        Metafile {
            device: (width as i32, height as i32),
            frame_mm100: (21000, 29700),
            images: vec![Image {
                left: 0.0,
                top: 0.0,
                right: width as f32,
                bottom: height as f32,
                src: (width, height),
                source: Source::Inline(0),
                raster_op: RasterOp::Copy,
                order: 0,
                clip: None,
                clip_path: None,
            }],
            rasters: vec![Raster {
                width,
                height,
                bits: 8,
                palette: Vec::new(),
                rows: Vec::new(),
                stencil: None,
            }],
            ..Metafile::default()
        }
    }

    fn full_sheet_overlay() -> Overlay {
        Overlay {
            kind: 1,
            expanded: 0,
            coded: Vec::new(),
            pixels: None,
            area: Some((0, 0, 21000, 29700)),
        }
    }

    #[test]
    fn thumbnail_page_body_is_replaced_by_full_sheet_vector_overlay() {
        assert!(page_body_replaced_by_vector_overlay(
            &thumbnail_body(104, 146),
            &[full_sheet_overlay()],
            (21000, 29700),
            &VectorOverlay,
        ));
    }

    #[test]
    fn page_sized_bitmap_is_not_replaced_by_vector_overlay() {
        assert!(!page_body_replaced_by_vector_overlay(
            &thumbnail_body(2480, 3508),
            &[full_sheet_overlay()],
            (21000, 29700),
            &VectorOverlay,
        ));
    }
}
