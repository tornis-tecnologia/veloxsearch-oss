// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-039 runtime **catalog client** (#75): fetch the registry's
//! `catalog.json` over HTTPS, cache it staleness-tolerantly, and install a
//! package by id — download → **verify its ed25519 signature** → hand the
//! verified bytes to the fixed apply-engine in [`crate::integrations`].
//!
//! The three layers, in order:
//!
//! 1. **Transport** ([`Registry`]) — where packages come from. Configurable by
//!    env (`VELOX_REGISTRY_URL`), `https://` **or** `file://`, because the
//!    registry repo may be private (bearer token via `VELOX_REGISTRY_TOKEN`)
//!    and because ADR-039 keeps the format transport-agnostic: an air-gapped
//!    buyer points the same client at a local checkout.
//! 2. **Trust** ([`verify_package`]) — `docs/integrations/signing.md` followed literally:
//!    canonical package digest (JCS over a sorted `[relpath, sha256]` list) and
//!    an ed25519 signature checked against a keyring **compiled into the core**
//!    (`keys/velox-registry-2026.pub`, vendored here). Fail closed: unsigned,
//!    unknown-key and tampered are three distinct hard rejects, and nothing is
//!    applied to a customer's cluster before this passes.
//! 3. **State** — installed id+version per deployment, recorded next to the
//!    existing `monitors` annotation (`crate::k8s::set_integration_version`), so
//!    *installed / available / update-available* is computable for the
//!    Integrations tab (#76).
//!
//! **Degraded registry is a state, not a crash** (ADR-047, the resolution of
//! ADR-039's open question): an unreachable or unauthorized registry yields the
//! last cached catalog marked `stale`, or — if nothing was ever cached — the
//! embedded [`bootstrap_catalog`] floor, always with a 200 and an `error`
//! string the UI can show. The deployment screen never breaks because the
//! registry is down.

use crate::scope::Deployment;
use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where the catalog and packages are read from when `VELOX_REGISTRY_URL` is
/// unset — raw files off the registry repo's default branch. Deliberately the
/// plain raw path and not the Packages API: it is the one URL shape that works
/// identically for a public repo, a private repo with a token, and a mirror.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/tornis-tecnologia/veloxsearch-registry/main";

/// How long a fetched catalog is served without re-asking the registry.
const DEFAULT_TTL: Duration = Duration::from_secs(900);

/// Per-request timeout for registry reads — short, because every caller is a
/// UI request and a hung registry must degrade, not block the tab.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The trusted keyring, compiled in (signing.md §2 option A: verification needs
/// no network, so it works in a fully egress-less install). Rotating or adding
/// a key is a core release — the same review gate everything cluster-touching
/// already goes through.
const KEYRING: &[(&str, &str)] = &[(
    "velox-registry-2026",
    include_str!("../keys/velox-registry-2026.pub"),
)];

/// The `#74` staging placeholder (bytes `0x00..0x3f`, base64). The registry's
/// own tooling treats it as UNSIGNED; so do we, with a message that says so
/// rather than the generic "bad signature".
const PLACEHOLDER_SIG_PREFIX: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

/// Bootstrap floor (ADR-047): the ids a brand-new, **egress-less** install can
/// still see and install, served from the in-binary recipe catalog. Exactly the
/// baseline monitor — the integration ADR-018 turns on for every deployment
/// anyway — so the core stays honest about the rule that "integrations must not
/// ship pre-loaded": the shelf is empty except the floor the product cannot
/// function without.
pub const BOOTSTRAP_IDS: &[&str] = &["kubernetes"];

/// Version the bootstrap entries claim. The in-binary assets are held
/// byte-equivalent to this version of the registry packages by the
/// `registry_golden` gates, so the claim is enforced, not asserted. A reachable
/// registry always overrides these entries with its own.
pub const BOOTSTRAP_VERSION: &str = "1.0.0";

/// The core version registry packages declare their floor against
/// (`min_core_version`). Read from the crate version so a release bump moves it.
fn core_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------------------
// Catalog documents
// ---------------------------------------------------------------------------

/// The registry's generated `catalog.json` — id → latest version, i18n title
/// and summary, and the core floor (ADR-039 "Versioning").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub schema_version: String,
    pub integrations: Vec<CatalogEntry>,
}

/// One catalog row as the registry publishes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub version: String,
    /// Locale → title (`en`/`es`/`pt` today).
    #[serde(default)]
    pub title: BTreeMap<String, String>,
    /// Locale → one-line summary.
    #[serde(default)]
    pub summary: BTreeMap<String, String>,
    #[serde(default)]
    pub min_core_version: String,
}

/// Where the catalog the UI is looking at came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogSource {
    /// Freshly fetched from the registry.
    Registry,
    /// Served from cache because the registry could not be reached now.
    Cache,
    /// The embedded floor — the registry has never been reached.
    Bootstrap,
}

/// What `GET`-ing the catalog returns to the Integrations tab. Always a 200:
/// `source`/`stale`/`error` describe the degradation instead of failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogView {
    pub schema_version: String,
    pub integrations: Vec<CatalogItem>,
    pub source: CatalogSource,
    /// Seconds since the catalog was last fetched from the registry
    /// (`None` = never fetched).
    pub age_seconds: Option<u64>,
    /// The registry could not be reached on this request.
    pub stale: bool,
    /// Why, verbatim, when `stale` — for the UI to show, not to parse.
    pub error: Option<String>,
}

/// One catalog row, enriched with everything the tab needs to render a state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    pub version: String,
    pub title: BTreeMap<String, String>,
    pub summary: BTreeMap<String, String>,
    pub min_core_version: String,
    /// This core satisfies `min_core_version`. `false` ⇒ install refuses.
    pub compatible: bool,
    /// Installable with no egress at all (the ADR-047 bootstrap floor).
    pub builtin: bool,
    /// Version recorded on the deployment, if installed (`None` = available).
    pub installed_version: Option<String>,
    /// Installed, but the catalog offers a newer version.
    pub update_available: bool,
}

/// The embedded bootstrap catalog (ADR-047). Kept as data here rather than
/// read from a file so it survives in a scratch container with no assets.
pub fn bootstrap_catalog() -> Catalog {
    let entries = BOOTSTRAP_IDS
        .iter()
        .filter_map(|id| bootstrap_entry(id))
        .collect();
    Catalog {
        schema_version: "1.0".into(),
        integrations: entries,
    }
}

