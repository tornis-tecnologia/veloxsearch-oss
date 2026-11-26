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
//! `/opt/velox-agent/state`, or `$VELOX_AGENT_STATE`, holds three things: the
//! `instance_uid` that survives restarts, the credential the Mesh issued at
//! enrollment (mode `0600`), and the engine's live/previous configuration.
//! Losing the credential means re-enrolling, which is the right failure: it
//! cannot be recovered from the Mesh, only reissued.
//!
//! The default is one fixed directory, not one derived from `$HOME`, because
//! a home-derived path made the state follow whoever typed the command:
//! `sudo velox-agent enroll` wrote `/root/.local/state/...`, and a service
//! running as its own user then found nothing and asked to be enrolled again.
//! `/opt/velox-agent` is the install root `deploy/install.sh` lays out (the
//! binary under `bin/`, the environment file under `etc/`, the state here),
//! owned by the `velox-agent` system user. The old home-derived locations
//! are only *looked at*, by `doctor`, to say an identity is stranded there.

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

/// The same engine, Spec 2.0 YAML configuration (ADR-074). Preferred when a
/// configuration carries both.
const FLUENT_BIT_YAML: &str = "fluent_bit_yaml";

pub struct Paths {
    pub dir: PathBuf,
}

/// The install root `deploy/install.sh` lays out. The state directory sits
/// under it; so do `bin/` and `etc/`, which the agent itself never reads.
#[cfg(not(windows))]
pub const INSTALL_ROOT: &str = "/opt/velox-agent";
#[cfg(windows)]
pub const INSTALL_ROOT: &str = r"C:\ProgramData\velox-agent";

impl Paths {
    pub fn from_env() -> Paths {
        Paths {
            dir: state_dir(std::env::var("VELOX_AGENT_STATE").ok()),
        }
    }
    /// Where earlier releases kept the state when `$VELOX_AGENT_STATE` was
    /// unset: under the invoking user's home, else `/var/lib/velox-agent`.
    /// Read by `doctor` only — an identity left there is reported, never
    /// moved behind the operator's back (`install.sh` moves it, saying so).
    pub fn legacy() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(h) = std::env::var("HOME") {
            v.push(PathBuf::from(h).join(".local/state/velox-agent"));
        }
        v.push(PathBuf::from("/var/lib/velox-agent"));
        v
    }
    pub fn identity(&self) -> PathBuf {
        self.dir.join("identity.json")
    }
    /// The configuration files in use: the YAML set when a YAML
    /// configuration is live, else the classic set (also the default before
    /// anything was applied).
    pub fn engine(&self) -> EngineFiles {
        let yaml = EngineFiles::fluent_bit_yaml(&self.dir);
        if yaml.live.exists() {
            yaml
        } else {
            EngineFiles::fluent_bit(&self.dir)
        }
    }

    /// What `{state_dir}` in a Spec 2.0 package means on this host — tail
    /// offsets, buffers — exported to the engine as `VELOX_STATE_DIR`
    /// (ADR-074 decision 7). Under the agent's own state directory, so an
    /// unprivileged install never needs `/var/lib`.
    pub fn engine_state(&self) -> PathBuf {
        self.dir.join("fluent-bit").join("state")
    }
}

/// `$VELOX_AGENT_STATE` when set and non-empty, else `<INSTALL_ROOT>/state`.
/// Pure, so the rule is testable without touching the environment.
pub fn state_dir(env: Option<String>) -> PathBuf {
    env.filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(INSTALL_ROOT).join("state"))
}

/// What enrollment produced: who we are and how we authenticate. Written once
/// and read on every start.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Identity {
    pub mesh: String,
    pub instance_uid: String,
    pub credential: String,
    /// Enrolled with `--insecure`: the Mesh's certificate is not verified on
    /// any request this identity makes. For a Mesh behind an ingress that
    /// serves its controller's self-signed default certificate (a lab on
    /// sslip.io, ADR-072 amendment 4) — the operator's explicit choice at
    /// enrol, remembered so `run` keeps talking to the same Mesh the same way.
    /// Absent in identities written before it existed: verify, as they did.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
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
        std::fs::rename(&tmp, &path)?;
        hand_to_directory_owner(&p.dir, &path)
    }
}

/// `sudo velox-agent enroll` runs as root, but the service runs as the
/// `velox-agent` user that owns the state directory. A `0600` credential left
/// owned by root is one that service cannot read — it would then report "not
/// enrolled" on a host that just enrolled. So when root writes into a
/// directory someone else owns, the file goes to that owner. Root writing
/// into a root-owned directory (a `--run-as-root` install) changes nothing.
#[cfg(unix)]
fn hand_to_directory_owner(dir: &Path, file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let meta = std::fs::metadata(dir)?;
    if meta.uid() == 0 {
        return Ok(());
    }
    std::os::unix::fs::chown(file, Some(meta.uid()), Some(meta.gid()))
}

