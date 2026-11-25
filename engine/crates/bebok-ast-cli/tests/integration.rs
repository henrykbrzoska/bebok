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

    assert!(
        output.status.success(),
        "CLI exited with error: {:?}",
        output.stderr
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(&stdout).unwrap()
}

/// Every item kind the legacy `bebok-ast` binary answers for, against the
/// `tests/fixtures` tree, in one pass over the stdio contract. One case per
/// kind is the point - the query logic itself lives in `bebok-core`'s
/// embedded `ast_search`, which the `code_ast` tool uses instead.
#[test]
fn every_kind_is_answered() {
    for (kind, expect_name) in [
        ("impl", Some("impl")),
        ("struct", None),
        ("fn", None),
        ("test", Some("test_something")),
        ("const", Some("MAX_SIZE")),
        ("static", Some("COUNTER")),
    ] {
        let resp = run_cli(json!({ "kind": kind, "limit": 10 }));
        assert!(resp["ok"].as_bool().unwrap(), "{kind}");
        let results = resp["results"].as_array().unwrap();
        assert!(!results.is_empty(), "{kind}: no results");
        if let Some(needle) = expect_name {
            let names: Vec<&str> = results
                .iter()
                .map(|r| r["name"].as_str().unwrap())
                .collect();
            assert!(
                names.iter().any(|n| n.contains(needle)),
                "{kind}: {needle} not in {names:?}"
            );
        }
    }
    // The fixture tree has no enums: an empty answer is still `ok`.
    let resp = run_cli(json!({ "kind": "enum", "limit": 10 }));
    assert!(resp["ok"].as_bool().unwrap());
    assert!(resp["results"].as_array().unwrap().is_empty());
}

/// The filters the request carries: `trait`, `derive`, `return_type`, `ext`
/// and `name_regex` (the last one matching nothing).
#[test]
fn filters_narrow_the_results() {
    for (kind, filters) in [
        ("impl", json!({ "trait": "Display" })),
        ("struct", json!({ "derive": "Serialize" })),
        ("fn", json!({ "return_type": "Result" })),
    ] {
        let resp = run_cli(json!({ "kind": kind, "filters": filters, "limit": 10 }));
        assert!(resp["ok"].as_bool().unwrap(), "{kind}");
        assert!(
            !resp["results"].as_array().unwrap().is_empty(),
            "{kind}: filter matched nothing"
        );
    }

    // `ext` restricts the walk to one language.
    let resp = run_cli(json!({ "kind": "fn", "filters": { "ext": "rs" }, "limit": 100 }));
    for r in resp["results"].as_array().unwrap() {
        let path = r["path"].as_str().unwrap();
        assert!(path.ends_with(".rs"), "got: {path}");
    }

    // A regex that matches nothing is an empty answer, not an error.
    let resp = run_cli(json!({
        "kind": "trait",
        "filters": { "name_regex": "^NonexistentStruct$//" },
        "limit": 10
    }));
    assert!(resp["ok"].as_bool().unwrap());
    assert!(resp["results"].as_array().unwrap().is_empty());
}

/// `limit` caps the result list, and the response reports how many files the
/// walk actually read.
#[test]
fn limit_and_parsed_files_are_reported() {
    let resp = run_cli(json!({ "kind": "fn", "limit": 1 }));
    assert!(resp["ok"].as_bool().unwrap());
    assert_eq!(resp["results"].as_array().unwrap().len(), 1);
    assert!(resp["parsed_files"].as_u64().unwrap() > 0);
}
