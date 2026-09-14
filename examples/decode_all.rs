// Verification harness: decode every coded stream of every file given.
use std::path::Path;
use xdw_salvage::domain::PageData;
use xdw_salvage::infrastructure::{self, lzh};

fn main() {
    let (mut ok, mut bad, mut emf) = (0usize, 0usize, 0usize);
    let service = infrastructure::local_service();
    for arg in std::env::args().skip(1) {
        let asset = match service.open(Path::new(&arg)) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let data = &asset.data;
        let doc = &asset.document;
        let mut streams: Vec<(&str, usize, usize, usize)> = Vec::new();
        for p in &doc.pages {
            match &p.data {
                PageData::Encoded {
                    offset,
                    len,
                    aux_len: Some(e),
                    ..
                } => streams.push(("page", *offset, *len, *e as usize)),
                PageData::Preview {
                    offset,
                    len,
                    pixels_at,
                    stored,
                    expanded,
                    ..
                } => streams.push((
                    "preview",
                    offset + pixels_at + 16,
                    (*stored as usize).min(len.saturating_sub(pixels_at + 16)),
                    *expanded as usize,
                )),
                _ => {}
            }
        }
        if let (Some((o, l)), Some((_, e))) = (doc.properties, doc.properties_len) {
            streams.push(("properties", o, l, e as usize));
        }
        for (role, off, len, exp) in streams {
            if len == 0 || off + len > data.len() {
                continue;
            }
            match lzh::decode(&data[off..off + len], exp) {
                Ok(out) if out.len() == exp => {
                    ok += 1;
                    if role == "page" && out.get(40..44) == Some(b" EMF") {
                        emf += 1;
                    }
                }
                Ok(_) => bad += 1,
                Err(e) => {
                    bad += 1;
                    eprintln!("{arg} [{role} @{off}]: {e}");
                }
            }
        }
    }
    println!("decoded {ok}/{} streams; {emf} page(s) are EMF", ok + bad);
}
