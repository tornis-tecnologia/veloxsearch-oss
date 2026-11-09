// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Is this host ready to run an engine? The agent does not ship Fluent Bit and
//! never downloads it (ADR-069): the operator installs it from their
//! distribution's or the project's repository, and the agent supervises what
//! it finds. That bargain only holds if the agent is loud about an engine it
//! cannot drive, so this module answers three questions before anything is
//! applied:
//!
//! 1. **Is it there?** [`find_engine`] looks on `$PATH` and then at the paths
//!    the upstream packages install to (`/opt/fluent-bit/bin/fluent-bit` on
//!    every Linux package, `C:\Program Files\fluent-bit\bin` on Windows).
//! 2. **Is it new enough?** [`parse_version`] reads `fluent-bit --version`
//!    and [`check`] compares it against [`MIN_FLUENT_BIT`].
//! 3. **Can its configuration be validated before it is applied?**
//!    `--dry-run` is documented but upstream never stated which version
//!    introduced it, so it is *probed*, not assumed (ADR-069 decision 5).
//!
//! The parsing and comparison are pure and tested here; running the binary is
//! the thin part at the bottom.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The oldest Fluent Bit the agent will drive (ADR-069 decision 3). 3.2 is
/// where YAML became the standard configuration format — the agent writes YAML
/// only, and classic `.conf` is deprecated upstream at the end of 2026. Hot
/// reload (2.1.0) and the OTLP input (2.0.0) are already in by then.
pub const MIN_FLUENT_BIT: Version = Version {
    major: 3,
    minor: 2,
    patch: 0,
};

/// A three-part version, compared in the obvious order. Pre-release and build
/// metadata are ignored: a distribution's `4.2.2-1.el9` is 4.2.2 here, which
/// is what the comparison means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Read a version out of whatever `fluent-bit --version` printed. Upstream's
/// own form is `Fluent Bit v3.2.1`; distribution builds and `--version` on
/// older releases have shown `fluent-bit 5.2.0` and a bare `v4.2.2-1.el9`, so
/// the scan is deliberately loose: the first `N.N` (with an optional `.N`) on
/// the first line that carries one. A missing patch is 0, because `3.2` and
/// `3.2.0` name the same release.
pub fn parse_version(output: &str) -> Option<Version> {
    for line in output.lines() {
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            // A digit run that is preceded by a digit or a dot is the tail of
            // something already examined, not the start of a version.
            if i > 0 && (bytes[i - 1].is_ascii_digit() || bytes[i - 1] == '.') {
                i += 1;
                continue;
            }
            let mut parts = Vec::new();
            let mut j = i;
            while parts.len() < 3 {
                let start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j == start {
                    break;
                }
                let n: u32 = bytes[start..j].iter().collect::<String>().parse().ok()?;
                parts.push(n);
                if j < bytes.len()
                    && bytes[j] == '.'
                    && j + 1 < bytes.len()
                    && bytes[j + 1].is_ascii_digit()
                {
                    j += 1;
                } else {
                    break;
                }
            }
            if parts.len() >= 2 {
                return Some(Version {
                    major: parts[0],
                    minor: parts[1],
                    patch: *parts.get(2).unwrap_or(&0),
                });
            }
            i = j.max(i + 1);
        }
    }
    None
}

/// What the agent concluded about the engine on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Usable. `dry_run` says whether the apply plan gets its validation step.
    Ok { version: Version, dry_run: bool },
    /// Installed but older than [`MIN_FLUENT_BIT`].
    TooOld { version: Version },
    /// `--version` ran but printed nothing a version could be read from.
    Unreadable { output: String },
    /// No binary found on `$PATH` or at a packaged location.
    NotFound { looked_in: Vec<PathBuf> },
}

impl Verdict {
    /// Can the agent run an engine at all? A `false` here is a refusal to
    /// start, not a warning: a policy applied to an engine the agent cannot
    /// drive would fail later and less clearly.
    pub fn usable(&self) -> bool {
        matches!(self, Verdict::Ok { .. })
    }

