// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! `velox` — the VeloxSearch operator CLI (ADR-001 option C).
//!
//! Today it has one subcommand, `init`: a one-command bootstrap that installs
//! VeloxSearch onto the operator's *current kubeconfig context*. It applies the
//! generic single-file manifest (`deploy/install.yaml`, ADR-027) with
//! server-side apply, waits for the Deployment to become ready, then prints the
//! next steps (port-forward + first-run setup URL).
//!
//! Authenticated-pull flow: by default `init` stays inside the side-loaded-image
//! model (ADR-025) — it does NOT pull from or depend on a registry. When the
//! cluster must pull the image from a registry instead, pass `--pull-token`
//! (with `--registry` / `--pull-user`): `init` then creates a
//! `kubernetes.io/dockerconfigjson` image-pull Secret in `veloxsearch-system`
//! *before* applying the manifest, so the `veloxsearch` ServiceAccount's
//! `imagePullSecrets` can resolve it. The registry stays a flag, not a
//! hardcoded assumption — the final registry choice is still pending (ADR-025).

// The whole binary is server-only — it needs the Kubernetes client layer.
#[cfg(not(feature = "ssr"))]
fn main() {}

#[cfg(feature = "ssr")]
fn main() {
    std::process::exit(imp::run());
}

#[cfg(feature = "ssr")]
mod imp {
    use anyhow::{Context, Result};

    /// The generic install manifest, embedded so the binary is self-contained
    /// (ADR-027). One source of truth: the same file `kubectl apply -f` uses.
    const INSTALL_YAML: &str = include_str!("../../deploy/install.yaml");

    /// SSA field manager for objects this installer owns — distinct from the
    /// app's own `veloxsearch` / `veloxsearch-bootstrap` managers so ownership
    /// is honest about who applied the install manifest.
    const FIELD_MANAGER: &str = "velox-init";

    /// The namespace the install manifest creates and the app runs in (ADR-027).
    /// The image-pull Secret must live here so the `veloxsearch` SA can use it.
    const NAMESPACE: &str = "veloxsearch-system";

    /// Default registry host for the pull Secret — the one the public release
    /// image lives on (`docker.io/tornistecnologia/veloxsearch-oss`). Note that
    /// the public image needs NO pull Secret at all; this default only matters
    /// when `--pull-token` is given, i.e. when pulling from a private mirror.
    /// Override with `--registry`.
    const DEFAULT_REGISTRY: &str = "docker.io";

    /// Default registry username for the pull Secret. Override with `--pull-user`
    /// to match your registry's pull/deploy-token username (many registries
    /// generate their own username for a read-only deploy token).
    const DEFAULT_PULL_USER: &str = "veloxsearch";

    /// Default name of the created image-pull Secret. Must match the
    /// `imagePullSecrets` entry on the ServiceAccount in `deploy/install.yaml`.
    const DEFAULT_PULL_SECRET_NAME: &str = "velox-pull";

    const USAGE: &str = "\
velox — VeloxSearch operator CLI

USAGE:
    velox init [OPTIONS]

COMMANDS:
    init        Install VeloxSearch onto the current kubeconfig context

OPTIONS (init):
    --pull-token <TOKEN>       Registry password/token. When given, velox creates
                               an image-pull Secret (kubernetes.io/dockerconfigjson)
                               in veloxsearch-system BEFORE applying the manifest,
                               enabling authenticated image pulls. Omit it to
                               install with no secret (side-loaded image, ADR-025).
    --pull-user <USER>         Registry username for the pull Secret
                               [default: veloxsearch].
    --registry <HOST>          Registry host the Secret authenticates to
                               [default: docker.io]. Only relevant with
                               --pull-token; override to match your mirror.
    --pull-secret-name <NAME>  Name of the created Secret [default: velox-pull].
    --dry-run                  Parse and print what would be applied (manifest
                               objects + the pull Secret) without touching a
                               cluster.
    -h, --help                 Print this help
";

    /// Parsed `init` options. `--pull-token` is what switches on the
    /// authenticated-pull path; without it, behaviour is exactly as before.
    pub(crate) struct InitOpts {
        pub dry_run: bool,
        pub pull_token: Option<String>,
        pub pull_user: String,
        pub registry: String,
        pub pull_secret_name: String,
    }

