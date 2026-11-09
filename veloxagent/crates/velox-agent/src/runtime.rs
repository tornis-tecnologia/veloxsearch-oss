// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The agent's two live behaviours: enrolling once, and then checking in
//! forever.
//!
//! ## Nothing is pushed into the host
//!
//! The agent always initiates. It POSTs to the Mesh, and the answer may carry
//! a configuration; there is no inbound port, no callback, no listening
//! socket. That is what lets an agent sit behind NAT and a firewall that
//! allows only outbound HTTPS, and it is why a compromised Mesh can offer a
//! bad configuration but cannot execute anything on the host.
//!
//! ## State on the host
//!
//! `~/.local/state/velox-agent` or `$VELOX_AGENT_STATE` (`/var/lib/velox-agent`
//! under a system unit) holds three things: the `instance_uid` that survives
//! restarts, the credential the Mesh issued at enrollment (mode `0600`), and
//! the engine's live/previous configuration. Losing the credential means
//! re-enrolling, which is the right failure: it cannot be recovered from the
//! Mesh, only reissued.
//!
//! ## Applying is safe or it is nothing
//!
//! A configuration is written to staging, validated when the engine can
//! validate, promoted atomically, and only then does the engine restart. If
//! the engine will not come up, the previous file goes back and the engine is
//! restarted on it — the host keeps collecting with what it had rather than
//! stopping because a policy was wrong. Either way the agent reports the
//! outcome with the engine's own words, so the operator sees the failure on
//! the Mesh's screen and not only in a log on one host.

use crate::preflight;
use crate::supervisor::EngineFiles;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// How often an idle agent checks in. The spec's default, and the number the
/// offline threshold on the server (5 min) is sized against.
const POLL_SECONDS: u64 = velox_opamp::DEFAULT_POLL_SECONDS;

/// The engine name the Mesh keys a configuration body by. Matches
/// `velox_policy::Engine`'s serialised form.
const FLUENT_BIT: &str = "fluent_bit";

pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        let dir = std::env::var("VELOX_AGENT_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".local/state/velox-agent"))
                    .unwrap_or_else(|_| PathBuf::from("/var/lib/velox-agent"))
            });
        Paths { dir }
    }
    pub fn identity(&self) -> PathBuf {
        self.dir.join("identity.json")
    }
    pub fn engine(&self) -> EngineFiles {
        EngineFiles::fluent_bit(&self.dir)
    }
}

/// What enrollment produced: who we are and how we authenticate. Written once
/// and read on every start.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Identity {
    pub mesh: String,
    pub instance_uid: String,
    pub credential: String,
}

impl Identity {
    pub fn load(p: &Paths) -> Option<Identity> {
        serde_json::from_slice(&std::fs::read(p.identity()).ok()?).ok()
    }

    /// Written `0600`: it is a credential, and a world-readable credential on
    /// a shared host is the whole fleet's problem.
    pub fn save(&self, p: &Paths) -> std::io::Result<()> {
        std::fs::create_dir_all(&p.dir)?;
        let path = p.identity();
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
        }
        std::fs::rename(tmp, path)
    }
}