#[cfg(not(unix))]
fn hand_to_directory_owner(_dir: &Path, _file: &Path) -> std::io::Result<()> {
    Ok(())
}

/// What `doctor` says about the state directory: where it is, whether this
/// host is enrolled, and whether an identity is stranded in a location an
/// earlier release used. Lines, not a verdict — none of this stops a host
/// from collecting, it explains why `run` may ask to be enrolled.
pub fn state_report(p: &Paths) -> Vec<String> {
    let mut out = vec![format!("state: {}", p.dir.display())];
    match Identity::load(p) {
        Some(id) => out.push(format!(
            "enrolled as {} with {}{}",
            id.instance_uid,
            id.mesh,
            if id.insecure { " (certificate not verified)" } else { "" }
        )),
        None if p.identity().exists() => out.push(format!(
            "{} exists but cannot be read by this user — run doctor as the service user, or check its owner",
            p.identity().display()
        )),
        None => out.push("not enrolled".into()),
    }
    for old in Paths::legacy() {
        if old != p.dir && old.join("identity.json").exists() {
            out.push(format!(
                "an identity from an earlier release is in {}: move it to {} (install.sh does this) or set VELOX_AGENT_STATE",
                old.display(),
                p.dir.display()
            ));
        }
    }
    out
}

/// The container's enrollment: `VELOX_MESH_URL` + `VELOX_ENROLL_TOKEN`
/// (+ `VELOX_AGENT_INSECURE`). The image has no shell to wrap `enroll` then
/// `run`, so `run` does it when the host is not enrolled yet. `None` unless
/// both are set: a half-configured container must fail on "not enrolled",
/// not enrol against a default.
pub fn enrolment_from_env(get: impl Fn(&str) -> Option<String>) -> Option<(String, String, bool)> {
    let nonempty = |k: &str| get(k).filter(|v| !v.trim().is_empty());
    let mesh = nonempty("VELOX_MESH_URL")?;
    let token = nonempty("VELOX_ENROLL_TOKEN")?;
    let insecure = get("VELOX_AGENT_INSECURE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    Some((mesh, token, insecure))
}

/// Exchange an enrollment token for an identity. Idempotent only in the sense
/// that running it again enrols a SECOND agent — the Mesh has no way to know
/// two enrollments are the same host, and silently reusing an identity would
/// hide a cloned image.
pub async fn enrol(mesh: &str, token: &str, insecure: bool) -> Result<Identity, String> {
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
    let res = client(insecure)?
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
        insecure,
    })
}

/// The HTTP client every Mesh request goes through. WebPKI roots only
/// (reqwest's `rustls-tls-webpki-roots-no-provider`), so `insecure` is the one
/// way past a certificate no public CA signed — chosen at enrol, never implied.
fn client(insecure: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .danger_accept_invalid_certs(insecure)
        .build()
        .map_err(|e| e.to_string())
}

/// The name the Mesh shows for this host. `VELOX_AGENT_HOSTNAME` first: in a
/// container `/etc/hostname` is the container id, and the compose file sets
/// this to the host's name instead.
fn hostname() -> String {
    pick_hostname(
        std::env::var("VELOX_AGENT_HOSTNAME").ok(),
        std::fs::read_to_string("/etc/hostname").ok(),
        std::env::var("HOSTNAME").ok(),
    )
}

