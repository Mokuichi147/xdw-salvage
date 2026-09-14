// Print the text of every page of the files given.
use std::path::Path;
use xdw_salvage::domain::PageData;
use xdw_salvage::infrastructure::{self, emf, lzh};

fn main() {
    let service = infrastructure::local_service();
    for arg in std::env::args().skip(1) {
        let asset = match service.open(Path::new(&arg)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{arg}: {e}");
                continue;
            }
        };
        let data = &asset.data;
        let doc = &asset.document;
        for (n, p) in doc.sheets().enumerate() {
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
            let Some(page) = emf::read(&raw) else {
                continue;
            };
            println!(
                "--- page {} : {} text run(s), {} record type(s) not drawn, {}x{} units, {:.0}x{:.0} pt",
                n + 1,
                page.text.len(),
                page.skipped.len(),
                page.device.0,
                page.device.1,
                page.points().0,
                page.points().1
            );
            let mut last = f32::MIN;
            for t in &page.text {
                if (t.y - last).abs() > 40.0 {
                    println!();
                }
                last = t.y;
                print!("{}", t.chars.iter().collect::<String>());
            }
            println!();
        }
    }
}
