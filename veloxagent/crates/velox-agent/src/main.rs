// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Velox Agent — one static binary per host that enrols with a Velox Mesh,
//! receives a compiled policy and runs the collection engines as child
//! processes: Fluent Bit for logs, the OpenTelemetry Collector for metrics and
//! traces. It is Elastic Agent's model (the agent is a supervisor, the
//! collectors are sub-processes) with an open protocol (OpAMP) and without a
//! Velox-specific line of code inside either engine — both stay replaceable
//! upstream binaries (veloxagent/docs/PLAN.md §3, the Fluent Bit critique).
//!
//! **The engines are not ours to build** (ADR-069): the agent binary carries
//! no Fluent Bit and never downloads one. The engine comes from the host's
//! distribution or the project's repository — the console's manual procedure,
//! or `deploy/install.sh` typing those same commands (amendment 1) — or, in
//! the container, from upstream's own image. The agent supervises what it
//! finds, refusing loudly below `preflight::MIN_FLUENT_BIT`.
//!
//! Skeleton: the command surface, the pure plan for applying a Fluent Bit
//! configuration safely, and the host preflight. No engine is spawned yet.
//!
//! ```text
//! velox-agent enroll --mesh https://mesh.example:4320 --token <enrollment token> [--insecure]
//! velox-agent run
//! velox-agent doctor
//! velox-agent version
//! ```

mod preflight;
mod runtime;
mod supervisor;
mod usage;

/// `--flag value` out of the remaining arguments.
fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Install the process-wide rustls provider before any TLS use. Required,
    // not defensive: reqwest is built on the `-no-provider` feature precisely
    // so ring is not linked as a second provider, which leaves installing one
    // to the caller. The core's `main()` does the same.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("version") => println!("velox-agent {}", env!("CARGO_PKG_VERSION")),
        Some("enroll") => {
            let (Some(mesh), Some(token)) = (flag(&args, "--mesh"), flag(&args, "--token")) else {
                eprintln!("usage: velox-agent enroll --mesh URL --token TOKEN [--insecure]");
                std::process::exit(64);
            };
            // Fleet's `--insecure`: accept a Mesh certificate no public CA
            // signed (an ingress controller's default one). Remembered in the
            // identity; said out loud, because it is a downgrade.
            let insecure = args.iter().any(|a| a == "--insecure");
            if insecure {
                eprintln!(
                    "velox-agent: --insecure: the Mesh's TLS certificate will NOT be verified on this host"
                );
            }
            match runtime::enrol(&mesh, &token, insecure).await {
                Ok(id) => {
                    let paths = runtime::Paths::from_env();
                    if let Err(e) = id.save(&paths) {
                        eprintln!(
                            "velox-agent: enrolled, but could not write {}: {e}",
                            paths.dir.display()
                        );
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            eprintln!(
                                "velox-agent: install with deploy/install.sh (it creates {} for the velox-agent user), run this as root, or set VELOX_AGENT_STATE",
                                paths.dir.display()
                            );
                        }
                        std::process::exit(1);
                    }
                    println!("velox-agent: enrolled as {}", id.instance_uid);
                    println!("state: {}", paths.dir.display());
                }
                Err(e) => {
                    eprintln!("velox-agent: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("run") => {
            if let Err(e) = runtime::run().await {
                eprintln!("velox-agent: {e}");
                std::process::exit(1);
            }
        }
        // Implemented, because it is what makes ADR-069's bargain checkable:
        // the operator installs the engine, so the agent must be able to say
        // whether this host is ready before anything is enrolled.
        Some("doctor") => {
            for line in runtime::state_report(&runtime::Paths::from_env()) {
                println!("{line}");
            }
            let verdict = preflight::probe();
            println!("{}", verdict.message());
            if !verdict.usable() {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!(
                "usage: velox-agent <enroll --mesh URL --token TOKEN [--insecure] | run | doctor | version>"
            );
            std::process::exit(64);
        }
    }
}
