use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;

#[test]
#[ignore]
fn send_telegram_records_http_calls() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = cargo_bin_cmd!("greentic-messaging-tester");
    cmd.arg("send")
        .arg("--provider")
        .arg("telegram")
        .arg("--values")
        .arg("tests/fixtures/values/telegram.json")
        .arg("--text")
        .arg("hello from test");
    let output = cmd.assert().success().get_output().stdout.clone();
    let parsed: Value = serde_json::from_slice(&output)?;
    let http_calls = parsed
        .get("http_calls")
        .and_then(Value::as_array)
        .map(|calls| calls.clone())
        .unwrap_or_default();
    assert!(!http_calls.is_empty(), "expected http_calls to be recorded");
    Ok(())
}