/// The embedded row for one bootstrap id. Strings mirror the registry
/// package's `manifest.yaml`; the assets behind them are the in-binary recipe,
/// held byte-equivalent by the `registry_golden` gates.
fn bootstrap_entry(id: &str) -> Option<CatalogEntry> {
    // (locale, title, summary) — verbatim from the package's manifest.yaml.
    let rows: &[(&str, &str, &str)] = match id {
        "kubernetes" => &[
            (
                "en",
                "Kubernetes / K3S",
                "All cluster and pod logs from every node — the cluster monitoring itself.",
            ),
            (
                "es",
                "Kubernetes / K3S",
                "Todos los registros del clúster y de los pods desde cada nodo — el clúster monitoreándose a sí mismo.",
            ),
            (
                "pt",
                "Kubernetes / K3S",
                "Todos os logs do cluster e dos pods de cada nó — o cluster monitorando a si mesmo.",
            ),
        ],
        _ => return None,
    };
    Some(CatalogEntry {
        id: id.to_string(),
        version: BOOTSTRAP_VERSION.to_string(),
        title: rows
            .iter()
            .map(|(l, t, _)| (l.to_string(), t.to_string()))
            .collect(),
        summary: rows
            .iter()
            .map(|(l, _, s)| (l.to_string(), s.to_string()))
            .collect(),
        min_core_version: core_version().to_string(),
    })
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// A registry endpoint: a base URL plus optional bearer credentials. Cheap to
/// build; every method is independent so tests construct their own instead of
/// mutating process env.
#[derive(Debug, Clone)]
pub struct Registry {
    base: String,
    token: Option<String>,
}

impl Registry {
    /// Point at an explicit base (no trailing slash required).
    pub fn new(base: impl Into<String>, token: Option<String>) -> Registry {
        Registry {
            base: base.into().trim_end_matches('/').to_string(),
            token,
        }
    }

    /// The process-wide registry: `VELOX_REGISTRY_URL` (default
    /// [`DEFAULT_REGISTRY_URL`]) + `VELOX_REGISTRY_TOKEN`. A `file://` base is
    /// the offline/air-gapped delivery route ADR-039 keeps open — same format,
    /// same signature check, no wire.
    pub fn from_env() -> Registry {
        let base = std::env::var("VELOX_REGISTRY_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());
        let token = std::env::var("VELOX_REGISTRY_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Registry::new(base, token)
    }

    /// Fetch one file, relative to the base. `rel` is always built from
    /// validated components (see [`valid_component`]) — never from raw input.
    async fn get(&self, rel: &str) -> Result<Vec<u8>> {
        if let Some(root) = self.base.strip_prefix("file://") {
            let path = std::path::Path::new(root).join(rel);
            return std::fs::read(&path)
                .with_context(|| format!("reading {} from the local registry", path.display()));
        }
        let url = format!("{}/{rel}", self.base);
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building the registry http client")?;
        let mut req = client.get(&url);
        if let Some(t) = &self.token {
            // GitLab accepts either; PRIVATE-TOKEN covers PATs and project
            // access tokens, Bearer covers OAuth/CI job tokens on other forges.
            req = req.header("PRIVATE-TOKEN", t).bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        Ok(resp
            .bytes()
            .await
            .with_context(|| format!("body of {url}"))?
            .to_vec())
    }

    /// Fetch and parse `catalog.json`.
    pub async fn fetch_catalog(&self) -> Result<Catalog> {
        let bytes = self.get("catalog.json").await?;
        serde_json::from_slice(&bytes).context("parsing catalog.json")
    }

    /// Fetch one package directory: `manifest.yaml` plus exactly the assets it
    /// names. The manifest is **untrusted** at this point — it has not been
    /// verified yet — so every filename it supplies is validated before it can
    /// become part of a URL or a path.
    pub async fn fetch_package(&self, id: &str) -> Result<FetchedPackage> {
        if !valid_component(id) {
            bail!("refusing to fetch integration with an unsafe id: {id:?}");
        }
        let manifest_yaml = String::from_utf8(
            self.get(&format!("integrations/{id}/manifest.yaml"))
                .await?,
        )
        .with_context(|| format!("{id}/manifest.yaml is not UTF-8"))?;
        let names = asset_filenames(&manifest_yaml)?;
        let mut assets = BTreeMap::new();
        for name in names {
            let bytes = self.get(&format!("integrations/{id}/{name}")).await?;
            assets.insert(name, bytes);
        }
        Ok(FetchedPackage {
            id: id.to_string(),
            manifest_yaml,
            assets,
        })
    }
}

/// A package as it came off the wire (or off disk): the manifest text plus each
/// named asset's raw bytes. Nothing here is trusted until [`verify_package`].
#[derive(Debug, Clone)]
pub struct FetchedPackage {
    pub id: String,
    pub manifest_yaml: String,
    /// Filename → raw bytes, **excluding** `manifest.yaml`.
    pub assets: BTreeMap<String, Vec<u8>>,
}

impl FetchedPackage {
    /// Read a package straight from a directory — the offline route, and what
    /// the tests use against a registry checkout.
    pub fn load_from_dir(dir: &std::path::Path) -> Result<FetchedPackage> {
        let id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .context("package directory has no name")?
            .to_string();
        let manifest_yaml = std::fs::read_to_string(dir.join("manifest.yaml"))
            .with_context(|| format!("reading {}/manifest.yaml", dir.display()))?;
        let mut assets = BTreeMap::new();
        for name in asset_filenames(&manifest_yaml)? {
            let bytes = std::fs::read(dir.join(&name))
                .with_context(|| format!("reading asset {name:?}"))?;
            assets.insert(name, bytes);
        }
        Ok(FetchedPackage {
            id,
            manifest_yaml,
            assets,
        })
    }

    /// Turn a **verified** package into the engine's [`crate::integrations::Package`].
    pub fn into_package(self) -> Result<crate::integrations::Package> {
        let manifest = crate::integrations::parse_manifest(&self.manifest_yaml)?;
        let text = |name: &str| -> Result<String> {
            let raw = self
                .assets
                .get(name)
                .with_context(|| format!("asset {name:?} was not fetched"))?;
            String::from_utf8(raw.clone()).with_context(|| format!("asset {name:?} is not UTF-8"))
        };
        let pipeline = manifest.assets.pipeline.as_deref().map(text).transpose()?;
        let index_template = manifest
            .assets
            .index_template
            .as_deref()
            .map(text)
            .transpose()?;
        let saved_objects = text(&manifest.assets.saved_objects)?;
        let agent_config = text(&manifest.assets.agent_config)?;
        Ok(crate::integrations::Package {
            manifest,
            pipeline,
            index_template,
            saved_objects,
            agent_config,
        })
    }
}

/// One path component that is safe to concatenate into a URL or a filesystem
/// path: lower-case ids and asset filenames only, no separators, no `..`.
fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s != "."
        && s != ".."
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// The asset filenames a manifest names, validated. The registry's `validate`
/// refuses any file in a package directory that the manifest does not name, so
/// this list is exactly the directory contents minus `manifest.yaml` — which is
/// what makes the digest computed here identical to the signer's.
fn asset_filenames(manifest_yaml: &str) -> Result<Vec<String>> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(manifest_yaml).context("parsing manifest.yaml")?;
    let assets = doc
        .get("assets")
        .and_then(|a| a.as_mapping())
        .context("manifest.yaml has no assets block")?;
    let mut out = Vec::new();
    for (_role, v) in assets {
        let name = v.as_str().context("assets values must be filenames")?;
        if !valid_component(name) {
            bail!("manifest names an unsafe asset filename: {name:?}");
        }
        out.push(name.to_string());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Trust — canonical digest + ed25519 verification (docs/integrations/signing.md)
// ---------------------------------------------------------------------------

/// RFC 8785 (JCS) serialization of the manifest subset that occurs in
/// practice. Floats are a hard error rather than a guess: they do not occur in
/// manifests, and JCS's float form is the one part of the spec worth not
/// implementing from memory (the registry's Python signer refuses them too, so
/// the two sides stay bit-identical by construction).
fn jcs(v: &serde_yaml::Value) -> Result<String> {
    Ok(match v {
        serde_yaml::Value::Null => "null".to_string(),
        serde_yaml::Value::Bool(true) => "true".to_string(),
        serde_yaml::Value::Bool(false) => "false".to_string(),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                bail!("manifest contains a floating-point value; JCS float form is not implemented")
            }
        }
        serde_yaml::Value::String(s) => json_string(s),
        serde_yaml::Value::Sequence(items) => {
            let parts: Result<Vec<String>> = items.iter().map(jcs).collect();
            format!("[{}]", parts?.join(","))
        }
        serde_yaml::Value::Mapping(m) => {
            let mut pairs: Vec<(String, &serde_yaml::Value)> = Vec::with_capacity(m.len());
            for (k, val) in m {
                let key = k.as_str().context("manifest keys must be strings")?;
                pairs.push((key.to_string(), val));
            }
            // JCS orders by UTF-16 code units.
            pairs.sort_by_key(|(k, _)| k.encode_utf16().collect::<Vec<u16>>());
            let mut parts = Vec::with_capacity(pairs.len());
            for (k, val) in pairs {
                parts.push(format!("{}:{}", json_string(&k), jcs(val)?));
            }
            format!("{{{}}}", parts.join(","))
        }
        other => bail!("unsupported YAML node in manifest: {other:?}"),
    })
}

/// A JSON string literal — the escaping half of JCS.
fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// The canonical package digest — the 32 bytes that get signed
/// (`docs/integrations/signing.md` §1): the manifest hashed **without** its `signature` key
/// and JCS-canonicalized, every other file hashed raw, the
/// `[relpath, "sha256:…"]` entries sorted bytewise and JCS-serialized, then
/// SHA-256 of that. Independent of YAML formatting and file order, so the
/// online and offline delivery routes produce the same bytes.
pub fn package_digest(manifest_yaml: &str, assets: &BTreeMap<String, Vec<u8>>) -> Result<[u8; 32]> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(manifest_yaml).context("parsing manifest.yaml")?;
    let map = doc
        .as_mapping_mut()
        .context("manifest.yaml is not a mapping")?;
    map.remove(serde_yaml::Value::String("signature".into()));
    let manifest_hash = sha256_hex(jcs(&doc)?.as_bytes());

    // BTreeMap<String, _> already iterates bytewise-ascending by name, and
    // "manifest.yaml" is inserted into the same ordering rather than prepended.
    let mut entries: BTreeMap<&str, String> = BTreeMap::new();
    entries.insert("manifest.yaml", format!("sha256:{manifest_hash}"));
    for (name, bytes) in assets {
        entries.insert(name.as_str(), format!("sha256:{}", sha256_hex(bytes)));
    }
    let list = entries
        .iter()
        .map(|(name, digest)| format!("[{},{}]", json_string(name), json_string(digest)))
        .collect::<Vec<_>>()
        .join(",");
    let digest = Sha256::digest(format!("[{list}]").as_bytes());
    Ok(digest.into())
}

/// The signature block a manifest carries (`signature:` in `manifest.yaml`).
#[derive(Debug, Clone, Deserialize)]
struct SignatureBlock {
    algorithm: String,
    key_id: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SignedManifest {
    signature: Option<SignatureBlock>,
}

/// Verify a fetched package against the compiled-in keyring, **fail closed**
/// (`docs/integrations/signing.md` §3). The three named rejects are distinguishable in the
/// error text: *unsigned* (no signature block / placeholder), *unknown key*
/// (`key_id` not in the keyring), *tampered* (digest mismatch).
///
/// This runs before a single byte reaches a customer's cluster — packages are
/// data, so this check **is** the security boundary.
pub fn verify_package(manifest_yaml: &str, assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let parsed: SignedManifest =
        serde_yaml::from_str(manifest_yaml).context("parsing manifest.yaml")?;
    let sig = parsed
        .signature
        .context("package is UNSIGNED: manifest.yaml carries no signature block")?;
    if sig.algorithm != "ed25519" {
        bail!(
            "package signature algorithm {:?} is not supported (only ed25519)",
            sig.algorithm
        );
    }
    if sig.value.starts_with(PLACEHOLDER_SIG_PREFIX) {
        bail!("package is UNSIGNED: manifest.yaml carries the staging placeholder signature");
    }
    let key_b64 = KEYRING
        .iter()
        .find(|(id, _)| *id == sig.key_id)
        .map(|(_, k)| k.trim())
        .with_context(|| {
            format!(
                "package signed by UNKNOWN KEY {:?} (trusted: {})",
                sig.key_id,
                KEYRING
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key_b64)
        .context("compiled-in public key is not valid base64")?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(sig.value.trim())
        .context("signature value is not valid base64")?;

    let digest = package_digest(manifest_yaml, assets)?;
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, &key)
        .verify(&digest, &signature)
        .map_err(|_| {
            anyhow::anyhow!(
                "package signature does not match its contents — TAMPERED or mis-signed \
                 (key_id {:?})",
                sig.key_id
            )
        })
}

/// Sign a package with the registry's private key, returning the base64 value
/// the manifest's `signature.value` carries.
///
/// Deliberately adjacent to [`verify_package`]: the one property that must hold
/// is that both sides derive the same bytes from [`package_digest`], and
/// keeping them in the same screenful is what makes a drift between them
/// visible in review rather than at install time.
///
/// The key is PKCS#8 PEM — what `openssl genpkey -algorithm ed25519` emits, and
/// what `keys/README.md` documents. `aws-lc-rs` is already the process-wide
/// rustls provider, so this adds no crypto stack (and `ring` stays banned; see
/// `deny.toml`).
///
/// This function does NOT touch the manifest. The caller writes the returned
/// value back and re-verifies — see `velox sign`, which refuses to write a
/// signature that does not verify.
pub fn sign_package(
    manifest_yaml: &str,
    assets: &BTreeMap<String, Vec<u8>>,
    key_pem: &str,
) -> Result<String> {
    use rustls_pki_types::pem::PemObject;

    let key = rustls_pki_types::PrivatePkcs8KeyDer::from_pem_slice(key_pem.as_bytes())
        .context("the signing key is not a PKCS#8 PEM private key")?;
    let pair = aws_lc_rs::signature::Ed25519KeyPair::from_pkcs8(key.secret_pkcs8_der())
        .map_err(|_| anyhow::anyhow!("the signing key is not a usable ed25519 key"))?;

    let digest = package_digest(manifest_yaml, assets)?;
    let sig = pair.sign(&digest);
    Ok(base64::engine::general_purpose::STANDARD.encode(sig.as_ref()))
}

/// Replace a manifest's `signature.value` with `value`, in place, **by line**.
///
/// Not a YAML round-trip, on purpose. Manifests carry hand-written comments
/// explaining every field, and re-serializing would erase them — and the
/// manifest bytes are themselves an input to [`package_digest`], so rewriting
/// the file wholesale would invalidate the very signature being written.
///
/// Safe by construction: `package_digest` removes the `signature` key before
/// hashing, so changing `value` cannot change the digest.
pub fn replace_signature_value(manifest_yaml: &str, value: &str) -> Result<String> {
    let mut out = String::with_capacity(manifest_yaml.len());
    let mut replaced = 0usize;
    for line in manifest_yaml.lines() {
        // The `value:` of the signature block is the only two-space-indented
        // `value:` a manifest has (the schema puts nothing else at that depth).
        if let Some(indent) = line.strip_suffix(line.trim_start()) {
            if line.trim_start().starts_with("value:") && indent == "  " {
                out.push_str(indent);
                out.push_str("value: ");
                out.push_str(value);
                out.push('\n');
                replaced += 1;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    match replaced {
        1 => Ok(out),
        0 => bail!("manifest.yaml has no `signature.value` line to replace"),
        n => bail!("manifest.yaml has {n} candidate `value:` lines; refusing to guess"),
    }
}

// ---------------------------------------------------------------------------
// Cache — staleness-tolerant, never a failure
// ---------------------------------------------------------------------------

struct Cached {
    catalog: Catalog,
    fetched: Instant,
}

/// A catalog cache. The process uses one global instance ([`view`]); tests
/// build their own so nothing is shared or env-dependent.
#[derive(Default)]
pub struct CatalogCache {
    inner: RwLock<Option<Cached>>,
}

/// The outcome of consulting the cache: a catalog plus how it was obtained.
pub struct CatalogState {
    pub catalog: Catalog,
    pub source: CatalogSource,
    pub age_seconds: Option<u64>,
    pub error: Option<String>,
}

impl CatalogCache {
    pub const fn new() -> CatalogCache {
        CatalogCache {
            inner: RwLock::new(None),
        }
    }

    /// Serve the catalog: fresh cache within `ttl`, else a fetch; on a failed
    /// fetch fall back to the stale cache, and to the embedded bootstrap floor
    /// when there is no cache at all. Never returns an error — a down registry
    /// is a *state* the UI renders, not a 500 (ADR-047).
    pub async fn load(&self, reg: &Registry, ttl: Duration) -> CatalogState {
        // Read-and-drop: the guard never spans the await below.
        if let Ok(g) = self.inner.read() {
            if let Some(c) = g.as_ref() {
                let age = c.fetched.elapsed();
                if age < ttl {
                    return CatalogState {
                        catalog: c.catalog.clone(),
                        source: CatalogSource::Registry,
                        age_seconds: Some(age.as_secs()),
                        error: None,
                    };
                }
            }
        }
        match reg.fetch_catalog().await {
            Ok(catalog) => {
                if let Ok(mut g) = self.inner.write() {
                    *g = Some(Cached {
                        catalog: catalog.clone(),
                        fetched: Instant::now(),
                    });
                }
                CatalogState {
                    catalog,
                    source: CatalogSource::Registry,
                    age_seconds: Some(0),
                    error: None,
                }
            }
            Err(e) => {
                let error = Some(format!("{e:#}"));
                if let Ok(g) = self.inner.read() {
                    if let Some(c) = g.as_ref() {
                        return CatalogState {
                            catalog: c.catalog.clone(),
                            source: CatalogSource::Cache,
                            age_seconds: Some(c.fetched.elapsed().as_secs()),
                            error,
                        };
                    }
                }
                CatalogState {
                    catalog: bootstrap_catalog(),
                    source: CatalogSource::Bootstrap,
                    age_seconds: None,
                    error,
                }
            }
        }
    }
}

static CACHE: CatalogCache = CatalogCache::new();

// ---------------------------------------------------------------------------
// The view the Integrations tab reads
// ---------------------------------------------------------------------------

/// `left >= right` on dotted numeric versions, missing components as 0. Not a
/// full semver implementation on purpose: `min_core_version` is `MAJOR.MINOR.PATCH`
/// by schema, and pre-release/build metadata has no meaning for a core floor.
fn version_ge(left: &str, right: &str) -> bool {
    let part = |s: &str, i: usize| -> u64 {
        s.split(['.', '-', '+'])
            .nth(i)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    };
    for i in 0..3 {
        let (l, r) = (part(left, i), part(right, i));
        if l != r {
            return l > r;
        }
    }
    true
}

/// Merge the fetched catalog with the bootstrap floor (a reachable registry
/// always wins for an id it publishes) and enrich each row with what this
/// deployment has installed.
fn build_view(state: CatalogState, installed: &BTreeMap<String, String>) -> CatalogView {
    let CatalogState {
        catalog,
        source,
        age_seconds,
        error,
    } = state;
    let mut entries = catalog.integrations;
    for id in BOOTSTRAP_IDS {
        if !entries.iter().any(|e| e.id == *id) {
            if let Some(e) = bootstrap_entry(id) {
                entries.push(e);
            }
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));

    let core = core_version();
    let integrations = entries
        .into_iter()
        .map(|e| {
            let installed_version = installed.get(&e.id).cloned();
            let update_available = installed_version
                .as_deref()
                .is_some_and(|v| v != e.version && version_ge(&e.version, v));
            CatalogItem {
                compatible: e.min_core_version.is_empty() || version_ge(core, &e.min_core_version),
                builtin: BOOTSTRAP_IDS.contains(&e.id.as_str()),
                installed_version,
                update_available,
                id: e.id,
                version: e.version,
                title: e.title,
                summary: e.summary,
                min_core_version: e.min_core_version,
            }
        })
        .collect();

    CatalogView {
        schema_version: catalog.schema_version,
        integrations,
        stale: error.is_some(),
        source,
        age_seconds,
        error,
    }
}

/// The Integrations tab's read (#76). `deployment` — when given and reachable —
/// adds the installed/update-available state; a K8s hiccup degrades that to
/// "nothing installed" rather than failing the whole request.
pub async fn view(deployment: Option<&Deployment>) -> CatalogView {
    let state = CACHE.load(&Registry::from_env(), DEFAULT_TTL).await;
    let installed = match deployment {
        Some(d) => crate::k8s::integration_versions(d)
            .await
            .unwrap_or_default(),
        None => BTreeMap::new(),
    };
    build_view(state, &installed)
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install an integration onto a deployment, end to end:
/// resolve in the catalog → download the package → **verify the signature** →
/// apply through the fixed engine → record `id@version` on the deployment.
///
/// `version`, when given, is an expectation: the registry serves one version
/// per id (latest on the tracked ref), so a mismatch is an honest error rather
/// than a silent downgrade.
///
/// Degraded registry (ADR-047): if the registry cannot be reached and the id is
/// in the [`BOOTSTRAP_IDS`] floor, the in-binary recipe — byte-equivalent to
/// that package by the `registry_golden` gates — is applied instead, so a
/// first, egress-less install still works. Any other id is a clear error.
pub async fn install(deployment: &Deployment, id: &str, version: Option<&str>) -> Result<()> {
    if !valid_component(id) {
        bail!("invalid integration id: {id:?}");
    }
    let reg = Registry::from_env();
    let state = CACHE.load(&reg, DEFAULT_TTL).await;
    let degraded = state.source != CatalogSource::Registry;
    let entry = state
        .catalog
        .integrations
        .iter()
        .find(|e| e.id == id)
        .cloned();

    if let Some(e) = &entry {
        if !e.min_core_version.is_empty() && !version_ge(core_version(), &e.min_core_version) {
            bail!(
                "integration {id} requires core {} or newer (this core is {})",
                e.min_core_version,
                core_version()
            );
        }
        if let Some(want) = version {
            if want != e.version {
                bail!(
                    "integration {id} version {want} is not available; the registry offers {}",
                    e.version
                );
            }
        }
    } else if !degraded {
        bail!("integration {id} is not in the registry catalog");
    }

    // What gets RECORDED is what actually gets applied — see the fallback arm.
    let mut resolved = entry
        .as_ref()
        .map(|e| e.version.clone())
        .unwrap_or_else(|| BOOTSTRAP_VERSION.to_string());

    match reg.fetch_package(id).await {
        Ok(fetched) => {
            verify_package(&fetched.manifest_yaml, &fetched.assets)
                .with_context(|| format!("refusing to install integration {id}"))?;
            let pkg = fetched.into_package()?;
            if pkg.manifest.id != id {
                bail!(
                    "package identity mismatch: asked for {id}, manifest says {}",
                    pkg.manifest.id
                );
            }
            crate::integrations::apply_package(deployment, &pkg).await?;
        }
        Err(e) if BOOTSTRAP_IDS.contains(&id) => {
            tracing::warn!(
                "registry unreachable ({e:#}); installing bootstrap integration {id} from the \
                 in-binary catalog"
            );
            crate::recipes::apply(deployment, id).await?;
            // The in-binary assets ARE the bootstrap version, whatever a warm
            // cache says the registry offers. Recording the catalog's version
            // here would claim an install that did not happen; recording what
            // was applied instead makes the row correctly read
            // "update-available" as soon as the registry is reachable again.
            resolved = BOOTSTRAP_VERSION.to_string();
        }
        Err(e) => return Err(e).with_context(|| format!("downloading integration {id}")),
    }

    // Record the install exactly as the recipe path does, plus the version, so
    // installed / available / update-available is computable (ADR-039).
    crate::k8s::set_monitor(deployment, id, true).await?;
    crate::k8s::set_integration_version(deployment, id, Some(&resolved)).await?;
    Ok(())
}

/// Uninstall an integration from a deployment (#76), the mirror of [`install`]:
/// fetch the package → **verify the signature** → tear down exactly the
/// manifest's `teardown` set through the engine → clear the monitor and the
/// recorded version.
///
/// The teardown set must come from the package, not be guessed: `recipes`'
/// per-id lookups fall back to the nginx recipe for an unknown id, so routing a
/// registry-only integration through `recipes::disable` would delete ANOTHER
/// integration's pipeline, template and dashboards.
///
/// Degraded registry: an id the core carries in-binary falls back to
/// `recipes::disable`, whose teardown is that recipe's own definition — so an
/// egress-less core can still undo what it installed. Any other id fails
/// loudly, leaving the deployment untouched, rather than deleting a guess.
pub async fn uninstall(deployment: &Deployment, id: &str) -> Result<()> {
    if !valid_component(id) {
        bail!("invalid integration id: {id:?}");
    }
    let reg = Registry::from_env();
    match reg.fetch_package(id).await {
        Ok(fetched) => {
            // Same trust boundary as install: the `teardown` block is a list of
            // objects to DELETE, so an unverified manifest is not something to
            // act on.
            verify_package(&fetched.manifest_yaml, &fetched.assets)
                .with_context(|| format!("refusing to uninstall integration {id}"))?;
            let pkg = fetched.into_package()?;
            crate::integrations::uninstall_package(deployment, &pkg).await?;
        }
        Err(e) if crate::recipes::RECIPES.contains(&id) => {
            tracing::warn!(
                "registry unreachable ({e:#}); tearing down integration {id} from the in-binary \
                 recipe"
            );
            crate::recipes::disable(deployment, id).await?;
        }
        Err(e) => return Err(e).with_context(|| format!("downloading integration {id}")),
    }

    // Drops the monitor AND the recorded version in one patch, so an
    // uninstalled integration is never reported back as installed.
    crate::k8s::set_monitor(deployment, id, false).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API request/response bodies (handlers live in `api.rs`)
// ---------------------------------------------------------------------------

/// `POST /api/catalog` — optionally scoped to a deployment for installed state.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogReq {
    #[serde(default)]
    pub deployment: Option<String>,
}

/// `POST /api/catalog_install`.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogInstallReq {
    pub deployment: String,
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// `POST /api/catalog_uninstall` (#76).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogUninstallReq {
    pub deployment: String,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry checkout to test the real packages against, or `None` (the
    /// same loud skip the `registry_golden` gates use). One shared decision so
    /// the catalog gates cannot skip on a lane where the golden gates fail:
    /// unset + CI is a hard failure, unset locally is a skip (#108).
    fn checkout() -> Option<std::path::PathBuf> {
        crate::registry_golden::registry_root()
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    // ── canonicalization ────────────────────────────────────────────────

    /// JCS orders keys and escapes strings the way the registry's Python
    /// signer does — the two implementations must agree byte for byte or no
    /// signature ever verifies.
    #[test]
    fn jcs_sorts_keys_and_escapes_strings() {
        let doc: serde_yaml::Value =
            serde_yaml::from_str("b: 2\na: \"x\\\"y\"\nc:\n  - 1\n  - true\n  - null\nA: z\n")
                .unwrap();
        assert_eq!(
            jcs(&doc).unwrap(),
            r#"{"A":"z","a":"x\"y","b":2,"c":[1,true,null]}"#
        );
    }

    /// Floats are refused rather than guessed (the Python signer refuses too).
    #[test]
    fn jcs_refuses_floats() {
        let doc: serde_yaml::Value = serde_yaml::from_str("a: 1.5\n").unwrap();
        assert!(jcs(&doc)
            .unwrap_err()
            .to_string()
            .contains("floating-point"));
    }

    /// The digest is over *content*, not layout: reordering the manifest's
    /// keys and re-serializing it must not change a single byte
    /// (signing.md §1 "order-independent", test obligation §3).
    #[test]
    fn digest_is_order_independent() {
        let a = "id: nginx\nversion: 1.0.0\nassets:\n  saved_objects: s.ndjson\n  agent_config: a.tmpl\nsignature:\n  algorithm: ed25519\n  key_id: k\n  value: v\n";
        let b = "signature:\n  value: v\n  key_id: k\n  algorithm: ed25519\nassets:\n  agent_config: a.tmpl\n  saved_objects: s.ndjson\nversion: 1.0.0\nid: nginx\n";
        let assets: BTreeMap<String, Vec<u8>> = [("s.ndjson".to_string(), b"{}\n".to_vec())]
            .into_iter()
            .collect();
        assert_eq!(
            package_digest(a, &assets).unwrap(),
            package_digest(b, &assets).unwrap()
        );
    }

    /// Changing the signature block alone does NOT change the digest (it is
    /// stripped before hashing — what lets the signature live in the file it
    /// protects); changing any asset byte DOES.
    #[test]
    fn digest_strips_signature_and_covers_assets() {
        let base = "id: x\nassets:\n  saved_objects: s.ndjson\n";
        let signed =
            format!("{base}signature:\n  algorithm: ed25519\n  key_id: k\n  value: AAAA\n");
        let assets: BTreeMap<String, Vec<u8>> = [("s.ndjson".to_string(), b"one".to_vec())]
            .into_iter()
            .collect();
        let flipped: BTreeMap<String, Vec<u8>> = [("s.ndjson".to_string(), b"two".to_vec())]
            .into_iter()
            .collect();
        assert_eq!(
            package_digest(base, &assets).unwrap(),
            package_digest(&signed, &assets).unwrap()
        );
        assert_ne!(
            package_digest(base, &assets).unwrap(),
            package_digest(base, &flipped).unwrap()
        );
    }

    // ── signature verification against the REAL registry packages ───────

    /// Every shipped package verifies against the compiled-in keyring. This is
    /// the whole security boundary, checked against production bytes rather
    /// than a fixture we signed ourselves.
    #[test]
    fn every_registry_package_verifies() {
        let Some(root) = checkout() else { return };
        let mut n = 0;
        for e in std::fs::read_dir(root.join("integrations")).unwrap() {
            let dir = e.unwrap().path();
            if !dir.is_dir() {
                continue;
            }
            let pkg = FetchedPackage::load_from_dir(&dir).expect("load package");
            verify_package(&pkg.manifest_yaml, &pkg.assets)
                .unwrap_or_else(|err| panic!("{}: {err:#}", pkg.id));
            n += 1;
        }
        assert!(n >= 12, "expected the 12 shipped packages, found {n}");
    }

    /// Flipping ONE byte in ONE asset is rejected (ADR-039's "signature
    /// verification rejects a tampered package").
    #[test]
    fn tampered_asset_is_rejected() {
        let Some(root) = checkout() else { return };
        let mut pkg = FetchedPackage::load_from_dir(&root.join("integrations/nginx")).unwrap();
        let name = pkg.assets.keys().next().unwrap().clone();
        pkg.assets.get_mut(&name).unwrap().push(b' ');
        let err = verify_package(&pkg.manifest_yaml, &pkg.assets).unwrap_err();
        assert!(err.to_string().contains("TAMPERED"), "unhelpful: {err:#}");
    }

    /// Editing a *signed manifest field* is rejected too (the manifest is part
    /// of the digest, minus only its own signature block).
    #[test]
    fn tampered_manifest_is_rejected() {
        let Some(root) = checkout() else { return };
        let pkg = FetchedPackage::load_from_dir(&root.join("integrations/nginx")).unwrap();
        let edited = pkg
            .manifest_yaml
            .replace("index: nginx-logs", "index: evil-logs");
        assert_ne!(edited, pkg.manifest_yaml, "the replace must have applied");
        let err = verify_package(&edited, &pkg.assets).unwrap_err();
        assert!(err.to_string().contains("TAMPERED"), "unhelpful: {err:#}");
    }

    /// Re-serializing the manifest through the YAML parser (key order and
    /// formatting destroyed) still verifies — proves the canonicalization is
    /// what is signed, not the file bytes.
    #[test]
    fn reserialized_manifest_still_verifies() {
        let Some(root) = checkout() else { return };
        let pkg = FetchedPackage::load_from_dir(&root.join("integrations/redis")).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&pkg.manifest_yaml).unwrap();
        let round_tripped = serde_yaml::to_string(&doc).unwrap();
        assert_ne!(round_tripped, pkg.manifest_yaml, "round trip must reformat");
        verify_package(&round_tripped, &pkg.assets).expect("canonicalization is order-independent");
    }

    /// A package with no signature block is UNSIGNED, and says so.
    #[test]
    fn unsigned_package_is_rejected() {
        let err = verify_package("id: x\nassets: {}\n", &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("UNSIGNED"), "unhelpful: {err:#}");
    }

    /// The #74 staging placeholder is UNSIGNED, not "bad signature".
    #[test]
    fn placeholder_signature_is_unsigned() {
        let value =
            base64::engine::general_purpose::STANDARD.encode((0u8..64).collect::<Vec<u8>>());
        let yaml = format!(
            "id: x\nassets: {{}}\nsignature:\n  algorithm: ed25519\n  key_id: velox-registry-2026\n  value: {value}\n"
        );
        let err = verify_package(&yaml, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("UNSIGNED"), "unhelpful: {err:#}");
    }

    /// A signature from a key the core does not trust is refused by key_id —
    /// before any crypto runs.
    #[test]
    fn unknown_key_id_is_rejected() {
        let Some(root) = checkout() else { return };
        let pkg = FetchedPackage::load_from_dir(&root.join("integrations/nginx")).unwrap();
        let edited = pkg
            .manifest_yaml
            .replace("key_id: velox-registry-2026", "key_id: attacker-2026");
        let err = verify_package(&edited, &pkg.assets).unwrap_err();
        assert!(
            err.to_string().contains("UNKNOWN KEY"),
            "unhelpful: {err:#}"
        );
    }

    /// The vendored public key is the registry's, byte for byte.
    #[test]
    fn keyring_matches_the_registry_key() {
        let Some(root) = checkout() else { return };
        let on_disk = std::fs::read_to_string(root.join("keys/velox-registry-2026.pub")).unwrap();
        let compiled = KEYRING
            .iter()
            .find(|(id, _)| *id == "velox-registry-2026")
            .map(|(_, k)| *k)
            .unwrap();
        assert_eq!(on_disk.trim(), compiled.trim(), "vendored key drifted");
    }

    // ── signing ─────────────────────────────────────────────────────────

    /// A throwaway key pair, generated per test so nothing here depends on the
    /// real signing key ever being present. Returns (PKCS#8 PEM, public base64
    /// in the form `keys/*.pub` carries).
    fn throwaway_key() -> (String, String) {
        let rng = aws_lc_rs::rand::SystemRandom::new();
        let pkcs8 = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
        );
        let pair = aws_lc_rs::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = base64::engine::general_purpose::STANDARD
            .encode(aws_lc_rs::signature::KeyPair::public_key(&pair).as_ref());
        (pem, public)
    }

    /// A tiny package, so the signing tests need no registry checkout.
    fn toy_package(sig_value: &str) -> (String, BTreeMap<String, Vec<u8>>) {
        let manifest = format!(
            "# a comment that a YAML round-trip would destroy\n\
             schema_version: \"1.0\"\n\
             id: toy\n\
             signature:\n  \
             algorithm: ed25519\n  \
             key_id: velox-registry-2026\n  \
             value: {sig_value}\n"
        );
        let mut assets = BTreeMap::new();
        assets.insert("a.json".to_string(), b"{}".to_vec());
        (manifest, assets)
    }

    /// Sign then verify: the round trip closes with the real keyring path.
    #[test]
    fn a_signature_this_code_produces_is_one_this_code_accepts() {
        let (key_pem, public_b64) = throwaway_key();
        let (manifest, assets) = toy_package("PLACEHOLDER");
        let value = sign_package(&manifest, &assets, &key_pem).unwrap();
        let signed = replace_signature_value(&manifest, &value).unwrap();

        // verify_package checks the compiled-in keyring, which does not hold a
        // throwaway key — so assert the crypto directly, the same way it does.
        let digest = package_digest(&signed, &assets).unwrap();
        let key = base64::engine::general_purpose::STANDARD
            .decode(&public_b64)
            .unwrap();
        aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, &key)
            .verify(
                &digest,
                &base64::engine::general_purpose::STANDARD
                    .decode(&value)
                    .unwrap(),
            )
            .expect("a freshly signed package must verify");
    }

    /// Writing the signature does not change what was signed. This is the
    /// property that lets `velox sign` rewrite the file it just hashed.
    #[test]
    fn writing_the_signature_does_not_change_the_digest() {
        let (manifest, assets) = toy_package("PLACEHOLDER");
        let before = package_digest(&manifest, &assets).unwrap();
        let rewritten = replace_signature_value(&manifest, "SOMETHINGELSE").unwrap();
        let after = package_digest(&rewritten, &assets).unwrap();
        assert_eq!(before, after, "the digest must ignore the signature block");
    }

    /// The comments are the documentation; a signer that eats them is a signer
    /// nobody will use twice.
    #[test]
    fn writing_the_signature_preserves_comments_and_every_other_line() {
        let (manifest, _) = toy_package("PLACEHOLDER");
        let rewritten = replace_signature_value(&manifest, "NEW").unwrap();
        assert!(rewritten.contains("# a comment that a YAML round-trip would destroy"));
        assert!(rewritten.contains("key_id: velox-registry-2026"));
        assert!(rewritten.contains("  value: NEW"));
        assert!(!rewritten.contains("PLACEHOLDER"));
        assert_eq!(manifest.lines().count(), rewritten.lines().count());
    }

    /// Refuse rather than guess: a manifest without exactly one signature value
    /// is a manifest this tool does not understand.
    #[test]
    fn a_manifest_with_no_signature_value_is_refused() {
        let err = replace_signature_value("id: toy\n", "X").unwrap_err();
        assert!(err.to_string().contains("no `signature.value`"), "{err:#}");
    }

    /// A key that is not a key fails with a message that says so, not a panic.
    #[test]
    fn a_bad_signing_key_is_a_clear_error() {
        let (manifest, assets) = toy_package("PLACEHOLDER");
        let err = sign_package(&manifest, &assets, "not a key").unwrap_err();
        assert!(err.to_string().contains("PKCS#8 PEM"), "{err:#}");
    }

    /// Signing with the wrong key produces a package the core rejects as
    /// TAMPERED — the keyring is what decides, not the signer.
    #[test]
    fn a_package_signed_by_a_foreign_key_is_rejected() {
        let (key_pem, _) = throwaway_key();
        let (manifest, assets) = toy_package("PLACEHOLDER");
        let value = sign_package(&manifest, &assets, &key_pem).unwrap();
        let signed = replace_signature_value(&manifest, &value).unwrap();
        let err = verify_package(&signed, &assets).unwrap_err();
        assert!(err.to_string().contains("TAMPERED"), "unhelpful: {err:#}");
    }

    /// THE regression test for this whole feature: re-signing a package that
    /// is already published must reproduce its signature byte for byte.
    /// ed25519 is deterministic, so a mismatch means the digest changed — which
    /// would silently invalidate every package already in the wild.
    ///
    /// Needs both the registry checkout and the private key, so it is opt-in.
    ///
    /// Skips — never fails — when the key is absent, including in CI. That is
    /// the opposite of the registry gate's rule (`registry_golden`, #108),
    /// deliberately: a CI lane can and does clone the registry, but the signing
    /// key is not in CI and is never going to be (`keys/README.md`). A test
    /// that demanded it would only ever be a broken lane. This one runs where
    /// it is meaningful — on the machine that actually signs.
    #[test]
    #[ignore = "needs the registry checkout and the private key (set VELOX_SIGNING_KEY_FILE)"]
    fn re_signing_a_published_package_reproduces_its_signature() {
        let Some(root) = checkout() else { return };
        let Ok(key_file) = std::env::var("VELOX_SIGNING_KEY_FILE") else {
            crate::registry_golden::to_stderr(
                "\n==============================================================\n\
                 SKIPPED re-signing gate: VELOX_SIGNING_KEY_FILE is not set.\n\
                 The signing key is not in CI and never will be; run this where\n\
                 you actually sign:\n\
                 VELOX_REGISTRY_PATH=… VELOX_SIGNING_KEY_FILE=… \\\n\
                 cargo test -- --ignored re_signing\n\
                 ==============================================================\n",
            );
            return;
        };
        let key_pem = std::fs::read_to_string(&key_file)
            .unwrap_or_else(|e| panic!("reading the signing key from {key_file}: {e}"));

        for id in ["nginx", "kubernetes", "kafka"] {
            let dir = root.join("integrations").join(id);
            let pkg = FetchedPackage::load_from_dir(&dir).expect(id);
            let published: SignedManifest = serde_yaml::from_str(&pkg.manifest_yaml).unwrap();
            let want = published.signature.expect(id).value;
            let got = sign_package(&pkg.manifest_yaml, &pkg.assets, &key_pem).unwrap();
            assert_eq!(
                want.trim(),
                got,
                "{id}: re-signing did not reproduce the published signature"
            );
        }
    }

    // ── catalog parsing + fetch ─────────────────────────────────────────

    /// The registry's real `catalog.json` parses, and every row it advertises
    /// has a package directory behind it.
    #[test]
    fn real_catalog_parses() {
        let Some(root) = checkout() else { return };
        let reg = Registry::new(format!("file://{}", root.display()), None);
        let cat = rt().block_on(reg.fetch_catalog()).expect("fetch catalog");
        assert_eq!(cat.schema_version, "1.0");
        assert!(
            cat.integrations.len() >= 12,
            "{} rows",
            cat.integrations.len()
        );
        for e in &cat.integrations {
            assert!(!e.version.is_empty(), "{}: no version", e.id);
            assert!(e.title.contains_key("pt"), "{}: no pt title", e.id);
            assert!(
                root.join("integrations").join(&e.id).is_dir(),
                "{}: catalog row with no package",
                e.id
            );
        }
    }

    /// THIS core satisfies every shipped package's `min_core_version`. The
    /// registry declares 0.7.0 as "the first core that ships the apply-engine";
    /// if a release ever regresses below a package's floor, install refuses and
    /// the Integrations tab greys out — so the claim is gated, not assumed.
    #[test]
    fn this_core_satisfies_every_package_floor() {
        let Some(root) = checkout() else { return };
        let reg = Registry::new(format!("file://{}", root.display()), None);
        let cat = rt().block_on(reg.fetch_catalog()).unwrap();
        for e in &cat.integrations {
            assert!(
                version_ge(core_version(), &e.min_core_version),
                "{}: needs core {} but this core is {}",
                e.id,
                e.min_core_version,
                core_version()
            );
        }
    }

    /// Malformed catalog JSON is an error, not a panic.
    #[test]
    fn malformed_catalog_is_an_error() {
        assert!(serde_json::from_slice::<Catalog>(b"{\"schema_version\":1}").is_err());
    }

    /// A `file://` base fetches a package whose bytes verify — the offline
    /// delivery route, same format and same signature check as the wire.
    #[test]
    fn file_transport_fetches_and_verifies() {
        let Some(root) = checkout() else { return };
        let reg = Registry::new(format!("file://{}", root.display()), None);
        let pkg = rt()
            .block_on(reg.fetch_package("nginx"))
            .expect("fetch nginx");
        assert_eq!(pkg.id, "nginx");
        verify_package(&pkg.manifest_yaml, &pkg.assets).expect("fetched package verifies");
        let engine_pkg = pkg.into_package().expect("into engine package");
        assert_eq!(engine_pkg.manifest.index, "nginx-logs");
        assert!(engine_pkg.pipeline.is_some());
    }

    /// Traversal never reaches the transport: an id or an asset filename with
    /// path separators is refused before a URL is built.
    #[test]
    fn unsafe_ids_and_filenames_are_refused() {
        for bad in ["../secrets", "a/b", "..", "", "x y"] {
            assert!(!valid_component(bad), "{bad:?} must be refused");
        }
        let reg = Registry::new("file:///nonexistent", None);
        let err = rt().block_on(reg.fetch_package("../../etc")).unwrap_err();
        assert!(err.to_string().contains("unsafe id"), "{err:#}");

        let manifest = "assets:\n  saved_objects: ../../etc/passwd\n  agent_config: a.tmpl\n";
        let err = asset_filenames(manifest).unwrap_err();
        assert!(err.to_string().contains("unsafe asset filename"), "{err:#}");
    }

    // ── degraded registry ───────────────────────────────────────────────

    /// Registry unreachable and nothing cached ⇒ the embedded bootstrap floor,
    /// flagged stale with the reason. The tab renders; it does not break.
    #[test]
    fn unreachable_registry_falls_back_to_bootstrap() {
        let cache = CatalogCache::new();
        let reg = Registry::new("file:///nonexistent-registry-path", None);
        let state = rt().block_on(cache.load(&reg, DEFAULT_TTL));
        assert_eq!(state.source, CatalogSource::Bootstrap);
        assert!(state.error.is_some(), "the reason must be reported");

        let view = build_view(state, &BTreeMap::new());
        assert!(view.stale);
        assert_eq!(view.integrations.len(), BOOTSTRAP_IDS.len());
        assert_eq!(view.integrations[0].id, "kubernetes");
        assert!(view.integrations[0].builtin);
        assert!(view.integrations[0].compatible);
    }

    /// Registry unreachable but a catalog was fetched earlier ⇒ the CACHED
    /// catalog is served, marked stale — the whole shelf stays visible.
    #[test]
    fn unreachable_registry_serves_the_stale_cache() {
        let Some(root) = checkout() else { return };
        let cache = CatalogCache::new();
        let rt = rt();

        let good = Registry::new(format!("file://{}", root.display()), None);
        let fresh = rt.block_on(cache.load(&good, DEFAULT_TTL));
        assert_eq!(fresh.source, CatalogSource::Registry);
        let n = fresh.catalog.integrations.len();

        // Same cache, now with a dead endpoint and a zero TTL so it must refetch.
        let dead = Registry::new("file:///nonexistent-registry-path", None);
        let state = rt.block_on(cache.load(&dead, Duration::ZERO));
        assert_eq!(state.source, CatalogSource::Cache);
        assert_eq!(state.catalog.integrations.len(), n, "cached rows survive");
        assert!(state.error.is_some());
    }

    /// A fetched catalog is served from cache inside the TTL without going
    /// back to the registry (proved by killing the endpoint in between).
    #[test]
    fn fresh_cache_is_served_without_refetching() {
        let Some(root) = checkout() else { return };
        let cache = CatalogCache::new();
        let rt = rt();
        rt.block_on(cache.load(
            &Registry::new(format!("file://{}", root.display()), None),
            DEFAULT_TTL,
        ));
        let state = rt.block_on(cache.load(
            &Registry::new("file:///nonexistent-registry-path", None),
            DEFAULT_TTL,
        ));
        assert_eq!(state.source, CatalogSource::Registry);
        assert!(state.error.is_none(), "no fetch was attempted");
    }

    // ── view assembly ───────────────────────────────────────────────────

    /// installed / available / update-available, computed from the recorded
    /// per-deployment versions (ADR-039 "Versioning").
    #[test]
    fn view_computes_installed_and_update_available() {
        let catalog = Catalog {
            schema_version: "1.0".into(),
            integrations: vec![
                CatalogEntry {
                    id: "nginx".into(),
                    version: "1.2.0".into(),
                    title: BTreeMap::new(),
                    summary: BTreeMap::new(),
                    min_core_version: "0.1.0".into(),
                },
                CatalogEntry {
                    id: "redis".into(),
                    version: "1.0.0".into(),
                    title: BTreeMap::new(),
                    summary: BTreeMap::new(),
                    min_core_version: "0.1.0".into(),
                },
                CatalogEntry {
                    id: "future".into(),
                    version: "1.0.0".into(),
                    title: BTreeMap::new(),
                    summary: BTreeMap::new(),
                    min_core_version: "99.0.0".into(),
                },
            ],
        };
        let installed: BTreeMap<String, String> = [("nginx".to_string(), "1.1.0".to_string())]
            .into_iter()
            .collect();
        let view = build_view(
            CatalogState {
                catalog,
                source: CatalogSource::Registry,
                age_seconds: Some(0),
                error: None,
            },
            &installed,
        );
        let by_id = |id: &str| {
            view.integrations
                .iter()
                .find(|i| i.id == id)
                .unwrap()
                .clone()
        };

        let nginx = by_id("nginx");
        assert_eq!(nginx.installed_version.as_deref(), Some("1.1.0"));
        assert!(nginx.update_available, "1.1.0 installed, 1.2.0 offered");

        let redis = by_id("redis");
        assert!(
            redis.installed_version.is_none(),
            "available, not installed"
        );
        assert!(!redis.update_available);

        assert!(
            !by_id("future").compatible,
            "min_core_version 99 is a floor"
        );
        // The bootstrap floor is merged in even when the registry omits it.
        assert!(by_id("kubernetes").builtin);
        assert!(!view.stale);
    }

    /// A registry row for a bootstrap id WINS over the embedded entry — the
    /// floor is a fallback, never a shadow copy that pins an old version.
    #[test]
    fn registry_row_overrides_the_bootstrap_entry() {
        let catalog = Catalog {
            schema_version: "1.0".into(),
            integrations: vec![CatalogEntry {
                id: "kubernetes".into(),
                version: "2.0.0".into(),
                title: BTreeMap::new(),
                summary: BTreeMap::new(),
                min_core_version: "0.1.0".into(),
            }],
        };
        let view = build_view(
            CatalogState {
                catalog,
                source: CatalogSource::Registry,
                age_seconds: Some(0),
                error: None,
            },
            &BTreeMap::new(),
        );
        assert_eq!(view.integrations.len(), 1);
        assert_eq!(view.integrations[0].version, "2.0.0");
    }

    /// Every bootstrap id is genuinely installable with no egress — i.e. it is
    /// a recipe the core still carries.
    #[test]
    fn bootstrap_ids_are_installable_offline() {
        for id in BOOTSTRAP_IDS {
            assert!(
                crate::recipes::RECIPES.contains(id),
                "{id} is advertised as a bootstrap floor but the core cannot apply it"
            );
        }
        assert!(!bootstrap_catalog().integrations.is_empty());
    }

    /// The bootstrap floor's version claim matches what the registry publishes
    /// for the same id — the entry is a mirror of a real package, not a guess.
    #[test]
    fn bootstrap_version_matches_the_registry() {
        let Some(root) = checkout() else { return };
        let reg = Registry::new(format!("file://{}", root.display()), None);
        let cat = rt().block_on(reg.fetch_catalog()).unwrap();
        for id in BOOTSTRAP_IDS {
            let e = cat
                .integrations
                .iter()
                .find(|e| e.id == *id)
                .unwrap_or_else(|| panic!("{id} missing from the registry catalog"));
            assert_eq!(
                e.version, BOOTSTRAP_VERSION,
                "{id}: embedded bootstrap version drifted from the registry"
            );
        }
    }

    // ── version comparison ──────────────────────────────────────────────

    #[test]
    fn version_ge_compares_by_component() {
        assert!(version_ge("0.7.0", "0.7.0"));
        assert!(version_ge("0.7.1", "0.7.0"));
        assert!(version_ge("1.0.0", "0.9.9"));
        assert!(version_ge("0.10.0", "0.9.0"), "10 > 9, not string order");
        assert!(!version_ge("0.6.2", "0.7.0"));
        assert!(version_ge("2", "1.9.9"), "missing components are 0");
    }
}
