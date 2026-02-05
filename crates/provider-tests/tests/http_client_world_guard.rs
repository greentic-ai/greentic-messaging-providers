use std::{fs, path::PathBuf};

use provider_tests::harness::workspace_root;

#[test]
fn providers_use_http_client_world() {
    let root = workspace_root().join("components");
    let mut offenders = Vec::new();
    fn visit(dir: &PathBuf, offenders: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.is_dir() {
                visit(&path, offenders);
            } else if let Some(ext) = path.extension()
                && ext == "wit"
                && let Ok(text) = fs::read_to_string(&path)
                && text.contains("greentic:http/http-client@")
            {
                offenders.push(path);
            }
        }
    }

    visit(&root, &mut offenders);
    if !offenders.is_empty() {
        panic!(
            "Found provider WIT files still importing greentic:http/http-client@: {:?}",
            offenders
        );
    }
}
