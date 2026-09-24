use serde_json::{Value, json};

/// Helper to invoke the CLI binary with a JSON request and return the parsed response.
fn run_cli(request: Value) -> Value {
    let fixture_dir = std::env::current_dir()
        .unwrap()
        .join("tests")
        .join("fixtures");

    let req = serde_json::json!({
        "root": fixture_dir.to_string_lossy(),
        "kind": request["kind"],
        "filters": request.get("filters").cloned().unwrap_or(json!({})),
        "limit": request.get("limit").cloned().unwrap_or(json!(20)),
    });

    // cargo sets CARGO_BIN_EXE_<name> for integration tests of this package.
    let bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_bebok-ast"));

    // The CLI reads one JSON request from stdin (stdio plugin contract).
    use std::io::Write;
    let mut child = std::process::Command::new(&bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to execute bebok-ast binary");
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(req.to_string().as_bytes())
        .expect("failed to write request to bebok-ast stdin");
    let output = child.wait_with_output().expect("failed to run bebok-ast");

    assert!(output.status.success(), "CLI exited with error: {:?}", output.stderr);
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn query_impl_display_finds_foo() {
    let resp = run_cli(json!({
        "kind": "impl",
        "filters": { "trait": "Display" },
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find impl Display for Foo");
    assert!(
        results[0]["name"].as_str().unwrap().contains("impl"),
        "name should mention impl"
    );
}

#[test]
fn query_struct_derive_serialize() {
    let resp = run_cli(json!({
        "kind": "struct",
        "filters": { "derive": "Serialize" },
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find struct Bar with #[derive(Serialize)]");
}

#[test]
fn query_fn_return_result() {
    let resp = run_cli(json!({
        "kind": "fn",
        "filters": { "return_type": "Result" },
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find fn baz() -> Result<()>");
}

#[test]
fn query_test_annotations() {
    let resp = run_cli(json!({
        "kind": "test",
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find #[test] fn");
    // The test function name should contain "test".
    let names: Vec<String> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.contains("test_something")),
        "should find test_something, got: {names:?}"
    );
}

#[test]
fn query_type_filter_ext_rs_only() {
    let resp = run_cli(json!({
        "kind": "fn",
        "filters": { "ext": "rs" },
        "limit": 100
    }));
    assert!(resp["ok"].as_bool().unwrap());
    // All results should be .rs files.
    let results = resp["results"].as_array().unwrap();
    for r in results {
        let path = r["path"].as_str().unwrap();
        assert!(
            path.ends_with(".rs"),
            "result path should end with .rs, got: {path}"
        );
    }
}

#[test]
fn query_limit_one() {
    let resp = run_cli(json!({
        "kind": "fn",
        "limit": 1
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "should return exactly 1 result");
}

#[test]
fn query_empty_results() {
    let resp = run_cli(json!({
        "kind": "trait",
        "filters": { "name_regex": "^NonexistentStruct$//" },
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(results.is_empty(), "should return empty results");
}

#[test]
fn query_returns_parsed_files_count() {
    let resp = run_cli(json!({
        "kind": "fn",
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    assert!(
        resp["parsed_files"].as_u64().unwrap() > 0,
        "should have parsed at least 1 file"
    );
}

#[test]
fn query_enum() {
    let resp = run_cli(json!({
        "kind": "enum",
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    // sample.rs has no enums, so results may be empty.
    assert!(resp["results"].as_array().unwrap().is_empty());
}

#[test]
fn query_const() {
    let resp = run_cli(json!({
        "kind": "const",
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find const MAX_SIZE");
    assert!(
        results[0]["name"].as_str().unwrap().contains("MAX_SIZE"),
        "should find MAX_SIZE"
    );
}

#[test]
fn query_static() {
    let resp = run_cli(json!({
        "kind": "static",
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "should find static COUNTER");
}
