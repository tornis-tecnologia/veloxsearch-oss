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
//! Skeleton: the command surface and the pure plan for applying a Fluent Bit
//! configuration safely. No process is spawned yet.
//!
//! ```text
//! velox-agent enroll --mesh https://mesh.example:4320 --token <enrollment token>
//! velox-agent run
//! velox-agent version
//! ```

mod supervisor;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("version") => println!("velox-agent {}", env!("CARGO_PKG_VERSION")),
        Some("enroll") => {
            eprintln!(
                "velox-agent: enroll is not implemented yet (veloxagent/docs/PLAN.md fase 1)"
            );
            std::process::exit(2);
        }
        Some("run") => {
            eprintln!("velox-agent: run is not implemented yet (veloxagent/docs/PLAN.md fase 1)");
            std::process::exit(2);
        }
        _ => {
            eprintln!("usage: velox-agent <enroll --mesh URL --token TOKEN | run | version>");
            std::process::exit(64);
        }
    }
}