    impl Default for InitOpts {
        fn default() -> Self {
            Self {
                dry_run: false,
                pull_token: None,
                pull_user: DEFAULT_PULL_USER.to_string(),
                registry: DEFAULT_REGISTRY.to_string(),
                pull_secret_name: DEFAULT_PULL_SECRET_NAME.to_string(),
            }
        }
    }

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("init") => init(&args[1..]),
            Some("-h") | Some("--help") | None => {
                print!("{USAGE}");
                0
            }
            Some(other) => {
                eprintln!("velox: unknown command '{other}'\n");
                print!("{USAGE}");
                2
            }
        }
    }

    fn init(args: &[String]) -> i32 {
        let opts = match parse_init_opts(args) {
            Ok(Some(opts)) => opts,
            Ok(None) => {
                print!("{USAGE}");
                return 0;
            }
            Err(msg) => {
                eprintln!("velox init: {msg}\n");
                print!("{USAGE}");
                return 2;
            }
        };

        let manifest = match Manifest::parse(INSTALL_YAML) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("velox init: {e:#}");
                return 1;
            }
        };

        if opts.dry_run {
            println!(
                "Parsed {} object(s) from the embedded install manifest:",
                manifest.docs.len()
            );
            for s in manifest.summaries() {
                println!("  - {s}");
            }
            if opts.pull_token.is_some() {
                println!(
                    "\nWould create image-pull Secret (applied BEFORE the manifest):\n  \
                     - Secret/{} ({NAMESPACE}) type=kubernetes.io/dockerconfigjson \
                     registry={} user={} [token REDACTED]",
                    opts.pull_secret_name, opts.registry, opts.pull_user
                );
            } else {
                println!(
                    "\nNo --pull-token given: no image-pull Secret (side-loaded image, ADR-025)."
                );
            }
            println!("\n(--dry-run: nothing was applied to any cluster.)");
            return 0;
        }

        // Real apply needs an async runtime + the kube client. Build it here so
        // --dry-run stays a pure, cluster-free path.
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("velox init: failed to start runtime: {e}");
                return 1;
            }
        };
        match rt.block_on(apply(&manifest, &opts)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("velox init: {e:#}");
                1
            }
        }
    }

    /// Parse `init` flags. Returns `Ok(None)` when `-h/--help` was requested,
    /// `Err(msg)` on a bad/missing argument value.
    pub(crate) fn parse_init_opts(
        args: &[String],
    ) -> std::result::Result<Option<InitOpts>, String> {
        let mut opts = InitOpts::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--dry-run" => opts.dry_run = true,
                "-h" | "--help" => return Ok(None),
                "--pull-token" => {
                    opts.pull_token = Some(take_value(&mut it, "--pull-token")?);
                }
                "--pull-user" => opts.pull_user = take_value(&mut it, "--pull-user")?,
                "--registry" => opts.registry = take_value(&mut it, "--registry")?,
                "--pull-secret-name" => {
                    opts.pull_secret_name = take_value(&mut it, "--pull-secret-name")?;
                }
                // `--flag=value` form.
                other if other.starts_with("--pull-token=") => {
                    opts.pull_token = Some(strip_eq(other));
                }
                other if other.starts_with("--pull-user=") => opts.pull_user = strip_eq(other),
                other if other.starts_with("--registry=") => opts.registry = strip_eq(other),
                other if other.starts_with("--pull-secret-name=") => {
                    opts.pull_secret_name = strip_eq(other);
                }
                other => return Err(format!("unknown option '{other}'")),
            }
        }
        Ok(Some(opts))
    }

    fn take_value(
        it: &mut std::slice::Iter<'_, String>,
        flag: &str,
    ) -> std::result::Result<String, String> {
        it.next()
            .cloned()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{flag} requires a value"))
    }

    fn strip_eq(arg: &str) -> String {
        arg.split_once('=').map(|x| x.1).unwrap_or("").to_string()
    }

    async fn apply(manifest: &Manifest, opts: &InitOpts) -> Result<()> {
        // Install the process-wide rustls provider before any TLS use. The
        // binary links no provider by default on purpose, so this is required
        // rather than defensive (mirrors src/main.rs).
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let client = veloxsearch::k8s::client().await?;
        let context = current_context_label();
        println!("Installing VeloxSearch onto the current kubeconfig context{context}…");

        // Authenticated-pull: create the image-pull Secret BEFORE the manifest,
        // so the ServiceAccount's `imagePullSecrets` resolves it (the namespace
        // is created in the same idempotent SSA bundle). Without --pull-token
        // this is skipped — the side-loaded-image path (ADR-025) is unchanged.
        if let Some(token) = opts.pull_token.as_deref() {
            let bundle = pull_secret_bundle(
                &opts.registry,
                &opts.pull_user,
                token,
                &opts.pull_secret_name,
            )?;
            veloxsearch::bootstrap::apply_bundle(&client, &bundle, FIELD_MANAGER)
                .await
                .context("creating image-pull secret")?;
            println!(
                "  ✓ image-pull Secret {}/{} ready (registry {})",
                NAMESPACE, opts.pull_secret_name, opts.registry
            );
        }

        veloxsearch::bootstrap::apply_bundle(&client, INSTALL_YAML, FIELD_MANAGER)
            .await
            .context("applying install.yaml")?;
        println!("  ✓ applied {} object(s)", manifest.docs.len());

        let (ns, name) = manifest
            .deployment()
            .context("install.yaml has no Deployment to wait for")?;
        println!("  … waiting for Deployment {ns}/{name} to become ready (up to 300s)");
        veloxsearch::bootstrap::wait_deploy(&client, &ns, &name, 300)
            .await
            .with_context(|| format!("Deployment {ns}/{name} did not become ready"))?;
        println!("  ✓ Deployment {ns}/{name} is ready");

        print!("\n{}", manifest.next_steps());
        Ok(())
    }

    /// Best-effort label of the active kubeconfig context for the install banner.
    fn current_context_label() -> String {
        std::env::var("VELOX_CONTEXT")
            .ok()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default()
    }

    /// The embedded manifest as a list of objects, with the few lookups the
    /// installer needs (which Deployment to wait on, how to reach the Service).
    pub(crate) struct Manifest {
        pub docs: Vec<serde_json::Value>,
    }

    impl Manifest {
        pub fn parse(yaml: &str) -> Result<Self> {
            let mut docs = Vec::new();
            for de in serde_yaml::Deserializer::from_str(yaml) {
                let v: serde_json::Value =
                    serde::Deserialize::deserialize(de).context("parsing install.yaml")?;
                // Skip comment-only / empty documents (deserialize to null).
                if v.is_object() {
                    docs.push(v);
                }
            }
            if docs.is_empty() {
                anyhow::bail!("install.yaml parsed to zero objects");
            }
            Ok(Self { docs })
        }

        /// `Kind/name (namespace)` for each object, for --dry-run output.
        pub fn summaries(&self) -> Vec<String> {
            self.docs
                .iter()
                .map(|d| {
                    let kind = str_at(d, "/kind").unwrap_or("?");
                    let name = str_at(d, "/metadata/name").unwrap_or("?");
                    match str_at(d, "/metadata/namespace") {
                        Some(ns) => format!("{kind}/{name} ({ns})"),
                        None => format!("{kind}/{name}"),
                    }
                })
                .collect()
        }

        /// The (namespace, name) of the single app Deployment to wait on.
        pub fn deployment(&self) -> Option<(String, String)> {
            self.find("Deployment").map(|(ns, name, _)| (ns, name))
        }

        /// The (namespace, name, port) of the app Service, for the port-forward
        /// hint.
        pub fn service(&self) -> Option<(String, String, i64)> {
            let (ns, name, d) = self.find("Service")?;
            let port = d
                .pointer("/spec/ports/0/port")
                .and_then(|v| v.as_i64())
                .unwrap_or(80);
            Some((ns, name, port))
        }

        fn find(&self, kind: &str) -> Option<(String, String, &serde_json::Value)> {
            self.docs.iter().find_map(|d| {
                if str_at(d, "/kind") != Some(kind) {
                    return None;
                }
                let name = str_at(d, "/metadata/name")?.to_string();
                let ns = str_at(d, "/metadata/namespace")?.to_string();
                Some((ns, name, d))
            })
        }

        /// The "now do this" block printed after a successful install.
        pub fn next_steps(&self) -> String {
            let (ns, svc, port) = self
                .service()
                .unwrap_or_else(|| ("veloxsearch-system".into(), "veloxsearch".into(), 80));
            next_steps(&ns, &svc, port, 3000)
        }
    }

    fn str_at<'a>(v: &'a serde_json::Value, ptr: &str) -> Option<&'a str> {
        v.pointer(ptr).and_then(|x| x.as_str())
    }

    /// Pure renderer for the post-install instructions. Port-forward is the
    /// zero-assumption first contact (ADR-027); the SPA is a single-URL app
    /// (ADR-032) so first run lands on the in-app admin setup screen.
    pub(crate) fn next_steps(ns: &str, svc: &str, svc_port: i64, local_port: u16) -> String {
        format!(
            "VeloxSearch is installed. Next steps:\n\
             \n\
             1. Forward the UI to your machine:\n\
             \x20    kubectl -n {ns} port-forward svc/{svc} {local_port}:{svc_port}\n\
             \n\
             2. Open http://localhost:{local_port}\n\
             \x20    On first run the app shows the admin-setup screen — create your\n\
             \x20    account there, then it checks the cluster and self-installs\n\
             \x20    cert-manager + the OpenSearch operator (ADR-014/023).\n\
             \n\
             Note (ADR-025): the image is side-loaded, not pulled from a registry.\n\
             If pods report ImagePullBackOff, import the image on each node:\n\
             \x20    sudo ctr -n k8s.io images import veloxsearch.tar     # k3s/containerd\n\
             \x20    sudo k0s ctr -n k8s.io images import veloxsearch.tar # k0s\n"
        )
    }

    /// Build the YAML bundle (Namespace + dockerconfigjson Secret) that
    /// `apply_bundle` applies before the install manifest. The Namespace is
    /// included so the Secret has somewhere to land on a virgin cluster;
    /// re-applying it (install.yaml ships it too) is a harmless idempotent SSA.
    pub(crate) fn pull_secret_bundle(
        registry: &str,
        user: &str,
        token: &str,
        secret_name: &str,
    ) -> Result<String> {
        let secret = pull_secret_object(registry, user, token, secret_name);
        let ns = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": { "name": NAMESPACE },
        });
        let ns_yaml = serde_yaml::to_string(&ns).context("serializing namespace")?;
        let secret_yaml = serde_yaml::to_string(&secret).context("serializing pull secret")?;
        Ok(format!("{ns_yaml}---\n{secret_yaml}"))
    }

    /// The `kubernetes.io/dockerconfigjson` Secret as a JSON object. Uses
    /// `stringData` so the API server base64-encodes the outer value; only the
    /// inner `auth` (`base64(user:token)`) must be encoded here. Idempotent
    /// under SSA with the `velox-init` field manager.
    fn pull_secret_object(
        registry: &str,
        user: &str,
        token: &str,
        secret_name: &str,
    ) -> serde_json::Value {
        let auth = base64_std(format!("{user}:{token}").as_bytes());
        let dockerconfig = serde_json::json!({
            "auths": {
                registry: {
                    "username": user,
                    "password": token,
                    "auth": auth,
                }
            }
        });
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "type": "kubernetes.io/dockerconfigjson",
            "metadata": { "name": secret_name, "namespace": NAMESPACE },
            "stringData": { ".dockerconfigjson": dockerconfig.to_string() },
        })
    }

    /// Standard-alphabet base64 (RFC 4648), padded. Kept dependency-free — the
    /// only use is the Secret's `auth` field, not a hot path.
    fn base64_std(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6 & 0x3f) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[(n & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn embedded_manifest_parses_to_objects() {
            let m = Manifest::parse(INSTALL_YAML).expect("install.yaml must parse");
            // The leading comment block must not become an object.
            assert!(
                m.docs.len() >= 8,
                "expected the full install set, got {}",
                m.docs.len()
            );
            assert!(m.summaries().iter().all(|s| !s.contains("?/")));
        }

        #[test]
        fn finds_the_deployment_to_wait_on() {
            let m = Manifest::parse(INSTALL_YAML).unwrap();
            assert_eq!(
                m.deployment(),
                Some(("veloxsearch-system".into(), "veloxsearch".into()))
            );
        }

        #[test]
        fn finds_the_service_and_port() {
            let m = Manifest::parse(INSTALL_YAML).unwrap();
            assert_eq!(
                m.service(),
                Some(("veloxsearch-system".into(), "veloxsearch".into(), 80))
            );
        }

        #[test]
        fn next_steps_uses_the_real_service_coordinates() {
            let s = Manifest::parse(INSTALL_YAML).unwrap().next_steps();
            assert!(
                s.contains("kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80"),
                "missing port-forward line:\n{s}"
            );
            assert!(
                s.contains("http://localhost:3000"),
                "missing setup URL:\n{s}"
            );
        }

        #[test]
        fn next_steps_renderer_is_pure() {
            let s = next_steps("ns-x", "svc-y", 8080, 9000);
            assert!(s.contains("kubectl -n ns-x port-forward svc/svc-y 9000:8080"));
            assert!(s.contains("http://localhost:9000"));
        }

        #[test]
        fn empty_manifest_is_an_error() {
            assert!(Manifest::parse("# just a comment\n").is_err());
        }

        fn argv(s: &[&str]) -> Vec<String> {
            s.iter().map(|x| x.to_string()).collect()
        }

        #[test]
        fn base64_matches_known_vectors() {
            // RFC 4648 test vectors + the docker-auth shape.
            assert_eq!(base64_std(b""), "");
            assert_eq!(base64_std(b"f"), "Zg==");
            assert_eq!(base64_std(b"fo"), "Zm8=");
            assert_eq!(base64_std(b"foo"), "Zm9v");
            assert_eq!(base64_std(b"foob"), "Zm9vYg==");
            assert_eq!(base64_std(b"fooba"), "Zm9vYmE=");
            assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
            assert_eq!(base64_std(b"user:tok"), "dXNlcjp0b2s=");
        }

        #[test]
        fn no_pull_token_means_no_secret() {
            let opts = parse_init_opts(&argv(&[])).unwrap().unwrap();
            assert!(opts.pull_token.is_none());
            assert_eq!(opts.registry, "docker.io");
            assert_eq!(opts.pull_user, "veloxsearch");
            assert_eq!(opts.pull_secret_name, "velox-pull");
        }

        #[test]
        fn parses_pull_flags_both_forms() {
            let opts = parse_init_opts(&argv(&[
                "--registry",
                "registry.example.com",
                "--pull-user=deploy-bot",
                "--pull-token",
                "s3cr3t",
                "--pull-secret-name=my-pull",
            ]))
            .unwrap()
            .unwrap();
            assert_eq!(opts.registry, "registry.example.com");
            assert_eq!(opts.pull_user, "deploy-bot");
            assert_eq!(opts.pull_token.as_deref(), Some("s3cr3t"));
            assert_eq!(opts.pull_secret_name, "my-pull");
        }

        #[test]
        fn flag_without_value_is_an_error() {
            assert!(parse_init_opts(&argv(&["--pull-token"])).is_err());
        }

        #[test]
        fn help_flag_returns_none() {
            assert!(parse_init_opts(&argv(&["--help"])).unwrap().is_none());
        }

        #[test]
        fn pull_secret_object_is_well_formed() {
            let s = pull_secret_object("registry.example.com", "u", "t", "velox-pull");
            assert_eq!(s["kind"], "Secret");
            assert_eq!(s["type"], "kubernetes.io/dockerconfigjson");
            assert_eq!(s["metadata"]["namespace"], "veloxsearch-system");
            assert_eq!(s["metadata"]["name"], "velox-pull");
            let dockercfg: serde_json::Value =
                serde_json::from_str(s["stringData"][".dockerconfigjson"].as_str().unwrap())
                    .unwrap();
            let entry = &dockercfg["auths"]["registry.example.com"];
            assert_eq!(entry["username"], "u");
            assert_eq!(entry["password"], "t");
            assert_eq!(entry["auth"], base64_std(b"u:t"));
        }

        #[test]
        fn pull_secret_bundle_has_namespace_then_secret() {
            let b = pull_secret_bundle("r", "u", "t", "velox-pull").unwrap();
            let docs: Vec<serde_json::Value> = serde_yaml::Deserializer::from_str(&b)
                .map(|d| serde::Deserialize::deserialize(d).unwrap())
                .collect();
            assert_eq!(docs.len(), 2);
            assert_eq!(docs[0]["kind"], "Namespace");
            assert_eq!(docs[0]["metadata"]["name"], "veloxsearch-system");
            assert_eq!(docs[1]["kind"], "Secret");
        }
    }
}
