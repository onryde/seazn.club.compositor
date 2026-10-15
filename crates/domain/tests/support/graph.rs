//! The workspace's RESOLVED dependency graph, from `cargo metadata`. Shared by the two dependency
//! guards, `crates/domain/tests/deps.rs` and `crates/app/tests/boundary.rs` (which includes this file
//! by `#[path]`).
//!
//! A manifest only shows the keys written in it. The resolved graph shows what cargo actually links:
//! a workspace-inherited or renamed dependency under its real package name, target-specific and
//! optional dependencies (`--all-features`, no platform filter), and every transitive dependency.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// The kind of a dependency edge (`dep_kinds[].kind` in the resolve graph; `null` is normal).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Normal,
    Build,
    Dev,
}

/// Package ids, their names, and each package's resolved edges.
#[derive(Debug)]
pub struct Graph {
    names: BTreeMap<String, String>,
    members: BTreeSet<String>,
    edges: BTreeMap<String, Vec<(String, Vec<Kind>)>>,
}

fn kind_of(v: &Value) -> Kind {
    match v.get("kind").and_then(Value::as_str) {
        None => Kind::Normal,
        Some("build") => Kind::Build,
        Some("dev") => Kind::Dev,
        Some(other) => panic!("unknown dependency kind {other:?} in cargo metadata"),
    }
}

impl Graph {
    /// The workspace's graph. `--locked`: a stale `Cargo.lock` fails here rather than being
    /// re-resolved into a graph nobody committed.
    pub fn load() -> Graph {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let out = Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--all-features",
                "--locked",
                "--manifest-path",
                manifest,
            ])
            .output()
            .expect("run cargo metadata");
        assert!(
            out.status.success(),
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Graph::from_metadata(&serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON"))
    }

    /// The graph in a `cargo metadata --format-version 1` document.
    pub fn from_metadata(meta: &Value) -> Graph {
        let str_of = |v: &Value, key: &str| v[key].as_str().unwrap_or_else(|| panic!("no {key}")).to_string();
        let names = meta["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .map(|p| (str_of(p, "id"), str_of(p, "name")))
            .collect();
        let members = meta["workspace_members"]
            .as_array()
            .expect("workspace_members")
            .iter()
            .map(|m| m.as_str().expect("member id").to_string())
            .collect();
        let mut edges = BTreeMap::new();
        for node in meta["resolve"]["nodes"]
            .as_array()
            .expect("resolve.nodes: run without --no-deps")
        {
            let deps = node["deps"].as_array().expect("node deps");
            let out: Vec<(String, Vec<Kind>)> = deps
                .iter()
                .map(|d| {
                    let kinds = d["dep_kinds"]
                        .as_array()
                        .expect("dep_kinds")
                        .iter()
                        .map(kind_of)
                        .collect();
                    (str_of(d, "pkg"), kinds)
                })
                .collect();
            edges.insert(str_of(node, "id"), out);
        }
        Graph {
            names,
            members,
            edges,
        }
    }

    /// The id of the workspace member called `name`.
    pub fn member(&self, name: &str) -> String {
        let ids: Vec<&String> = self.members.iter().filter(|id| self.names[*id] == name).collect();
        assert_eq!(
            ids.len(),
            1,
            "expected exactly one workspace member named {name:?}, found {ids:?}"
        );
        ids[0].clone()
    }

    /// Names of every package reachable from the member `root`, excluding `root` itself. The root's
    /// own edges are followed when they are of a kind in `root_kinds`; past the root only normal and
    /// build edges are, because a dependency's dev-dependencies are never built for its dependents.
    pub fn closure(&self, root: &str, root_kinds: &[Kind]) -> BTreeSet<String> {
        let root_id = self.member(root);
        let mut seen = BTreeSet::from([root_id.clone()]);
        let mut queue = vec![(root_id.clone(), true)];
        while let Some((id, is_root)) = queue.pop() {
            for (dep, kinds) in self.edges.get(&id).map(Vec::as_slice).unwrap_or_default() {
                let follow = kinds.iter().any(|k| {
                    if is_root {
                        root_kinds.contains(k)
                    } else {
                        *k != Kind::Dev
                    }
                });
                if follow && seen.insert(dep.clone()) {
                    queue.push((dep.clone(), false));
                }
            }
        }
        seen.remove(&root_id);
        seen.iter().map(|id| self.names[id].clone()).collect()
    }
}