/// Exchange an enrollment token for an identity. Idempotent only in the sense
/// that running it again enrols a SECOND agent — the Mesh has no way to know
/// two enrollments are the same host, and silently reusing an identity would
/// hide a cloned image.
pub async fn enrol(mesh: &str, token: &str) -> Result<Identity, String> {
    let mesh = mesh.trim_end_matches('/').to_string();
    let hostname = hostname();
    let body = serde_json::json!({
        "token": token,
        "hostname": hostname,
        "attributes": {
            "velox.os": std::env::consts::OS,
            "velox.arch": std::env::consts::ARCH,
            "velox.agent.version": env!("CARGO_PKG_VERSION"),
        }
    });
    let res = client()?
        .post(format!("{mesh}/v1/enroll"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("cannot reach the Mesh at {mesh}: {e}"))?;
    if !res.status().is_success() {
        let code = res.status();
        let msg = res.text().await.unwrap_or_default();
        return Err(format!("the Mesh refused the enrollment ({code}): {msg}"));
    }
    #[derive(serde::Deserialize)]
    struct Out {
        instance_uid: String,
        credential: String,
    }
    let out: Out = res.json().await.map_err(|e| e.to_string())?;
    Ok(Identity {
        mesh,
        instance_uid: out.instance_uid,
        credential: out.credential,
    })
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

/// The supervised engine. One child process; the agent owns its lifetime.
struct Engine {
    bin: PathBuf,
    files: EngineFiles,
    child: Option<Child>,
    dry_run: bool,
}

impl Engine {
    fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            let _ = std::fs::remove_file(self.pid_path());
        }
    }

    /// Where the engine's own stdout and stderr go. Not `/dev/null`: when
    /// data is not arriving, the reason is almost always a line the engine
    /// printed — a rejected bulk request, a TLS failure, a permission denied
    /// on a path it tails. Dropping that output leaves an operator with a
    /// healthy-looking agent and no way to find out why nothing lands. The
    /// file sits next to the configuration, so `velox-agent doctor` and a
    /// support bundle find it in one place.
    /// Where the running engine's pid is recorded, so that an agent which
    /// starts up — after a crash, an upgrade, a `systemctl restart` — can
    /// find the engine its predecessor left behind.
    ///
    /// Without this the agent spawns a SECOND Fluent Bit on the same
    /// configuration: both tail the same files and both write to the same
    /// index, so every line is indexed twice and nothing on screen looks
    /// wrong. Observed on this host — two engines, one config, after a plain
    /// agent restart. A child process cannot be re-adopted by a new parent,
    /// so the only honest invariant is *at most one engine per state
    /// directory*, enforced by reaping before spawning.
    fn pid_path(&self) -> PathBuf {
        self.files
            .live
            .parent()
            .unwrap_or(Path::new("."))
            .join("engine.pid")
    }

    /// Kill the engine recorded in the pid file, if it is still running and
    /// still ours. The command line is checked against our configuration path
    /// because pids are reused, and killing an unrelated process would be far
    /// worse than leaving a stale file behind.
    fn reap_stale(&self) {
        let path = self.pid_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(pid) = raw.trim().parse::<i32>() else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        #[cfg(target_os = "linux")]
        {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            let ours = cmdline.contains(&self.files.live.display().to_string());
            if ours {
                // SIGTERM first: Fluent Bit flushes what it has buffered on a
                // graceful stop, and a KILL here would drop it.
                unsafe { libc::kill(pid, libc::SIGTERM) };
                for _ in 0..50 {
                    if !Path::new(&format!("/proc/{pid}")).exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if Path::new(&format!("/proc/{pid}")).exists() {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                eprintln!(
                    "velox-agent: stopped the engine left behind by a previous run (pid {pid})"
                );
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    fn log_path(&self) -> PathBuf {
        self.files
            .live
            .parent()
            .unwrap_or(Path::new("."))
            .join("engine.log")
    }

    fn start(&mut self) -> Result<(), String> {
        self.stop();
        // Before spawning, make sure no engine from an earlier *process* is
        // still on this configuration.
        self.reap_stale();
        // Append, never truncate: a restart loop whose cause scrolled away is
        // the one case this file exists for.
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path());
        let (out, err) = match log {
            Ok(f) => match f.try_clone() {
                Ok(g) => (Stdio::from(f), Stdio::from(g)),
                Err(_) => (Stdio::from(f), Stdio::null()),
            },
            // A log we cannot open must not stop collection.
            Err(_) => (Stdio::null(), Stdio::null()),
        };
        let child = Command::new(&self.bin)
            .arg("-c")
            .arg(&self.files.live)
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.bin.display()))?;
        let _ = std::fs::write(self.pid_path(), child.id().to_string());
        self.child = Some(child);
        // A config the engine rejects makes it exit immediately; give it a
        // moment and then ask, rather than reporting success on a spawn.
        std::thread::sleep(std::time::Duration::from_millis(700));
        if !self.alive() {
            return Err("the engine exited immediately after starting".into());
        }
        Ok(())
    }

    /// Write, validate, promote, restart — and put the previous file back if
    /// the engine will not come up on the new one.
    fn apply(&mut self, body: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.files.live.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        std::fs::write(&self.files.staging, body).map_err(|e| e.to_string())?;

        if self.dry_run {
            let out = Command::new(&self.bin)
                .arg("--dry-run")
                .arg("-c")
                .arg(&self.files.staging)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                // The engine's own words, but only the ones that say what is
                // wrong. Fluent Bit opens every run with a copyright notice
                // and twelve lines of ASCII art, and reporting the raw output
                // puts that art on the Mesh's screen where the reason should
                // be — observed on 5.2.0, where the `section 'no_such_plugin'
                // tried to instance a plugin name that doesn't exist` line was
                // pushed out of view by the banner.
                let msg = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stderr),
                    String::from_utf8_lossy(&out.stdout)
                );
                return Err(diagnostics(&msg));
            }
        }

        let had_live = self.files.live.exists();
        if had_live {
            let _ = std::fs::rename(&self.files.live, &self.files.previous);
        }
        std::fs::rename(&self.files.staging, &self.files.live).map_err(|e| e.to_string())?;

        match self.start() {
            Ok(()) => Ok(()),
            Err(e) => {
                if had_live && self.files.previous.exists() {
                    let _ = std::fs::rename(&self.files.previous, &self.files.live);
                    let _ = self.start();
                    return Err(format!("{e} — rolled back to the previous configuration"));
                }
                Err(e)
            }
        }
    }
}

