// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Licensing for Velox Mesh: the signed licence document, the free tier, and
//! seat accounting. Pure — no I/O, no clock of its own — so every rule here is
//! unit-tested and the server (`velox-mesh`) only persists and applies.
//!
//! ## The rules (veloxagent/docs/ARCHITECTURE.md §6)
//!
//! * **Free tier without any document**: [`FREE_SEATS`] agents. No activation,
//!   no network, no registration. An install that never sees a licence file
//!   behaves exactly like one holding a 10-seat licence that never expires.
//! * **A seat is an *active* agent**: enrolled and seen within the seat window
//!   (the server decides the window; this crate only counts what it is told).
//!   Unenrolling frees the seat immediately. Agents that stop reporting fall
//!   out of the count on their own, so a decommissioned host never needs a
//!   human to release anything.
//! * **Over the limit, an agent is parked, not refused.** Enrollment is
//!   accepted, the agent is visible in the UI as *waiting for a seat*, it
//!   receives **no policy** and heartbeats at a slow cadence. The operator sees
//!   the shortfall where they would look for it, instead of debugging a host
//!   that "does not show up".
//! * **Expiry never stops data.** Past `expires_at` the licence enters a grace
//!   period ([`License::grace_days`], default [`DEFAULT_GRACE_DAYS`]) in which
//!   everything keeps working under a banner. After grace the Mesh is
//!   *read-only*: no new enrollments, no policy changes, but every enrolled
//!   agent keeps the configuration it has. Logs do not stop flowing because a
//!   purchase order is late — that is a product principle, not a courtesy.
//! * **Enforcement is server-side and honest about it.** This code is
//!   AGPL-3.0-only (ADR-068): anyone may remove the check and run the result,
//!   and that is a right the licence grants, not a loophole. The seat count
//!   is therefore a *commercial* signal — what the official build and its
//!   support subscription cover — not a technical lock. The signature exists
//!   so that a *forged* document is detectable in the official build, never
//!   to stop a *removed* check.
//!
//! ## The document
//!
//! A [`SignedLicense`] is the licence JSON as issued (base64, byte-exact) plus
//! an ed25519 signature over those exact bytes and the id of the signing key.
//! Signing the bytes rather than a canonicalised form keeps the verifier
//! trivial and leaves no canonicalisation ambiguity to exploit. Verification
//! uses a keyring compiled into the binary, like the core's registry keyring
//! (`src/catalog.rs`): no network, works air-gapped, and rotating a key is a
//! release — never drop the old key in the release that adds its replacement
//! (a customer's licence file predates the new key).

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Agents an unlicensed installation may manage concurrently.
pub const FREE_SEATS: u32 = 10;

/// Days of full operation after `expires_at` before the Mesh goes read-only.
pub const DEFAULT_GRACE_DAYS: u32 = 30;

/// Seconds in a day, for the grace arithmetic. Timestamps everywhere in this
/// crate are Unix seconds, UTC.
const DAY: u64 = 86_400;

/// Compiled-in licence-signing public keys: `(key id, raw 32-byte ed25519
/// public key)`. Empty until the first issuing key is generated (veloxagent/keys/,
/// private half never committed). An empty keyring means every signed
/// document is rejected as `UnknownKey` — fail closed — while the free tier
/// keeps working, because the free tier needs no document.
pub const KEYRING: &[(&str, &[u8; 32])] = &[];

/// The licence as the issuer writes it. Only the fields the Mesh reads are
/// modelled; unknown fields are ignored so an issuer can add metadata (order
/// number, contact) without a Mesh release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    /// Who holds it, for the Settings › Licence screen. Display only.
    pub licensee: String,
    /// Concurrent agents this licence allows. Independent of the free tier:
    /// a 50-seat licence is 50 seats, not 60.
    pub seats: u32,
    /// Unix seconds, UTC.
    pub issued_at: u64,
    /// `None` is perpetual.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Full-operation days after expiry. Defaults to [`DEFAULT_GRACE_DAYS`].
    #[serde(default = "default_grace")]
    pub grace_days: u32,
    /// Feature flags beyond seats (`gateway`, `audit`, `sso`). The free tier
    /// has none; the Mesh treats an unknown flag as absent.
    #[serde(default)]
    pub features: Vec<String>,
}

fn default_grace() -> u32 {
    DEFAULT_GRACE_DAYS
}

