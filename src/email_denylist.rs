// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Corporate-email gate (ADR-038) — the server-side port of the demo-gate rule.
//!
//! The landing page's lead form already refuses free/consumer/disposable
//! mailboxes (`is_free_email` in `get-site/lead-api/lead_api.py`, issue #62).
//! Self-serve signup (#79) must refuse the same addresses, or the two front
//! doors of the same product disagree about who is a qualified user. This
//! module is that list ported verbatim to Rust — the *policy* is ADR-038's,
//! this is only its second implementation.
//!
//! It stays a DENYLIST, never an allowlist: the set of legitimate corporate
//! domains is unbounded and unknowable, the set of consumer mailboxes is small
//! and famous. Two rules, both cheap:
//!
//!   1. [`FREE_EMAIL_DOMAINS`] — exact domain, or any subdomain of one, so
//!      `smtp.gmail.com` cannot sneak past.
//!   2. [`FREE_EMAIL_BRANDS`] × [`PUBLIC_SUFFIXES`] — the giants ship one
//!      mailbox per country (`yahoo.com.br`, `hotmail.fr`, `live.com.mx`) and
//!      enumerating every ccTLD is a losing game. `<brand>.<public suffix>` is
//!      free; the public suffix is REQUIRED so a real company's
//!      `live.acme-corp.io` still passes.
//!
//! A false reject costs a signup, so the brand rule stays conservative.
//!
//! The three tables are sorted so lookup is a binary search over `&'static
//! str` with no allocation and no lazy initialisation; `tables_are_sorted`
//! below is what keeps that invariant true when someone appends a domain.

/// One local part, exactly one `@`, a dot-separated domain. Deliberately
/// plainer than RFC 5322 — this is account signup, not address verification.
/// Kept as hand-rolled character classes rather than a regex dependency: the
/// grammar is small enough that the code IS the specification.
const MAX_EMAIL_LEN: usize = 254; // RFC 5321 practical ceiling

/// Normalise and syntactically validate an address. Returns the trimmed,
/// lower-cased address when acceptable — `users.email` is `citext`, so
/// lower-casing here only makes what we store match what we compare.
pub fn validate_email(raw: &str) -> Option<String> {
    let email = raw.trim();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN {
        return None;
    }
    let mut parts = email.split('@');
    let (local, domain) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None; // more than one '@'
    }
    if local.is_empty() || !local.bytes().all(is_local_byte) {
        return None;
    }
    if !is_domain(domain) {
        return None;
    }
    Some(email.to_ascii_lowercase())
}

fn is_local_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b)
}

/// At least two dot-separated labels; each label alphanumeric, with interior
/// hyphens allowed but never leading or trailing.
fn is_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2 && labels.iter().all(|l| is_domain_label(l))
}