/// The lines of an engine's output that diagnose a failure, with its banner
/// dropped.
///
/// Fluent Bit tags what matters with a level — `[error]`, `[warn]` — so those
/// lines are the message. When nothing is tagged (a future version, a crash
/// before logging starts) the tail is the next best thing, because the last
/// words before an abort are usually the reason; an empty result would be
/// worse than noise, since the operator would see a failure with no cause at
/// all.
fn diagnostics(raw: &str) -> String {
    let tagged: Vec<&str> = raw
        .lines()
        .map(str::trim_end)
        .filter(|l| l.contains("[error]") || l.contains("[warn]"))
        .collect();
    if !tagged.is_empty() {
        return tagged.join("\n");
    }
    let tail: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = tail.len().saturating_sub(5);
    tail[start..].join("\n")
}

/// Check in forever. Returns only on an unrecoverable local problem; a Mesh
/// that is down is a transient the loop rides out, because a host must keep
/// collecting while the controller is being upgraded.
pub async fn run() -> Result<(), String> {
    let paths = Paths::from_env();
    let id = Identity::load(&paths).ok_or_else(|| {
        format!(
            "not enrolled: run `velox-agent enroll --mesh URL --token TOKEN` first ({} is empty)",
            paths.dir.display()
        )
    })?;

    let verdict = preflight::probe();
    let (bin, dry_run) = match (preflight::find_engine().0, &verdict) {
        (Some(bin), preflight::Verdict::Ok { dry_run, .. }) => (bin, *dry_run),
        _ => {
            // Report the refusal to the Mesh anyway, then stop: an agent that
            // cannot drive its engine must say so where the operator is
            // looking, not just exit on the host.
            let _ = report_fatal(&id, &verdict.message()).await;
            return Err(verdict.message());
        }
    };
    eprintln!("velox-agent: {}", verdict.message());

    let mut engine = Engine {
        bin,
        files: paths.engine(),
        child: None,
        dry_run,
    };
    // A restart of the agent must not stop collection: if a live config is
    // already on disk, bring the engine up on it before the first check-in.
    if engine.files.live.exists() {
        let _ = engine.start();
    }

    let http = client()?;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut seq = 0u64;
    let mut applied_hash = String::new();
    let mut status = velox_opamp::RemoteConfigStatuses::Unset;
    let mut error = String::new();

    // Backoff between restart attempts, so an engine that cannot start does
    // not become a spawn loop. Resets as soon as one stays up.
    let mut restart_backoff = 1u64;

    loop {
        seq += 1;

        // Supervise, not just report: an engine that died — OOM, a kill, a
        // crash — is brought back on the configuration it already has. This
        // is the agent's whole job between check-ins, and it happens whether
        // or not the Mesh is reachable.
        if !engine.alive() && engine.files.live.exists() {
            match engine.start() {
                Ok(()) => {
                    eprintln!("velox-agent: engine was down, restarted it");
                    error.clear();
                    restart_backoff = 1;
                }
                Err(e) => {
                    error = e;
                    eprintln!(
                        "velox-agent: engine will not start ({error}); retrying in {restart_backoff}s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(restart_backoff)).await;
                    restart_backoff = (restart_backoff * 2).min(60);
                }
            }
        }

        let msg = velox_opamp::AgentToServer {
            instance_uid: id.instance_uid.clone(),
            sequence_num: seq,
            agent_description: None,
            remote_config_status: Some(velox_opamp::RemoteConfigStatus {
                last_remote_config_hash: applied_hash.clone(),
                status,
                error_message: error.clone(),
            }),
            health: Some(velox_opamp::ComponentHealth {
                healthy: engine.alive(),
                start_time_unix_nano: started,
                last_error: error.clone(),
                status: if engine.alive() {
                    "running".into()
                } else {
                    "stopped".into()
                },
                status_time_unix_nano: 0,
                component_health_map: Default::default(),
            }),
            capabilities: vec![
                velox_opamp::AgentCapabilities::ReportsStatus,
                velox_opamp::AgentCapabilities::AcceptsRemoteConfig,
                velox_opamp::AgentCapabilities::ReportsHealth,
            ],
        };

        match http
            .post(format!("{}{}", id.mesh, velox_opamp::DEFAULT_PATH))
            .bearer_auth(&id.credential)
            .json(&msg)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {
                match res.json::<velox_opamp::ServerToAgent>().await {
                    Ok(s2a) => {
                        if let Some(cfg) = s2a.remote_config {
                            if cfg.config_hash != applied_hash {
                                match cfg.config.config_map.get(FLUENT_BIT) {
                                    Some(obj) => match engine.apply(&obj.body) {
                                        Ok(()) => {
                                            applied_hash = cfg.config_hash;
                                            status = velox_opamp::RemoteConfigStatuses::Applied;
                                            error.clear();
                                            eprintln!("velox-agent: applied {applied_hash}");
                                        }
                                        Err(e) => {
                                            // The hash is still echoed back so the
                                            // Mesh can say WHICH revision failed.
                                            applied_hash = cfg.config_hash;
                                            status = velox_opamp::RemoteConfigStatuses::Failed;
                                            error = e;
                                            eprintln!("velox-agent: refused: {error}");
                                        }
                                    },
                                    None => {
                                        applied_hash = cfg.config_hash;
                                        status = velox_opamp::RemoteConfigStatuses::Failed;
                                        error = format!(
                                            "the policy carries no '{FLUENT_BIT}' configuration"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("velox-agent: unreadable answer: {e}"),
                }
            }
            Ok(res) => eprintln!("velox-agent: the Mesh answered {}", res.status()),
            Err(e) => eprintln!("velox-agent: check-in failed: {e}"),
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_seconds())).await;
    }
}

/// The poll interval, overridable so a test does not wait half a minute.
fn poll_seconds() -> u64 {
    std::env::var("VELOX_AGENT_POLL")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(POLL_SECONDS)
}

/// One check-in whose only purpose is to carry a refusal the operator must
/// see on the fleet screen.
async fn report_fatal(id: &Identity, message: &str) -> Result<(), String> {
    let msg = velox_opamp::AgentToServer {
        instance_uid: id.instance_uid.clone(),
        sequence_num: 0,
        agent_description: None,
        remote_config_status: Some(velox_opamp::RemoteConfigStatus {
            last_remote_config_hash: String::new(),
            status: velox_opamp::RemoteConfigStatuses::Failed,
            error_message: message.to_string(),
        }),
        health: Some(velox_opamp::ComponentHealth {
            healthy: false,
            start_time_unix_nano: 0,
            last_error: message.to_string(),
            status: "no engine".into(),
            status_time_unix_nano: 0,
            component_health_map: Default::default(),
        }),
        capabilities: vec![velox_opamp::AgentCapabilities::ReportsHealth],
    };
    client()?
        .post(format!("{}{}", id.mesh, velox_opamp::DEFAULT_PATH))
        .bearer_auth(&id.credential)
        .json(&msg)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {

    /// A failure message that an operator cannot read is a failure message
    /// that costs a support call. Fluent Bit's banner must not survive.
    #[test]
    fn a_refusal_reports_the_reason_not_the_banner() {
        let raw = "Fluent Bit v5.2.0\n* Copyright (C) 2015-2026 The Fluent Bit Authors\n\
                   |  ___| |                | |   | ___ (_) |\n\
                   [2026/11/09 20:16:42.444] [error] [config] section 'no_such_plugin' tried to instance a plugin name that doesn't exist\n\
                   [2026/11/09 20:16:42.444] [error] configuration file contains errors, aborting.\n";
        let out = diagnostics(raw);
        assert!(out.contains("no_such_plugin"), "{out}");
        assert!(!out.contains("Copyright"), "{out}");
        assert!(!out.contains("___"), "{out}");
    }

    /// Output with no level tags at all still has to say something.
    #[test]
    fn untagged_output_falls_back_to_the_tail() {
        let out = diagnostics("one\ntwo\nthree\n");
        assert!(out.contains("three"), "{out}");
    }
    use super::*;

    /// The credential never lands world-readable, and the identity survives a
    /// round trip through the file.
    #[test]
    fn the_identity_is_written_private_and_read_back() {
        let dir = std::env::temp_dir().join(format!("velox-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = Paths { dir: dir.clone() };
        let id = Identity {
            mesh: "http://mesh:4320".into(),
            instance_uid: "uid-1".into(),
            credential: "s3cr3t".into(),
        };
        id.save(&p).expect("save");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(p.identity())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the credential file is group/world readable"
            );
        }
        let back = Identity::load(&p).expect("load");
        assert_eq!(back.instance_uid, "uid-1");
        assert_eq!(back.credential, "s3cr3t");
        assert_eq!(back.mesh, "http://mesh:4320");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not enrolled is a clear instruction, not a panic or a silent loop.
    #[test]
    fn an_unenrolled_agent_is_told_what_to_run() {
        let p = Paths {
            dir: std::env::temp_dir().join("velox-agent-does-not-exist"),
        };
        assert!(Identity::load(&p).is_none());
    }

    /// The poll interval is the spec default and is overridable for tests.
    #[test]
    fn the_poll_interval_defaults_to_the_spec_and_rejects_nonsense() {
        assert_eq!(POLL_SECONDS, 30);
        // Unset / unparsable / zero all fall back rather than spinning.
        std::env::remove_var("VELOX_AGENT_POLL");
        assert_eq!(poll_seconds(), 30);
        std::env::set_var("VELOX_AGENT_POLL", "0");
        assert_eq!(poll_seconds(), 30, "zero would be a busy loop");
        std::env::set_var("VELOX_AGENT_POLL", "nonsense");
        assert_eq!(poll_seconds(), 30);
        std::env::set_var("VELOX_AGENT_POLL", "2");
        assert_eq!(poll_seconds(), 2);
        std::env::remove_var("VELOX_AGENT_POLL");
    }

    /// The engine body is keyed by the name `velox_policy::Engine`
    /// serialises, so a rename on either side fails here rather than
    /// silently delivering an empty configuration.
    #[test]
    fn the_engine_key_matches_the_policy_crates_name() {
        assert_eq!(
            FLUENT_BIT,
            serde_json::to_value(velox_policy::Engine::FluentBit)
                .unwrap()
                .as_str()
                .unwrap()
        );
    }
}