/// The file an operator pastes or uploads. `license` is base64 of the licence
/// JSON exactly as signed; `signature` is base64 of the 64-byte ed25519
/// signature over those decoded bytes; `key_id` names the keyring entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedLicense {
    pub license: String,
    pub key_id: String,
    pub signature: String,
}

/// Why a document was refused. Three distinct rejects, never collapsed into
/// one message — the operator fixing a paste error and the operator holding
/// a forged file need different words.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("licence is not valid base64 or JSON: {0}")]
    Malformed(String),
    #[error(
        "licence was signed by an unknown key ({0}); update Velox Mesh or ask for a reissued file"
    )]
    UnknownKey(String),
    #[error("licence signature does not match its contents; the file was altered or corrupted")]
    BadSignature,
}

/// Verify `signed` against `keyring` and return the licence it carries.
pub fn verify(
    signed: &SignedLicense,
    keyring: &[(&str, &[u8; 32])],
) -> Result<License, VerifyError> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let bytes = b64
        .decode(signed.license.trim())
        .map_err(|e| VerifyError::Malformed(e.to_string()))?;
    let sig = b64
        .decode(signed.signature.trim())
        .map_err(|e| VerifyError::Malformed(e.to_string()))?;
    let (_, key) = keyring
        .iter()
        .find(|(id, _)| *id == signed.key_id)
        .ok_or_else(|| VerifyError::UnknownKey(signed.key_id.clone()))?;
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, key.as_slice())
        .verify(&bytes, &sig)
        .map_err(|_| VerifyError::BadSignature)?;
    // Parse only after the signature holds: a forged document never reaches
    // the deserialiser.
    serde_json::from_slice(&bytes).map_err(|e| VerifyError::Malformed(e.to_string()))
}

/// Where the installation stands right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// No document: [`FREE_SEATS`], forever.
    Free,
    /// A valid, unexpired licence.
    Licensed,
    /// Expired, inside the grace window. Everything works; the UI shows a
    /// banner with the day the Mesh goes read-only.
    Grace { read_only_at: u64 },
    /// Expired past grace. Enrolled agents keep their configuration; nothing
    /// new is accepted until a licence is loaded.
    ReadOnly { expired_at: u64 },
}

/// What the Mesh may do, derived from the licence and the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlement {
    pub seats: u32,
    pub tier: Tier,
    pub features: Vec<String>,
}

impl Entitlement {
    /// Policy changes and new enrollments are allowed.
    pub fn writable(&self) -> bool {
        !matches!(self.tier, Tier::ReadOnly { .. })
    }

    pub fn has_feature(&self, flag: &str) -> bool {
        self.features.iter().any(|f| f == flag)
    }
}

/// Derive the entitlement at `now` (Unix seconds).
pub fn entitlement(license: Option<&License>, now: u64) -> Entitlement {
    let Some(lic) = license else {
        return Entitlement {
            seats: FREE_SEATS,
            tier: Tier::Free,
            features: Vec::new(),
        };
    };
    let tier = match lic.expires_at {
        None => Tier::Licensed,
        Some(exp) if now < exp => Tier::Licensed,
        Some(exp) => {
            let read_only_at = exp.saturating_add(u64::from(lic.grace_days).saturating_mul(DAY));
            if now < read_only_at {
                Tier::Grace { read_only_at }
            } else {
                Tier::ReadOnly { expired_at: exp }
            }
        }
    };
    Entitlement {
        seats: lic.seats,
        tier,
        features: lic.features.clone(),
    }
}

