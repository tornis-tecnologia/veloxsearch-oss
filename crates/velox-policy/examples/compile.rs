// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Compile a policy against real registry packages and print what an agent
//! would receive. Phase 0 has no screens — the Mesh console is phase 1 — so
//! this is how its deliverable is inspected by hand: the shared renderer, the
//! closed token set, schema-1.1 variables and the fail-closed refusals, all
//! on the actual signed packages.
//!
//! ```sh
//! cd veloxagent
//! cargo run -p velox-policy --example compile -- <registry checkout> [package ...]
//! ```
//!
//! Default packages are `nginx` and `postgres`. The `--host HOST` flag renders
//! for an agent outside the cluster (a published endpoint instead of the
//! in-cluster service name), and `--var name=value` sets a schema-1.1 package
//! variable, the way a policy would.

use std::collections::BTreeMap;
use velox_integrations::Package;
use velox_policy::{
    compile, Activation, AgentSettings, Engine, Output, PackageRenderer, PackageStore, Policy,
    Target,
};

struct Store(BTreeMap<(String, String), Package>);

impl PackageStore for Store {
    fn get(&self, id: &str, version: &str) -> Option<&Package> {
        self.0.get(&(id.to_string(), version.to_string()))
    }
}

const VERSION: &str = "1.0.0";

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!(
            "usage: cargo run -p velox-policy --example compile -- \
             <veloxsearch-registry checkout> [--host HOST[:PORT]|URL] [package ...]"
        );
        std::process::exit(2);
    };
    let mut os_host = None;
    let mut ids: Vec<String> = Vec::new();
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    let mut rest = args.collect::<Vec<_>>().into_iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--host" => os_host = rest.next(),
            "--var" => {
                let pair = rest.next().unwrap_or_default();
                let Some((k, v)) = pair.split_once('=') else {
                    eprintln!("--var wants name=value, got {pair:?}");
                    std::process::exit(2);
                };
                vars.insert(k.to_string(), v.to_string());
            }
            other => ids.push(other.to_string()),
        }
    }
    if ids.is_empty() {
        ids = vec!["nginx".into(), "postgres".into()];
    }

    // 1. Load the packages. In the Mesh these arrive verified
    //    (`catalog::verify_package`); here they come off a checkout, which is
    //    the same `Package` the core's DaemonSet path applies.
    let dir = std::path::Path::new(&root).join("integrations");
    let mut packages = BTreeMap::new();
    for id in &ids {
        match Package::load_from_dir(&dir.join(id)) {
            Ok(p) => {
                let declared: Vec<&str> = p
                    .manifest
                    .variables
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect();
                println!(
                    "loaded {id:<12} index={:<14} otel_asset={:<5} variables={:?}",
                    p.manifest.index,
                    p.otel_config.is_some(),
                    declared
                );
                packages.insert((id.clone(), VERSION.to_string()), p);
            }
            Err(e) => {
                eprintln!("cannot load {id}: {e}");
                std::process::exit(1);
            }
        }
    }
    let store = Store(packages);

    // 2. A policy: the activations, where the data goes, the agent's knobs.
    let policy = Policy {
        id: "p-web".into(),
        name: "web tier".into(),
        revision: 7,
        activations: ids
            .iter()
            .map(|id| Activation {
                package_id: id.clone(),
                package_version: VERSION.into(),
                vars: vars.clone(),
                inputs: Default::default(),
            })
            .collect(),
        outputs: vec![Output::OpenSearch {
            deployment: "logs-a1b2".into(),
        }],
        agent: AgentSettings::default(),
        // The console binds a policy to the deployment's named endpoints and
        // the core resolves them; here the address comes straight from
        // `--host`, so the ids are illustrative.
        endpoints: velox_policy::PolicyEndpoints {
            opensearch: Some("published".into()),
            mesh: Some("published".into()),
        },
        packages: vec![],
    };

    let renderer = PackageRenderer {
        store: &store,
        target: Target {
            deployment: "logs-a1b2".into(),
            namespace: "veloxsearch".into(),
            os_endpoint: os_host
                .as_deref()
                .map(|h| match velox_policy::OsAddress::parse(h) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("--host {h:?}: {e}");
                        std::process::exit(2);
                    }
                }),
        },
    };

    let compiled = match compile(&policy, &renderer) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("compile failed: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "\n=== compiled policy {} revision {}",
        compiled.policy_id, compiled.revision
    );
    println!("hash {}", compiled.hash);
    if let Some(h) = &os_host {
        println!("rendered for an off-cluster agent: endpoint = {h}");
    }
    for (engine, body) in &compiled.bodies {
        let name = match engine {
            Engine::FluentBit => "fluent-bit",
            Engine::OtelCollector => "otelcol",
        };
        println!("\n--- {name} ({} bytes) ---\n{body}", body.len());
    }

    // 3. The same compile is deterministic: the Mesh's replicas must agree on
    //    the hash, because the agent echoes it back to say what it applied.
    let again = compile(&policy, &renderer).expect("recompile");
    assert_eq!(again.hash, compiled.hash);
    println!("recompiled to the same hash: the agent's echo is meaningful");

    // 4. The refusals. Each one is what keeps a registry package from naming
    //    something the engine did not author.
    println!("\n=== fail-closed checks");
    let mut p = policy.clone();
    p.activations[0]
        .vars
        .insert("cluster_admin_ssh".into(), "root@host".into());
    report(
        "a policy variable the manifest never declared",
        compile(&p, &renderer).err(),
    );

    let mut p = policy.clone();
    p.outputs = vec![Output::Gateway {
        endpoint: "https://gateway.veloxsearch:8443".into(),
    }];
    report(
        "a gateway output (the queue tier is phase 3)",
        compile(&p, &renderer).err(),
    );

    let mut p = policy.clone();
    p.outputs = vec![Output::OpenSearch {
        deployment: "someone-elses".into(),
    }];
    report(
        "an output naming another deployment",
        compile(&p, &renderer).err(),
    );

    let mut p = policy.clone();
    p.activations[0].package_version = "9.9.9".into();
    report(
        "a version the verified store never saw",
        compile(&p, &renderer).err(),
    );

    let mut p = policy.clone();
    p.outputs.clear();
    report(
        "a policy with no output at all",
        compile(&p, &renderer).err(),
    );
}

fn report(what: &str, err: Option<velox_policy::CompileError>) {
    match err {
        Some(e) => println!("  refused: {what}\n           {e}"),
        None => {
            eprintln!("  NOT REFUSED: {what} — this is a bug");
            std::process::exit(1);
        }
    }
}
