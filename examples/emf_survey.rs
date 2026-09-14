use std::collections::BTreeMap;
use std::path::Path;
use xdw_salvage::domain::PageData;
use xdw_salvage::infrastructure::{self, emf, lzh};
fn main() {
    let mut kinds: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let (mut pages, mut with_dw, mut with_text, mut with_img) = (0usize, 0, 0, 0);
    let service = infrastructure::local_service();
    for arg in std::env::args().skip(1) {
        let Ok(asset) = service.open(Path::new(&arg)) else {
            continue;
        };
        let data = &asset.data;
        let doc = &asset.document;
        for p in doc.sheets() {
            let PageData::Encoded {
                offset,
                len,
                aux_len: Some(exp),
                ..
            } = &p.data
            else {
                continue;
            };
            let Ok(raw) = lzh::decode(&data[*offset..*offset + *len], *exp as usize) else {
                continue;
            };
            let Some(m) = emf::read(&raw) else { continue };
            pages += 1;
            if !m.text.is_empty() {
                with_text += 1;
            }
            if !m.images.is_empty() {
                with_img += 1;
            }
            let mut dw = 0usize;
            for (k, n) in &m.skipped {
                let e = kinds.entry(*k).or_insert((0, 0));
                e.0 += 1;
                e.1 += n;
                if *k == 70 {
                    dw += n;
                }
            }
            if dw > 0 {
                with_dw += 1;
            }
        }
    }
    println!("pages read {pages}: {with_text} carry text, {with_img} carry pictures, {with_dw} carry private comment data");
    println!("\nrecord types not drawn (pages affected, total records):");
    for (k, v) in &kinds {
        let name = match k {
            2 => "POLYBEZIER",
            3 => "POLYGON",
            4 => "POLYLINE",
            9 => "SETWINDOWEXTEX",
            10 => "SETWINDOWORGEX",
            11 => "SETVIEWPORTEXTEX",
            12 => "SETVIEWPORTORGEX",
            13 => "SETBRUSHORGEX",
            14 => "EOF",
            17 => "SETMAPMODE",
            18 => "SETBKMODE",
            19 => "SETPOLYFILLMODE",
            20 => "SETROP2",
            21 => "SETSTRETCHBLTMODE",
            25 => "SETBKCOLOR",
            27 => "MOVETOEX",
            30 => "INTERSECTCLIPRECT",
            33 => "SAVEDC",
            34 => "RESTOREDC",
            35 => "SETWORLDTRANSFORM",
            36 => "MODIFYWORLDTRANSFORM",
            38 => "CREATEPEN",
            39 => "CREATEBRUSHINDIRECT",
            42 => "ELLIPSE",
            43 => "RECTANGLE",
            54 => "LINETO",
            58 => "SETMITERLIMIT",
            59 => "BEGINPATH",
            60 => "ENDPATH",
            62 => "FILLPATH",
            63 => "STROKEANDFILLPATH",
            64 => "STROKEPATH",
            70 => "GDICOMMENT (private)",
            75 => "EXTSELECTCLIPRGN",
            76 => "BITBLT",
            81 => "STRETCHDIBITS",
            85 => "POLYBEZIER16",
            86 => "POLYGON16",
            87 => "POLYLINE16",
            88 => "POLYBEZIERTO16",
            89 => "POLYLINETO16",
            90 => "POLYPOLYLINE16",
            91 => "POLYPOLYGON16",
            95 => "EXTCREATEPEN",
            _ => "?",
        };
        println!("  {k:3} {name:22} {:5} page(s)  {:8} record(s)", v.0, v.1);
    }
}
