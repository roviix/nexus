use nexus_crsr::inject;
use std::fs;
use std::path::Path;

fn main() {
    let files = [
        "/Applications/Cursor.app/Contents/Resources/app/extensions/cursor-always-local/dist/main.js",
        "/Applications/Cursor.app/Contents/Resources/app/extensions/cursor-agent-host/dist/main.js",
    ];

    for path_str in &files {
        let path = Path::new(path_str);
        println!("Checking {}", path.display());
        let content = fs::read_to_string(path).expect("read file");
        let hits_before = inject::count_hits(&content);
        let anchors = inject::count_anchors(&content);
        println!("  hits before: {}, anchors: {}", hits_before, anchors);

        if let Some(patched) = inject::apply(&content) {
            let hits_after = inject::count_hits(&patched);
            println!("  patched! hits after: {}", hits_after);
            // write backup if not exists
            let bak = format!("{}.bak", path_str);
            if !Path::new(&bak).exists() {
                fs::copy(path, &bak).expect("write backup");
                println!("  backup saved to {}", bak);
            }
            fs::write(path, patched).expect("write patched");
            println!("  successfully written to {}", path.display());
        } else {
            println!("  apply returned None (already up-to-date or no anchors)");
        }
    }
}
