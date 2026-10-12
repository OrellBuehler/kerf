use kerf_core::*;
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    for p in names {
        match Project::probe_asset(&p) {
            Ok(a) => {
                for s in a.streams.iter().filter(|s| s.kind == StreamKind::Video) {
                    println!(
                        "{:24} codec={:10} pix_fmt={:14} color_space={:12} image={:5} {}x{} transfer={:?}",
                        p.file_name().unwrap().to_string_lossy(),
                        s.codec,
                        s.pix_fmt.clone().unwrap_or_default(),
                        s.color_space.clone().unwrap_or("-".into()),
                        s.image,
                        s.width.unwrap_or(0),
                        s.height.unwrap_or(0),
                        s.color_transfer
                    );
                }
            }
            Err(e) => println!("{:24} ERR {e}", p.file_name().unwrap().to_string_lossy()),
        }
    }
}
