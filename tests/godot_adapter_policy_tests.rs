#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]
//! Offline policy gates for the lockstep Godot adapter and compatibility fixtures.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const ADAPTER_MANIFEST: &str = "crates/signal-fish-client-godot/Cargo.toml";
const MIN_FIXTURE: &str = "tests/godot-compat-min";
const LATEST_FIXTURE: &str = "tests/godot-web-smoke";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parse_toml(path: impl AsRef<Path>) -> toml::Value {
    let path = path.as_ref();
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn dependency<'a>(manifest: &'a toml::Value, name: &str) -> &'a toml::Value {
    manifest["dependencies"]
        .get(name)
        .unwrap_or_else(|| panic!("missing dependency {name}"))
}

fn workspace_version(manifest: &toml::Value) -> &str {
    manifest["workspace"]["package"]["version"]
        .as_str()
        .unwrap_or_else(|| panic!("workspace.package.version must be a string"))
}

#[test]
fn core_manifest_is_godot_independent() {
    let manifest = parse_toml(root().join("Cargo.toml"));
    let dependencies = manifest["dependencies"]
        .as_table()
        .expect("core dependencies must be a table");
    let features = manifest["features"]
        .as_table()
        .expect("core features must be a table");

    assert!(!dependencies.contains_key("godot"));
    assert!(!features.contains_key("transport-godot"));
    assert!(!root().join("src/transports/godot_websocket.rs").exists());
    let package_include = manifest["package"]["include"]
        .as_array()
        .expect("core package.include must be an array");
    for repository_only_test in [
        "!/tests/ci_config_tests.rs",
        "!/tests/godot_adapter_policy_tests.rs",
        "!/tests/shared_core_policy_tests.rs",
    ] {
        assert!(
            package_include
                .iter()
                .any(|value| value.as_str() == Some(repository_only_test)),
            "{repository_only_test} must not ship without its repository inputs"
        );
    }

    let library = fs::read_to_string(root().join("src/lib.rs")).expect("read core crate root");
    assert!(!library.contains("GodotWebSocketTransport"));
}

#[test]
fn adapter_declares_lockstep_core_and_supported_godot_range() {
    let core = parse_toml(root().join("Cargo.toml"));
    let adapter = parse_toml(root().join(ADAPTER_MANIFEST));
    let core_version = workspace_version(&core);

    assert_eq!(
        core["package"]["version"]["workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(
        adapter["package"]["version"]["workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(core["package"]["publish"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        adapter["package"]["publish"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(adapter["package"]["rust-version"].as_str(), Some("1.94.0"));

    let godot = dependency(&adapter, "godot");
    assert_eq!(godot["version"].as_str(), Some(">=0.4.5, <0.6"));
    for feature in [
        "experimental-wasm",
        "experimental-wasm-nothreads",
        "lazy-function-tables",
    ] {
        assert!(
            godot["features"]
                .as_array()
                .expect("godot features must be an array")
                .iter()
                .any(|value| value.as_str() == Some(feature)),
            "adapter must retain godot feature {feature}"
        );
    }

    let core_dependency = dependency(&adapter, "signal-fish-client");
    let expected_core = format!("={core_version}");
    assert_eq!(core_dependency["workspace"].as_bool(), Some(true));
    assert_eq!(core_dependency["default-features"].as_bool(), Some(false));
    assert_eq!(
        core["workspace"]["dependencies"]["signal-fish-client"]["version"].as_str(),
        Some(expected_core.as_str())
    );
    assert_eq!(
        core["workspace"]["dependencies"]["signal-fish-client"]["default-features"].as_bool(),
        Some(false)
    );
    assert_eq!(
        core_dependency["features"].as_array(),
        Some(&vec![toml::Value::String("polling-client".to_string())])
    );
}

#[test]
fn minimum_and_latest_fixtures_pin_the_contract_endpoints() {
    let core = parse_toml(root().join("Cargo.toml"));
    let expected_client = workspace_version(&core);

    for (fixture, expected_godot) in [(MIN_FIXTURE, "0.4.5"), (LATEST_FIXTURE, "0.5.4")] {
        let manifest = parse_toml(root().join(fixture).join("Cargo.toml"));
        let expected_requirement = format!("={expected_godot}");
        assert_eq!(
            dependency(&manifest, "godot")["version"].as_str(),
            Some(expected_requirement.as_str()),
            "{fixture} must use an exact Godot endpoint"
        );
        assert!(manifest["dependencies"]
            .as_table()
            .expect("fixture dependencies must be a table")
            .contains_key("signal-fish-client-godot"));
        assert_eq!(
            package_versions_in_lock(&root().join(fixture).join("Cargo.lock"))
                .get("signal-fish-client")
                .and_then(|versions| versions.iter().next())
                .map(String::as_str),
            Some(expected_client)
        );
        assert_unified_godot_lock(fixture, expected_godot);
    }
}

fn package_versions_in_lock(path: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let lock = parse_toml(path);
    let mut versions = BTreeMap::<String, BTreeSet<String>>::new();
    for package in lock["package"]
        .as_array()
        .expect("Cargo.lock package list must be an array")
    {
        let name = package["name"].as_str().expect("lock package name");
        let version = package["version"].as_str().expect("lock package version");
        versions
            .entry(name.to_string())
            .or_default()
            .insert(version.to_string());
    }
    versions
}

fn assert_unified_godot_lock(fixture: &str, expected: &str) {
    let versions = package_versions_in_lock(&root().join(fixture).join("Cargo.lock"));
    let godot = versions
        .get("godot")
        .expect("fixture lock must contain godot");
    assert_eq!(godot.len(), 1, "{fixture} must resolve exactly one godot");
    assert!(godot.contains(expected));

    for (name, resolved) in versions
        .iter()
        .filter(|(name, _)| *name == "godot" || name.starts_with("godot-"))
    {
        assert_eq!(
            resolved.len(),
            1,
            "{fixture} resolves multiple versions of {name}: {resolved:?}"
        );
    }
}

#[test]
fn minimum_fixture_passes_a_direct_godot_peer_to_the_adapter() {
    let source = fs::read_to_string(root().join(MIN_FIXTURE).join("src/lib.rs"))
        .expect("read minimum compatibility source");
    assert!(source.contains("let peer: Gd<WebSocketPeer> = WebSocketPeer::new_gd();"));
    assert!(source.contains("GodotWebSocketTransport::from_peer(peer)"));
    assert!(source.contains("SignalFishPollingClient::new(wrap_direct_peer()"));
}
