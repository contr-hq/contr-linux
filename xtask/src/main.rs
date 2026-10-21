//! Repository automation: `cargo xtask archlint` checks which workspace crates may depend on which, and
//! `cargo xtask package-linux` builds the Debian and RPM packages.

mod linux_packages;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use serde_json::Value;

const SDK: &str = "contr-sdk";
const DESKTOP: &str = "contr-desktop";
const DAEMON_UI: &str = "contr-daemon-ui";
const UI_COMMON: &str = "contr-ui-common";
/// Workspace crates that are not feature modules; every other crate under `crates/` is one.
const SHELL: [&str; 4] = [DESKTOP, DAEMON_UI, UI_COMMON, "xtask"];

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("archlint") => archlint(),
        Some("package-linux") => linux_packages::build(&workspace_root(), &linux_packages::Options::parse(args)?),
        _ => bail!(
            "usage: cargo xtask archlint | cargo xtask package-linux --deb-binary PATH --rpm-binary PATH \
             [--version VERSION] [--out DIR]"
        ),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn archlint() -> anyhow::Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("cannot run cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).context("cannot parse cargo metadata")?;
    let violations = check(&dependencies(&metadata));
    if violations.is_empty() {
        return Ok(());
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    bail!("{} dependency rule violations", violations.len())
}

/// Each workspace package's normal and build dependencies, by name.
fn dependencies(metadata: &Value) -> BTreeMap<String, Vec<String>> {
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let name = package["name"].as_str()?.to_owned();
            let dependencies = package["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|dependency| dependency["kind"].as_str() != Some("dev"))
                .filter_map(|dependency| dependency["name"].as_str().map(str::to_owned))
                .collect();
            Some((name, dependencies))
        })
        .collect()
}

fn check(graph: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let is_module = |name: &str| graph.contains_key(name) && !SHELL.contains(&name);
    let is_api = |name: &str| is_module(name) && name.ends_with("-api");
    let is_feature = |name: &str| is_module(name) && !name.ends_with("-api");

    let mut violations = Vec::new();
    for (package, dependencies) in graph {
        for dependency in dependencies {
            let reason = if is_api(package) && (dependency == SDK || dependency == DAEMON_UI || is_feature(dependency))
            {
                Some("an -api crate holds plain types and reaches neither the daemon nor a module")
            } else if is_feature(package) && is_feature(dependency) {
                Some("a module reaches another module only through its -api crate")
            } else if (is_module(package) || package == DAEMON_UI || package == UI_COMMON) && dependency == DESKTOP {
                Some("nothing depends on the application crate")
            } else if package == UI_COMMON && (dependency == SDK || dependency == DAEMON_UI || is_module(dependency)) {
                Some("shared widgets know nothing of the daemon or the modules")
            } else if package == DAEMON_UI && is_module(dependency) {
                Some("the daemon connection knows nothing of the modules")
            } else {
                None
            };
            if let Some(reason) = reason {
                violations.push(format!("{package} -> {dependency}: {reason}"));
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        edges
            .iter()
            .map(|(package, dependencies)| {
                (
                    (*package).to_owned(),
                    dependencies.iter().map(|name| (*name).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn modules_meet_through_api_crates() {
        let graph = graph(&[
            (DESKTOP, &["contr-images", "contr-containers"]),
            ("contr-images", &[SDK, DAEMON_UI, UI_COMMON, "contr-containers-api"]),
            ("contr-containers", &[SDK, DAEMON_UI, "contr-containers-api"]),
            ("contr-containers-api", &[]),
            (DAEMON_UI, &[SDK, UI_COMMON]),
            (UI_COMMON, &[]),
        ]);
        assert!(check(&graph).is_empty());
    }

    #[test]
    fn rejects_module_to_module_and_api_to_daemon() {
        let graph = graph(&[
            ("contr-images", &["contr-containers"]),
            ("contr-containers", &[]),
            ("contr-containers-api", &[SDK]),
            (UI_COMMON, &[DAEMON_UI]),
            (DAEMON_UI, &[]),
        ]);
        assert_eq!(check(&graph).len(), 3);
    }
}
