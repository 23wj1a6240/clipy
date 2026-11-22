//! Parity with the TypeScript planner: every golden file written by
//! `packages/moments/scripts/make-golden.mjs` is reproduced exactly
//! (moments, order, reasons, frame times, coordinates to 1e-6, and for the
//! model cases the request body byte for byte).

mod common;

use std::fs;
use std::path::PathBuf;

use clipy_local_moments::{plan_rules, plan_with_provider, Input, Planner, ProviderConfig};
use serde_json::Value;

fn golden_files() -> Vec<(String, Value)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("tests/golden exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(
        files.len() >= 9,
        "expected the golden files, found {}",
        files.len()
    );
    files
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
            (name, value)
        })
        .collect()
}

/// Equal JSON, with numbers compared to 1e-6.
fn assert_same(case: &str, path: &str, got: &Value, want: &Value) {
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            assert!((a - b).abs() <= 1e-6, "{case}: {path}: got {a}, want {b}");
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{case}: {path}: length differs\ngot  {got}\nwant {want}"
            );
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert_same(case, &format!("{path}[{i}]"), x, y);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            let mut ka: Vec<&String> = a.keys().collect();
            let mut kb: Vec<&String> = b.keys().collect();
            ka.sort();
            kb.sort();
            assert_eq!(
                ka, kb,
                "{case}: {path}: keys differ\ngot  {got}\nwant {want}"
            );
            for k in ka {
                assert_same(case, &format!("{path}.{k}"), &a[k], &b[k]);
            }
        }
        _ => assert_eq!(got, want, "{case}: {path}"),
    }
}

fn input_of(golden: &Value) -> Input {
    serde_json::from_value(golden["input"].clone()).expect("golden input deserializes")
}

#[test]
fn rules_match_the_typescript_planner() {
    for (name, golden) in golden_files() {
        let plan = plan_rules(&input_of(&golden));
        assert_eq!(plan.planner, Planner::Rules, "{name}");
        let got = serde_json::to_value(&plan.moments).unwrap();
        assert_same(&name, "rules.moments", &got, &golden["rules"]["moments"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spoken_cues_match_the_typescript_planner() {
    let mut cases = 0;
    for (name, golden) in golden_files() {
        let Some(model) = golden.get("model") else {
            continue;
        };
        cases += 1;
        let reply = model["reply"].as_str().unwrap().to_string();
        let server = common::serve(move |_| common::Reply::completion(&reply)).await;
        let config =
            ProviderConfig::new(server.base_url.clone(), "test/model").with_api_key("test-key");
        let plan = plan_with_provider(&input_of(&golden), &config)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        let want_planner = model["planner"].as_str().unwrap();
        assert_eq!(
            serde_json::to_value(plan.planner).unwrap(),
            want_planner,
            "{name}"
        );
        let got = serde_json::to_value(&plan.moments).unwrap();
        assert_same(&name, "model.moments", &got, &model["moments"]);

        let requests = server.requests();
        let want = &model["request"];
        assert_eq!(
            requests.len() as u64,
            want["requests"].as_u64().unwrap(),
            "{name}: request count"
        );
        if let Some(seen) = requests.first() {
            assert_eq!(seen.method, "POST", "{name}");
            assert_eq!(seen.path, want["path"].as_str().unwrap(), "{name}");
            assert_eq!(
                seen.header("authorization"),
                want["authorization"].as_str(),
                "{name}"
            );
            assert_eq!(
                seen.header("content-type"),
                Some("application/json"),
                "{name}"
            );
            let want_body = want["body"].as_str().unwrap();
            if seen.body != want_body {
                let at = seen
                    .body
                    .bytes()
                    .zip(want_body.bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                let from = at.saturating_sub(80);
                panic!(
                    "{name}: request body differs at byte {at}\ngot  ...{}\nwant ...{}",
                    seen.body
                        .get(from..(at + 80).min(seen.body.len()))
                        .unwrap_or(""),
                    want_body
                        .get(from..(at + 80).min(want_body.len()))
                        .unwrap_or("")
                );
            }
        }
    }
    assert!(cases >= 6, "expected model cases, found {cases}");
}

#[test]
fn moving_capture_timeline_changes_coordinates() {
    let golden = golden_files()
        .into_iter()
        .find(|(n, _)| n == "moving-timeline")
        .unwrap()
        .1;
    let with_timeline = plan_rules(&input_of(&golden));
    let mut input = input_of(&golden);
    input.capture_timeline.clear();
    let without = plan_rules(&input);
    assert_ne!(with_timeline.moments, without.moments);
}