    /// One line for the operator, naming the installed version and what to do.
    /// These are the words `velox-agent doctor` prints and the Mesh shows on
    /// the agent's detail screen, so they say the version, never just "too
    /// old".
    pub fn message(&self) -> String {
        match self {
            Verdict::Ok { version, dry_run: true } => {
                format!("Fluent Bit {version} — usable, configuration is validated before each apply")
            }
            Verdict::Ok { version, dry_run: false } => format!(
                "Fluent Bit {version} — usable, but it has no --dry-run: \
                 a bad configuration is caught by the health check and rolled back, not before the apply"
            ),
            Verdict::TooOld { version } => format!(
                "Fluent Bit {version} is older than the minimum {MIN_FLUENT_BIT} this agent supports \
                 (YAML configuration is the standard format from 3.2 on). \
                 Install a newer build from the Fluent Bit repository for your distribution."
            ),
            Verdict::Unreadable { output } => format!(
                "the Fluent Bit binary ran but its version could not be read from: {:?}",
                output.lines().next().unwrap_or("").trim()
            ),
            Verdict::NotFound { looked_in } => format!(
                "no Fluent Bit binary found. VeloxSearch does not install it (ADR-069): \
                 install it from your distribution's or the project's repository. Looked in: {}",
                looked_in
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Decide from what the probes returned. Pure, so every branch is tested:
/// `version_output` is `None` when no binary was found.
pub fn check(version_output: Option<&str>, dry_run: bool, looked_in: Vec<PathBuf>) -> Verdict {
    let Some(out) = version_output else {
        return Verdict::NotFound { looked_in };
    };
    match parse_version(out) {
        None => Verdict::Unreadable {
            output: out.to_string(),
        },
        Some(version) if version < MIN_FLUENT_BIT => Verdict::TooOld { version },
        Some(version) => Verdict::Ok { version, dry_run },
    }
}

/// Where the upstream packages put the binary, in the order to try after
/// `$PATH`. Every Linux package in the project's repositories installs to
/// `/opt/fluent-bit/bin/fluent-bit`; Fedora's and EPEL's distribution package
/// puts it on the path as `/usr/bin/fluent-bit`.
pub fn packaged_paths() -> Vec<PathBuf> {
    [
        "/opt/fluent-bit/bin/fluent-bit",
        "/usr/bin/fluent-bit",
        "/usr/local/bin/fluent-bit",
        r"C:\Program Files\fluent-bit\bin\fluent-bit.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// The engine binary, from `$VELOX_FLUENT_BIT` if the operator pointed us at
/// it, then `$PATH`, then the packaged locations. Returns the path and
/// everywhere it looked, so a `NotFound` can say where.
pub fn find_engine() -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut looked = Vec::new();
    if let Ok(p) = std::env::var("VELOX_FLUENT_BIT") {
        let p = PathBuf::from(p);
        looked.push(p.clone());
        if p.is_file() {
            return (Some(p), looked);
        }
    }
    for dir in std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        for name in ["fluent-bit", "fluent-bit.exe"] {
            let p = dir.join(name);
            if p.is_file() {
                looked.push(p.clone());
                return (Some(p), looked);
            }
        }
    }
    looked.push(PathBuf::from("$PATH"));
    for p in packaged_paths() {
        looked.push(p.clone());
        if p.is_file() {
            return (Some(p), looked);
        }
    }
    (None, looked)
}

/// Run the probes against a real binary and decide. The only impure function
/// here: `--version` is read-only, and the `--dry-run` probe validates a
/// throwaway config that has no inputs or outputs, so nothing is collected
/// and nothing is shipped.
pub fn probe() -> Verdict {
    let (found, looked) = find_engine();
    let Some(bin) = found else {
        return check(None, false, looked);
    };
    let out = match Command::new(&bin).arg("--version").output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        }
        Err(e) => {
            return Verdict::Unreadable {
                output: format!("{}: {e}", bin.display()),
            }
        }
    };
    check(Some(&out), probe_dry_run(&bin), looked)
}

/// Does this build accept `--dry-run`? Upstream never documented the version
/// that introduced it (ADR-069 decision 5), so ask the binary: `--help` names
/// its own flags, which costs one process and touches nothing.
fn probe_dry_run(bin: &Path) -> bool {
    Command::new(bin)
        .arg("--help")
        .output()
        .map(|o| {
            let s = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            s.contains("--dry-run")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forms `--version` has actually printed, upstream and repackaged.
    #[test]
    fn version_is_read_from_every_shape_upstream_and_distros_print() {
        let cases = [
            ("Fluent Bit v3.2.1", (3, 2, 1)),
            ("Fluent Bit v5.2.0\n", (5, 2, 0)),
            ("fluent-bit 5.2.0", (5, 2, 0)),
            // Fedora / EPEL carry a release suffix.
            ("Fluent Bit v4.2.2-1.el9", (4, 2, 2)),
            // A two-part version is a release, with patch 0.
            ("Fluent Bit v3.2", (3, 2, 0)),
            // Banner first, version on a later line.
            (
                "Fluent Bit\nCopyright (C) The Fluent Bit Authors\nv4.0.3",
                (4, 0, 3),
            ),
        ];
        for (out, (major, minor, patch)) in cases {
            assert_eq!(
                parse_version(out),
                Some(Version {
                    major,
                    minor,
                    patch
                }),
                "could not read a version from {out:?}"
            );
        }
    }

    /// Nothing version-shaped means `Unreadable`, never a guess. A bare
    /// integer is not a version.
    #[test]
    fn output_without_a_version_is_not_guessed_at() {
        for out in ["", "fluent-bit", "command not found", "v5", "5"] {
            assert_eq!(parse_version(out), None, "{out:?} should not parse");
        }
        let v = check(Some("command not found"), false, vec![]);
        assert!(!v.usable());
        assert!(v.message().contains("could not be read"), "{}", v.message());
    }

    /// The minimum is the ADR's, and the boundary is inclusive.
    #[test]
    fn the_minimum_is_three_two_and_the_boundary_is_inclusive() {
        assert_eq!(MIN_FLUENT_BIT.to_string(), "3.2.0");
        let v = |s: &str| check(Some(s), true, vec![]);

        assert!(v("Fluent Bit v3.2.0").usable(), "the minimum itself passes");
        assert!(v("Fluent Bit v3.2.1").usable());
        assert!(v("Fluent Bit v5.2.0").usable());
        // EPEL 9 ships 4.2.2 — the case that made a documented minimum worth
        // having, because EPEL 8 ships 2.2.2.
        assert!(v("Fluent Bit v4.2.2-1.el9").usable());

        for old in [
            "Fluent Bit v3.1.9",
            "Fluent Bit v2.2.2-1.el8",
            "Fluent Bit v1.9.0",
        ] {
            let verdict = v(old);
            assert!(!verdict.usable(), "{old} must be refused");
            assert!(
                matches!(verdict, Verdict::TooOld { .. }),
                "{old}: {verdict:?}"
            );
            // The message names the installed version AND the minimum, so the
            // operator knows which way to move.
            let m = verdict.message();
            assert!(m.contains("3.2.0"), "{m}");
            assert!(m.contains("older than the minimum"), "{m}");
        }
        // The DaemonSet image the core pins today is below the host minimum
        // (ADR-069 consequences) — the test states it so the alignment is not
        // forgotten silently.
        assert!(!v("Fluent Bit v3.1.9").usable());
    }

    /// A usable engine without `--dry-run` is usable, and says what is lost.
    #[test]
    fn a_missing_dry_run_degrades_instead_of_refusing() {
        let with = check(Some("Fluent Bit v4.0.0"), true, vec![]);
        let without = check(Some("Fluent Bit v4.0.0"), false, vec![]);
        assert!(with.usable() && without.usable());
        assert!(with.message().contains("validated before each apply"));
        assert!(
            without.message().contains("no --dry-run"),
            "{}",
            without.message()
        );
        assert!(without.message().contains("rolled back"));
    }

    /// Not found says where it looked and that we do not install it.
    #[test]
    fn not_found_names_the_places_and_the_premise() {
        let v = check(None, false, packaged_paths());
        assert!(!v.usable());
        let m = v.message();
        assert!(m.contains("does not install it"), "{m}");
        assert!(m.contains("/opt/fluent-bit/bin/fluent-bit"), "{m}");
    }

    /// The packaged locations cover both the upstream layout and a
    /// distribution package on the path.
    #[test]
    fn packaged_paths_cover_upstream_and_distro_layouts() {
        let ps: Vec<String> = packaged_paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert!(ps.iter().any(|p| p == "/opt/fluent-bit/bin/fluent-bit"));
        assert!(ps.iter().any(|p| p == "/usr/bin/fluent-bit"));
        assert!(ps.iter().any(|p| p.contains("Program Files")));
    }
}