/// The answer to "may this agent take a seat?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Enrol and deliver policy.
    Admit,
    /// Enrol, show, deliver nothing. The reason is what the UI says.
    Park(ParkReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkReason {
    /// `active` agents already hold every seat of `seats`.
    SeatsExhausted { seats: u32, active: u32 },
    /// The licence is past grace; nothing new is admitted.
    ReadOnly,
}

/// Decide admission for one more agent when `active` agents currently hold
/// seats. Pure: the caller counts active agents against its seat window.
pub fn admit(ent: &Entitlement, active: u32) -> Admission {
    if !ent.writable() {
        return Admission::Park(ParkReason::ReadOnly);
    }
    if active >= ent.seats {
        return Admission::Park(ParkReason::SeatsExhausted {
            seats: ent.seats,
            active,
        });
    }
    Admission::Admit
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};

    const NOW: u64 = 1_800_000_000;

    fn lic(seats: u32, expires_at: Option<u64>) -> License {
        License {
            licensee: "ACME".into(),
            seats,
            issued_at: NOW - 10 * DAY,
            expires_at,
            grace_days: DEFAULT_GRACE_DAYS,
            features: vec!["gateway".into()],
        }
    }

    #[test]
    fn no_document_is_the_free_tier() {
        let e = entitlement(None, NOW);
        assert_eq!(e.seats, FREE_SEATS);
        assert_eq!(e.tier, Tier::Free);
        assert!(e.writable());
        assert!(!e.has_feature("gateway"));
    }

    #[test]
    fn tenth_agent_is_admitted_and_eleventh_is_parked() {
        let e = entitlement(None, NOW);
        assert_eq!(admit(&e, 9), Admission::Admit);
        assert_eq!(
            admit(&e, 10),
            Admission::Park(ParkReason::SeatsExhausted {
                seats: 10,
                active: 10
            })
        );
    }

    #[test]
    fn a_licence_replaces_the_free_seats_rather_than_adding_to_them() {
        let e = entitlement(Some(&lic(5, None)), NOW);
        assert_eq!(e.seats, 5);
        assert_eq!(e.tier, Tier::Licensed);
        assert!(e.has_feature("gateway"));
    }

    #[test]
    fn expiry_means_grace_then_read_only_and_data_never_stops() {
        let exp = NOW - DAY;
        let e = entitlement(Some(&lic(50, Some(exp))), NOW);
        assert_eq!(
            e.tier,
            Tier::Grace {
                read_only_at: exp + 30 * DAY
            }
        );
        assert!(e.writable(), "grace is full operation");
        assert_eq!(admit(&e, 10), Admission::Admit);

        let later = exp + 31 * DAY;
        let e = entitlement(Some(&lic(50, Some(exp))), later);
        assert_eq!(e.tier, Tier::ReadOnly { expired_at: exp });
        assert!(!e.writable());
        // Seats still report 50: the enrolled agents keep what they have.
        assert_eq!(e.seats, 50);
        assert_eq!(admit(&e, 0), Admission::Park(ParkReason::ReadOnly));
    }

    fn sign(lic: &License, kp: &Ed25519KeyPair, key_id: &str) -> (SignedLicense, [u8; 32]) {
        let b64 = base64::engine::general_purpose::STANDARD;
        let bytes = serde_json::to_vec(lic).unwrap();
        let sig = kp.sign(&bytes);
        let mut pk = [0u8; 32];
        pk.copy_from_slice(kp.public_key().as_ref());
        (
            SignedLicense {
                license: b64.encode(&bytes),
                key_id: key_id.into(),
                signature: b64.encode(sig.as_ref()),
            },
            pk,
        )
    }

    #[test]
    fn a_signed_document_verifies_and_the_three_rejects_are_distinct() {
        let kp = Ed25519KeyPair::generate().unwrap();
        let (signed, pk) = sign(&lic(25, None), &kp, "test-2026-11");
        let keyring: &[(&str, &[u8; 32])] = &[("test-2026-11", &pk)];

        assert_eq!(verify(&signed, keyring).unwrap().seats, 25);

        // Unknown key: a Mesh older than the issuing key, or a wrong file.
        let other = SignedLicense {
            key_id: "test-2030-01".into(),
            ..signed.clone()
        };
        assert_eq!(
            verify(&other, keyring),
            Err(VerifyError::UnknownKey("test-2030-01".into()))
        );

        // Tampered: seats edited after signing.
        let edited = lic(2500, None);
        let b64 = base64::engine::general_purpose::STANDARD;
        let tampered = SignedLicense {
            license: b64.encode(serde_json::to_vec(&edited).unwrap()),
            ..signed.clone()
        };
        assert_eq!(verify(&tampered, keyring), Err(VerifyError::BadSignature));

        // Malformed: a paste error.
        let garbage = SignedLicense {
            license: "not base64!".into(),
            ..signed.clone()
        };
        assert!(matches!(
            verify(&garbage, keyring),
            Err(VerifyError::Malformed(_))
        ));

        // The compiled-in keyring is empty today: fail closed.
        assert!(matches!(
            verify(&signed, KEYRING),
            Err(VerifyError::UnknownKey(_))
        ));
    }

    #[test]
    fn unknown_fields_in_the_document_are_tolerated() {
        let raw = r#"{"licensee":"ACME","seats":3,"issued_at":1,"order":"PO-77"}"#;
        let l: License = serde_json::from_str(raw).unwrap();
        assert_eq!(l.grace_days, DEFAULT_GRACE_DAYS);
        assert_eq!(l.expires_at, None);
        assert!(l.features.is_empty());
    }
}