fn pick_hostname(over: Option<String>, file: Option<String>, env: Option<String>) -> String {
    [over, file, env]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The supervised engine. One child process; the agent owns its lifetime.
struct Engine {
    bin: PathBuf,
    files: EngineFiles,
    child: Option<Child>,
    dry_run: bool,
    /// The data-path login, handed down by the Mesh on every check-in and put
    /// in front of the engine as environment variables. Held in memory only:
    /// writing it next to the configuration would put a password on the disk
    /// of every collecting host, which is the thing the `${...}` references in
    /// the compiled config exist to avoid.
    creds: Option<velox_opamp::OutputCredential>,
    /// Integration secrets by id (ADR-074 decision 5), exported as
    /// `VELOX_SECRET_<ID>`. Memory only, like `creds`.
    secrets: std::collections::BTreeMap<String, String>,
    /// Exported as `VELOX_STATE_DIR`; created before the engine starts.
    state: PathBuf,
}

impl Engine {
    /// The engine's environment: the data-path login, the state directory
    /// and the integration secrets. Applied to the dry-run as well as to the
    /// engine, so validation expands `${...}` the way the run will.
    ///
    /// Anything of ours not offered now is removed rather than left
    /// inherited: a value from the agent's own environment would silently
    /// outlive a credential or secret the Mesh revoked.
    fn environment(&self, cmd: &mut Command) {
        match &self.creds {
            Some(c) => {
                cmd.env(velox_policy::OS_USER_ENV, &c.username)
                    .env(velox_policy::OS_PASSWORD_ENV, &c.password);
            }
            None => {
                cmd.env_remove(velox_policy::OS_USER_ENV)
                    .env_remove(velox_policy::OS_PASSWORD_ENV);
            }
        }
        cmd.env(velox_policy::compile2::STATE_DIR_ENV, &self.state);
        let ours: std::collections::BTreeMap<String, &String> = self
            .secrets
            .iter()
            .map(|(id, v)| (velox_policy::compile2::secret_env(id), v))
            .collect();
        for (k, _) in std::env::vars_os() {
            if let Some(k) = k.to_str() {
                if k.starts_with("VELOX_SECRET_") && !ours.contains_key(k) {
                    cmd.env_remove(k);
                }
            }
        }
        for (k, v) in ours {
            cmd.env(k, v);
        }
    }

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
            // Either format's live file: the predecessor may have run the
            // classic configuration and this run the YAML one (ADR-074).
            let dir = self.files.live.parent().unwrap_or(Path::new("."));
            let ours = ["conf", "yaml"].iter().any(|ext| {
                cmdline.contains(&dir.join(format!("fluent-bit.{ext}")).display().to_string())
            });
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
        let mut cmd = Command::new(&self.bin);
        cmd.arg("-c").arg(&self.files.live).stdout(out).stderr(err);
        // Fluent Bit expands `${VAR}` from its own environment at start, so
        // this is where the credential and the secrets meet the configuration.
        // The names come from `velox_policy`, which is also what renders the
        // references into the config — one definition, so the two cannot drift.
        let _ = std::fs::create_dir_all(&self.state);
        self.environment(&mut cmd);
        let child = cmd
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
    /// Apply `body` with `files` — the set its format needs. A change of
    /// format (classic → YAML, ADR-074) swaps the set; when the new one will
    /// not come up, the old set is put back and the engine restarted on it,
    /// and when it does, the old set's files are removed so a restart of the
    /// agent cannot bring the old configuration back.
    fn apply_as(&mut self, files: EngineFiles, body: &str) -> Result<(), String> {
        if files == self.files {
            return self.apply(body);
        }
        let old = std::mem::replace(&mut self.files, files);
        // The engine runs on the old set's file; stop it before the new set
        // is promoted so two engines never share the state directory.
        self.stop();
        match self.apply(body) {
            Ok(()) => {
                let _ = std::fs::remove_file(&old.live);
                let _ = std::fs::remove_file(&old.previous);
                let _ = std::fs::remove_file(&old.staging);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&self.files.live);
                self.files = old;
                if self.files.live.exists() {
                    let _ = self.start();
                }
                Err(format!("{e} — kept the previous configuration"))
            }
        }
    }

    fn apply(&mut self, body: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.files.live.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        std::fs::write(&self.files.staging, body).map_err(|e| e.to_string())?;

        if self.dry_run {
            let mut cmd = Command::new(&self.bin);
            cmd.arg("--dry-run").arg("-c").arg(&self.files.staging);
            self.environment(&mut cmd);
            let out = cmd.output().map_err(|e| e.to_string())?;
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
    let id = match Identity::load(&paths) {
        Some(id) => id,
        None => match enrolment_from_env(|k| std::env::var(k).ok()) {
            Some((mesh, token, insecure)) => {
                let id = enrol(&mesh, &token, insecure).await?;
                id.save(&paths).map_err(|e| {
                    format!("enrolled, but could not write {}: {e}", paths.dir.display())
                })?;
                eprintln!(
                    "velox-agent: enrolled as {} (from the environment)",
                    id.instance_uid
                );
                id
            }
            None => {
                return Err(format!(
                    "not enrolled: run `velox-agent enroll --mesh URL --token TOKEN` first, \
                     or set VELOX_MESH_URL and VELOX_ENROLL_TOKEN ({} has no identity)",
                    paths.dir.display()
                ))
            }
        },
    };

    let verdict = preflight::probe();
    let (bin, dry_run, engine_version) = match (preflight::find_engine().0, &verdict) {
        (Some(bin), preflight::Verdict::Ok { dry_run, version }) => {
            (bin, *dry_run, version.to_string())
        }
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
        creds: None,
        secrets: Default::default(),
        state: paths.engine_state(),
    };
    // A restart of the agent must not stop collection: if a live config is
    // already on disk, bring the engine up on it before the first check-in.
    if engine.files.live.exists() {
        let _ = engine.start();
    }

    let http = client(id.insecure)?;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut seq = 0u64;
    let mut applied_hash = String::new();
    let mut status = velox_opamp::RemoteConfigStatuses::Unset;
    let mut error = String::new();
    // What this host costs (usage.rs), carried on every check-in in the
    // description's non-identifying attributes — the OpAMP slot for facts
    // about the agent that change — so the fleet view can show CPU, memory
    // and the engine's version per agent without a second protocol.
    let mut meter = crate::usage::Meter::default();

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

        let usage = meter.round(engine.child.as_ref().map(|c| c.id()));
        let msg = velox_opamp::AgentToServer {
            instance_uid: id.instance_uid.clone(),
            sequence_num: seq,
            agent_description: Some(describe(&engine_version, engine.alive(), &usage)),
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
                        // Credentials first: a configuration applied in this
                        // same round must start the engine with the newest
                        // login, not the previous one.
                        let offered = s2a.velox_output_credential.as_ref();
                        let before = engine.creds.as_ref().map(|c| c.fingerprint());
                        let after = offered.map(|c| c.fingerprint());
                        if before != after {
                            engine.creds = offered.cloned();
                            match &after {
                                Some(fp) => {
                                    eprintln!("velox-agent: output credential in place ({fp})")
                                }
                                None => {
                                    eprintln!("velox-agent: the Mesh offers no output credential")
                                }
                            }
                            // Restart on a rotation: the engine read the old
                            // value into its process environment at start and
                            // will never see the new one otherwise. Skipped
                            // when a configuration is about to be applied in
                            // this same round, because that restarts anyway.
                            let config_coming = s2a
                                .remote_config
                                .as_ref()
                                .is_some_and(|c| c.config_hash != applied_hash);
                            if !config_coming && engine.files.live.exists() {
                                if let Err(e) = engine.start() {
                                    error = e;
                                    eprintln!(
                                        "velox-agent: could not restart on the new credential: {error}"
                                    );
                                }
                            }
                        }
                        // Integration secrets: same rule as the credential —
                        // compare fingerprints, never values, and restart
                        // only when they changed and no configuration is
                        // about to restart the engine anyway.
                        let before = velox_opamp::secrets_fingerprint(&engine.secrets);
                        let after = velox_opamp::secrets_fingerprint(&s2a.velox_secrets);
                        if before != after {
                            engine.secrets = s2a.velox_secrets.clone();
                            eprintln!(
                                "velox-agent: {} integration secret(s) in place ({after})",
                                engine.secrets.len()
                            );
                            let config_coming = s2a
                                .remote_config
                                .as_ref()
                                .is_some_and(|c| c.config_hash != applied_hash);
                            if !config_coming && engine.files.live.exists() {
                                if let Err(e) = engine.start() {
                                    error = e;
                                    eprintln!(
                                        "velox-agent: could not restart on the new secrets: {error}"
                                    );
                                }
                            }
                        }
                        if let Some(cfg) = s2a.remote_config {
                            if cfg.config_hash != applied_hash {
                                // The YAML configuration when the policy has
                                // one (Spec 2.0), else the classic one.
                                let chosen = cfg
                                    .config
                                    .config_map
                                    .get(FLUENT_BIT_YAML)
                                    .map(|o| (EngineFiles::fluent_bit_yaml(&paths.dir), o))
                                    .or_else(|| {
                                        cfg.config
                                            .config_map
                                            .get(FLUENT_BIT)
                                            .map(|o| (EngineFiles::fluent_bit(&paths.dir), o))
                                    });
                                match chosen {
                                    Some((files, obj)) => match engine.apply_as(files, &obj.body) {
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
                                            "the policy carries no '{FLUENT_BIT}' or '{FLUENT_BIT_YAML}' configuration"
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
    client(id.insecure)?
        .post(format!("{}{}", id.mesh, velox_opamp::DEFAULT_PATH))
        .bearer_auth(&id.credential)
        .json(&msg)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The agent's description for a check-in: what identifies the install
/// (the same three facts `enrol` sent) and what changes round to round —
/// the engine and its version, whether it is up, and the host usage. The
/// Mesh merges the non-identifying map into the agent's stored attributes.
fn describe(
    engine_version: &str,
    engine_up: bool,
    usage: &crate::usage::Usage,
) -> velox_opamp::AgentDescription {
    let mut non_identifying = std::collections::BTreeMap::new();
    non_identifying.insert("velox.engine".to_string(), FLUENT_BIT.to_string());
    non_identifying.insert(
        "velox.engine.version".to_string(),
        engine_version.to_string(),
    );
    // The name Spec 2.0 reads for `since` gates (ADR-074 decision 9),
    // beside the engine-neutral one the fleet view already shows.
    non_identifying.insert(
        "velox.fluent_bit.version".to_string(),
        engine_version.to_string(),
    );
    non_identifying.insert(
        "velox.engine.up".to_string(),
        if engine_up { "true" } else { "false" }.to_string(),
    );
    if let Some(p) = usage.cpu_percent {
        non_identifying.insert("velox.agent.cpu_percent".to_string(), format!("{p}"));
    }
    if let Some(b) = usage.rss_bytes {
        non_identifying.insert("velox.agent.rss_bytes".to_string(), b.to_string());
    }
    velox_opamp::AgentDescription {
        identifying_attributes: [
            ("velox.os".to_string(), std::env::consts::OS.to_string()),
            ("velox.arch".to_string(), std::env::consts::ARCH.to_string()),
            (
                "velox.agent.version".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        non_identifying_attributes: non_identifying,
    }
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
            insecure: false,
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
        assert!(!back.insecure);
        // Verifying is the default for every identity written before the
        // flag existed, and `false` is not written at all.
        assert!(!std::fs::read_to_string(p.identity())
            .unwrap()
            .contains("insecure"));
        let old: Identity =
            serde_json::from_str(r#"{"mesh":"https://m","instance_uid":"u","credential":"c"}"#)
                .unwrap();
        assert!(!old.insecure);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One fixed default, whoever runs the command; the variable still wins.
    #[test]
    fn the_state_lives_under_the_install_root_unless_overridden() {
        assert_eq!(state_dir(None), Path::new(INSTALL_ROOT).join("state"));
        assert_eq!(
            state_dir(Some("  ".into())),
            Path::new(INSTALL_ROOT).join("state")
        );
        assert_eq!(
            state_dir(Some("/srv/agent".into())),
            PathBuf::from("/srv/agent")
        );
        #[cfg(unix)]
        assert_eq!(state_dir(None), PathBuf::from("/opt/velox-agent/state"));
    }

    /// The container's host name beats the container id in /etc/hostname.
    #[test]
    fn the_hostname_override_wins_and_blanks_are_skipped() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(pick_hostname(s("web-1"), s("3f2a9c\n"), s("x")), "web-1");
        assert_eq!(pick_hostname(s(" "), s("3f2a9c\n"), s("x")), "3f2a9c");
        assert_eq!(pick_hostname(None, s(""), s("env-host")), "env-host");
        assert_eq!(pick_hostname(None, None, None), "unknown");
    }

    /// Both variables or nothing; the insecure switch takes the usual truthy
    /// spellings and nothing else.
    #[test]
    fn enrolment_from_the_environment_needs_both_values() {
        let from = |pairs: &[(&str, &str)]| {
            let map: std::collections::HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            enrolment_from_env(|k| map.get(k).cloned())
        };
        let (m, t) = (("VELOX_MESH_URL", "https://m"), ("VELOX_ENROLL_TOKEN", "t"));
        assert_eq!(from(&[m]), None);
        assert_eq!(from(&[m, ("VELOX_ENROLL_TOKEN", "")]), None);
        assert_eq!(from(&[m, t]), Some(("https://m".into(), "t".into(), false)));
        for yes in ["1", "true", "YES", "on"] {
            assert!(
                from(&[m, t, ("VELOX_AGENT_INSECURE", yes)]).unwrap().2,
                "{yes}"
            );
        }
        assert!(!from(&[m, t, ("VELOX_AGENT_INSECURE", "maybe")]).unwrap().2);
    }

    /// doctor names the directory and the enrolment state.
    #[test]
    fn the_state_report_says_where_and_whether() {
        let dir = std::env::temp_dir().join(format!("velox-agent-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = Paths { dir: dir.clone() };
        let r = state_report(&p);
        assert!(r[0].contains(&dir.display().to_string()), "{r:?}");
        assert_eq!(r[1], "not enrolled");
        Identity {
            mesh: "https://m".into(),
            instance_uid: "uid-9".into(),
            credential: "c".into(),
            insecure: true,
        }
        .save(&p)
        .unwrap();
        let r = state_report(&p);
        assert!(
            r[1].contains("uid-9") && r[1].contains("not verified"),
            "{r:?}"
        );
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
