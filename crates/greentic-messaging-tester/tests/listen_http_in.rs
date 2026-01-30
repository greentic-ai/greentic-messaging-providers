use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::{fs, path::PathBuf};
use tempfile::tempdir;

#[test]
fn listen_http_in_mode_writes_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_values =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/values/listen_dummy.json");
    let tempdir = tempdir()?;
    let http_in_path = tempdir.path().join("http_in.json");

    cargo_bin_cmd!("greentic-messaging-tester")
        .arg("listen")
        .arg("--provider")
        .arg("dummy")
        .arg("--values")
        .arg(fixture_values)
        .arg("--public-base-url")
        .arg("https://example.test")
        .arg("--http-in")
        .arg(&http_in_path)
        .arg("--path")
        .arg("/webhook")
        .arg("--http-method")
        .arg("post")
        .arg("--header")
        .arg("x-custom:42")
        .arg("--body")
        .arg("{\"hello\":\"world\"}")
        .assert()
        .success();

    let content = fs::read_to_string(&http_in_path)?;
    let json: Value = serde_json::from_str(&content)?;
    assert_eq!(json["method"], "POST");
    assert_eq!(json["path"], "/webhook");
    assert_eq!(json["headers"]["x-custom"], "42");
    assert_eq!(json["body"], "{\"hello\":\"world\"}");
    Ok(())
}