fn is_domain_label(label: &str) -> bool {
    let b = label.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

/// The lower-cased domain part of an already-validated address.
pub fn email_domain(email: &str) -> &str {
    email.rsplit('@').next().unwrap_or("").trim_end_matches('.')
}

/// True when the address is a free/consumer/disposable mailbox — i.e. NOT a
/// work address. The ADR-038 gate; see the module docs for the two rules.
pub fn is_free_email(email: &str) -> bool {
    let domain = email_domain(email).to_ascii_lowercase();
    if denied(&domain) {
        return true;
    }
    // A subdomain of a denied domain is denied too (mail.yahoo.com, …).
    let labels: Vec<&str> = domain.split('.').collect();
    for i in 1..labels.len() {
        if denied(&labels[i..].join(".")) {
            return true;
        }
    }
    // <brand>.<public suffix>: yahoo.com.br, hotmail.fr, live.com.mx …
    labels.len() >= 2
        && FREE_EMAIL_BRANDS.binary_search(&labels[0]).is_ok()
        && PUBLIC_SUFFIXES
            .binary_search(&labels[1..].join(".").as_str())
            .is_ok()
}

fn denied(domain: &str) -> bool {
    FREE_EMAIL_DOMAINS.binary_search(&domain).is_ok()
}

/// Free webmail, ISP mailboxes and the usual disposable-address services.
/// Sorted (see `tables_are_sorted`); grouped by origin in ADR-038's list, the
/// grouping is preserved in the comments rather than the order.
// global webmail · Brazil/LatAm consumer + ISP · Europe ISP · disposable
const FREE_EMAIL_DOMAINS: &[&str] = &[
    "10minutemail.com",
    "126.com",
    "163.com",
    "aol.com",
    "bk.ru",
    "bluewin.ch",
    "bol.com.br",
    "brturbo.com.br",
    "click21.com.br",
    "daum.net",
    "discard.email",
    "dispostable.com",
    "email.com",
    "emailondeck.com",
    "fakeinbox.com",
    "fastmail.com",
    "free.fr",
    "freenet.de",
    "getnada.com",
    "globo.com",
    "globomail.com",
    "gmail.com",
    "gmx.com",
    "gmx.de",
    "gmx.net",
    "googlemail.com",
    "guerrillamail.com",
    "hanmail.net",
    "hotmail.com",
    "hushmail.com",
    "icloud.com",
    "ig.com.br",
    "inbox.ru",
    "inboxkitten.com",
    "interia.pl",
    "itelefonica.com.br",
    "laposte.net",
    "libero.it",
    "list.ru",
    "live.com",
    "mac.com",
    "mail.com",
    "mail.ru",
    "maildrop.cc",
    "mailinator.com",
    "mailnesia.com",
    "mailsac.com",
    "me.com",
    "mintemail.com",
    "moakt.com",
    "mohmal.com",
    "msn.com",
    "naver.com",
    "o2.pl",
    "oi.com.br",
    "orange.fr",
    "outlook.com",
    "passport.com",
    "pm.me",
    "pop.com.br",
    "prodigy.net.mx",
    "proton.me",
    "protonmail.ch",
    "protonmail.com",
    "qq.com",
    "r7.com",
    "rediffmail.com",
    "rocketmail.com",
    "sapo.pt",
    "seznam.cz",
    "sfr.fr",
    "sharklasers.com",
    "sina.com",
    "spamgourmet.com",
    "superig.com.br",
    "t-online.de",
    "telefonica.net",
    "telenet.be",
    "temp-mail.org",
    "tempmail.com",
    "tempr.email",
    "terra.com.br",
    "throwawaymail.com",
    "tiscali.it",
    "trashmail.com",
    "tuta.io",
    "tutanota.com",
    "uol.com",
    "uol.com.br",
    "usa.com",
    "veloxmail.com.br",
    "virgilio.it",
    "wanadoo.fr",
    "web.de",
    "wp.pl",
    "yahoo.com",
    "yandex.com",
    "yandex.ru",
    "ymail.com",
    "yopmail.com",
    "ziggo.nl",
    "zipmail.com.br",
    "zoho.com",
];

/// Brands that ship a per-country mailbox; only free when paired with a
/// [`PUBLIC_SUFFIXES`] tail.
const FREE_EMAIL_BRANDS: &[&str] = &[
    "aol",
    "gmail",
    "gmx",
    "googlemail",
    "hotmail",
    "icloud",
    "live",
    "msn",
    "outlook",
    "protonmail",
    "rocketmail",
    "yahoo",
    "ymail",
];

/// Deliberately not a full PSL — just enough tails to make the brand rule
/// sound for the markets the product is sold in (en/es/pt) plus the usual
/// suspects.
const PUBLIC_SUFFIXES: &[&str] = &[
    "ar", "at", "au", "be", "biz", "bo", "br", "ca", "ch", "cl", "cn", "co", "co.il", "co.in",
    "co.jp", "co.kr", "co.nz", "co.uk", "co.za", "com", "com.ar", "com.au", "com.bo", "com.br",
    "com.cn", "com.co", "com.ec", "com.hk", "com.mx", "com.my", "com.pe", "com.ph", "com.py",
    "com.sg", "com.tr", "com.tw", "com.uy", "com.ve", "com.vn", "cz", "de", "dk", "ec", "es", "fi",
    "fr", "gr", "hk", "hu", "id", "ie", "il", "in", "info", "it", "jp", "kr", "mx", "my", "net",
    "nl", "no", "nz", "org", "pe", "ph", "pl", "pt", "py", "ro", "ru", "se", "sg", "sk", "th",
    "tr", "tw", "ua", "uk", "us", "uy", "ve", "vn", "za",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The lookup is a binary search; an unsorted table silently answers
    /// "not free" for entries past the first inversion, which is the exact
    /// failure this gate must never have.
    #[test]
    fn tables_are_sorted_and_unique() {
        for (name, table) in [
            ("FREE_EMAIL_DOMAINS", FREE_EMAIL_DOMAINS),
            ("FREE_EMAIL_BRANDS", FREE_EMAIL_BRANDS),
            ("PUBLIC_SUFFIXES", PUBLIC_SUFFIXES),
        ] {
            for w in table.windows(2) {
                assert!(
                    w[0] < w[1],
                    "{name} not sorted/unique at {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            }
        }
    }

    /// The port's fidelity to ADR-038 cannot be re-derived from the tree —
    /// `get-site/` was removed from core by !112, so the source list now lives
    /// only in git history (`git show 5bc9872:get-site/lead-api/lead_api.py`).
    /// These counts are what the diff against that revision produced; they turn
    /// a silently dropped entry into a failing test.
    #[test]
    fn tables_match_the_adr038_source_sizes() {
        assert_eq!(FREE_EMAIL_DOMAINS.len(), 103);
        assert_eq!(FREE_EMAIL_BRANDS.len(), 13);
        assert_eq!(PUBLIC_SUFFIXES.len(), 84);
    }

    #[test]
    fn rejects_the_famous_consumer_mailboxes() {
        for email in [
            "someone@gmail.com",
            "someone@GMAIL.COM",
            "someone@hotmail.com",
            "someone@uol.com.br",
            "someone@proton.me",
            "someone@mailinator.com",
        ] {
            assert!(is_free_email(email), "{email} must be denied");
        }
    }

    /// Rule 1's subdomain arm: `smtp.gmail.com` is the same mailbox provider.
    #[test]
    fn rejects_subdomains_of_denied_domains() {
        assert!(is_free_email("someone@mail.yahoo.com"));
        assert!(is_free_email("someone@smtp.gmail.com"));
    }

    /// Rule 2: `<brand>.<public suffix>` covers the per-country mailboxes.
    #[test]
    fn rejects_brand_plus_public_suffix() {
        for email in [
            "someone@yahoo.com.br",
            "someone@hotmail.fr",
            "someone@live.com.mx",
            "someone@outlook.es",
        ] {
            assert!(is_free_email(email), "{email} must be denied");
        }
    }

    /// The conservative half of rule 2: the public suffix is REQUIRED, so a
    /// company that happens to start with a brand word still gets in.
    #[test]
    fn accepts_corporate_addresses() {
        for email in [
            "alice@acmecorp.com.br",
            "ops@veloxsearch.ai",
            "someone@live.acme-corp.io",
            "someone@outlook-consulting.com",
            "someone@gmail.acme.com",
        ] {
            assert!(!is_free_email(email), "{email} must be accepted");
        }
    }

    #[test]
    fn validate_email_normalises_and_rejects_junk() {
        assert_eq!(
            validate_email("  Ops@VeloxSearch.AI "),
            Some("ops@veloxsearch.ai".to_string())
        );
        for bad in [
            "",
            "   ",
            "no-at-sign",
            "two@at@signs.com",
            "@nolocal.com",
            "local@",
            "local@nodot",
            "local@-leadinghyphen.com",
            "local@trailinghyphen-.com",
            "local@double..dot.com",
            "spaced address@example.com",
        ] {
            assert!(validate_email(bad).is_none(), "{bad:?} must be rejected");
        }
        // Length ceiling (RFC 5321): 254 accepted, 255 refused.
        let long_local = "a".repeat(MAX_EMAIL_LEN - "@example.com".len());
        assert!(validate_email(&format!("{long_local}@example.com")).is_some());
        assert!(validate_email(&format!("{long_local}a@example.com")).is_none());
    }

    #[test]
    fn email_domain_strips_the_local_part_and_trailing_dot() {
        assert_eq!(email_domain("someone@example.com"), "example.com");
        assert_eq!(email_domain("someone@example.com."), "example.com");
    }
}
