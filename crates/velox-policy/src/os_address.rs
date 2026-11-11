// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Where a host's collector reaches OpenSearch, in the shape Fluent Bit's
//! `opensearch` output takes it (ADR-072 amendment 2).
//!
//! The output stanza has no single "address" key: it takes `Host` (a name or
//! an IP, nothing else), `Port`, `tls` and an optional `Path` prefix for an
//! OpenSearch served behind a reverse proxy. A package's `agent.conf.tmpl`
//! fixes three of the four (`Port 9200`, `tls On`, no path) and interpolates
//! only `{os_host}` — right for the DaemonSet path, where the service is
//! always `<deployment>.<ns>.svc:9200`, wrong the moment an operator types
//! `os.example.com:443` or an ingress URL into the deployment's settings:
//! `Host os.example.com:443` is not a host, and `Port 9200` is not the port.
//!
//! So the endpoint is parsed once, here, and the compiler **binds** the
//! policy's endpoint onto every `opensearch` output it assembles, replacing
//! the package's connection keys. The package keeps what is its to say
//! (index, credentials reference, type suppression, tracing); the endpoint
//! says where. The closed token set is not widened for this (ADR-039): a
//! `{os_port}` would have to be added to every signed package, and would
//! still leave `tls` and `Path` to be written by hand.

use serde::{Deserialize, Serialize};

/// A parsed OpenSearch address. `host` is a bare name or IP (IPv6 without
/// the brackets); `path` is a prefix without the trailing slash, or absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsAddress {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub path: Option<String>,
}

/// The port a bare `host` means: OpenSearch's own, and what every package
/// template fixes — so an address typed as before changes nothing.
pub const DEFAULT_PORT: u16 = 9200;

impl OsAddress {
    /// Accept the three spellings an operator reaches for, and nothing
    /// ambiguous:
    ///
    /// - `host` — port 9200, TLS on (the package default, unchanged);
    /// - `host:port` — that port, TLS on;
    /// - `http://host[:port][/path]` / `https://…` — the scheme decides
    ///   TLS, the port defaults to 80/443 like a browser's, and a path is
    ///   the proxy prefix Fluent Bit's `Path` takes.
    ///
    /// Refused: another scheme, userinfo, a query or fragment, whitespace,
    /// an empty host, a port that is not 1..=65535. IPv6 literals go in
    /// brackets (`[::1]:9200`).
    pub fn parse(input: &str) -> Result<OsAddress, String> {
        let s = input.trim();
        if s.is_empty() {
            return Err("the address is empty".into());
        }
        if s.chars().any(char::is_whitespace) {
            return Err("the address contains whitespace".into());
        }
        let (tls, default_port, rest) = match s.split_once("://") {
            Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
                "http" => (false, 80, rest),
                "https" => (true, 443, rest),
                other => {
                    return Err(format!(
                        "scheme `{other}://` is not http or https (a bare host[:port] means https on 9200)"
                    ))
                }
            },
            None => (true, DEFAULT_PORT, s),
        };
        if rest.contains('?') || rest.contains('#') {
            return Err("the address must not carry a query or a fragment".into());
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], Some(&rest[i..])),
            None => (rest, None),
        };
        if authority.contains('@') {
            return Err(
                "credentials do not go in the address; the policy's account is used".into(),
            );
        }
        let (host, port_str) = if let Some(r) = authority.strip_prefix('[') {
            let (h, after) = r
                .split_once(']')
                .ok_or_else(|| "an IPv6 literal needs its closing bracket".to_string())?;
            match after {
                "" => (h, None),
                p => (
                    h,
                    Some(p.strip_prefix(':').ok_or_else(|| {
                        "after an IPv6 literal only `:port` may follow".to_string()
                    })?),
                ),
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h, Some(p)),
                Some(_) => {
                    return Err("an IPv6 literal goes in brackets: `[addr]:port`".into());
                }
                None => (authority, None),
            }
        };
        if host.is_empty() {
            return Err("the address names no host".into());
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'))
        {
            return Err(format!("host `{host}` has characters a host name cannot"));
        }
        let port = match port_str {
            None => default_port,
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("port `{p}` is not a number in 1..=65535"))?,
        };
        let path = path
            .map(|p| p.trim_end_matches('/'))
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        Ok(OsAddress {
            host: host.to_string(),
            port,
            tls,
            path,
        })
    }

    /// The address as a URL, for a screen or a log line.
    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{scheme}://{host}:{}{}",
            self.port,
            self.path.as_deref().unwrap_or("")
        )
    }
}

/// The connection keys the endpoint owns in an `[OUTPUT]` stanza of
/// `Name opensearch`. Everything else in the stanza is the package's.
const BOUND_KEYS: [&str; 4] = ["host", "port", "tls", "path"];

/// Bind `addr` onto every `opensearch` output of a classic-format Fluent Bit
/// body: the stanza's `Host`/`Port`/`tls`/`Path` lines are dropped wherever
/// they sit and the endpoint's are written right after its `Name` line, in
/// the stanza's own indentation. Other sections, other outputs, comments
/// and the stanza's remaining keys (`tls.verify`, `Index`, `HTTP_User`…)
/// pass through untouched, so the body stays diffable against the package.
///
/// Keys match case-insensitively, as Fluent Bit reads them. A body in the
/// engine's YAML format is not understood and is returned as it came — the
/// packages this crate renders are classic-format by contract.
pub fn bind_fluent_bit_output(body: &str, addr: &OsAddress) -> String {
    let lines: Vec<&str> = body.lines().collect();
    // Section ranges: a header line to the line before the next header.
    let mut sections: Vec<(usize, usize)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if let Some(last) = sections.last_mut() {
                last.1 = i;
            }
            sections.push((i, lines.len()));
        }
    }
    let key_of = |l: &str| -> Option<String> {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
            return None;
        }
        t.split_whitespace().next().map(str::to_ascii_lowercase)
    };
    let value_of = |l: &str| -> Option<String> {
        l.trim()
            .split_whitespace()
            .nth(1)
            .map(str::to_ascii_lowercase)
    };
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    let mut cursor = 0;
    for (start, end) in sections {
        // Lines before the first section (or between — there are none) pass.
        out.extend(lines[cursor..start].iter().map(|l| l.to_string()));
        cursor = end;
        let header = lines[start].trim();
        let is_os_output = header.eq_ignore_ascii_case("[OUTPUT]")
            && lines[start + 1..end].iter().any(|l| {
                key_of(l).as_deref() == Some("name") && value_of(l).as_deref() == Some("opensearch")
            });
        if !is_os_output {
            out.extend(lines[start..end].iter().map(|l| l.to_string()));
            continue;
        }
        out.push(lines[start].to_string());
        for l in &lines[start + 1..end] {
            match key_of(l) {
                Some(k) if BOUND_KEYS.contains(&k.as_str()) => continue,
                Some(k) if k == "name" => {
                    let indent: String = l.chars().take_while(|c| c.is_whitespace()).collect();
                    out.push(l.to_string());
                    out.push(format!("{indent}Host {}", addr.host));
                    out.push(format!("{indent}Port {}", addr.port));
                    out.push(format!(
                        "{indent}tls {}",
                        if addr.tls { "On" } else { "Off" }
                    ));
                    if let Some(p) = &addr.path {
                        out.push(format!("{indent}Path {p}"));
                    }
                }
                _ => out.push(l.to_string()),
            }
        }
    }
    out.extend(lines[cursor..].iter().map(|l| l.to_string()));
    let mut s = out.join("\n");
    if body.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> OsAddress {
        OsAddress::parse(s).unwrap()
    }

    /// The three spellings, and what each one means for the four keys.
    #[test]
    fn a_bare_host_keeps_the_package_defaults_and_a_url_decides_them() {
        assert_eq!(
            p("10.43.207.250"),
            OsAddress {
                host: "10.43.207.250".into(),
                port: 9200,
                tls: true,
                path: None
            }
        );
        assert_eq!(p("os.example.com:443").port, 443);
        assert!(p("os.example.com:443").tls);
        // The user's case: an ingress on 443, typed as host:port.
        let a = p("tornis-096o-opensearch.192.168.1.200.sslip.io:443");
        assert_eq!(
            (a.host.as_str(), a.port, a.tls),
            ("tornis-096o-opensearch.192.168.1.200.sslip.io", 443, true)
        );
        // …or as the URL, which is the same thing.
        assert_eq!(
            p("https://tornis-096o-opensearch.192.168.1.200.sslip.io"),
            a
        );
        let h = p("http://os.internal");
        assert_eq!((h.port, h.tls, h.path.as_deref()), (80, false, None));
        let h = p("http://os.internal:9201/");
        assert_eq!((h.port, h.tls, h.path.as_deref()), (9201, false, None));
        let h = p("https://proxy.example.com/opensearch/");
        assert_eq!(
            (h.port, h.tls, h.path.as_deref()),
            (443, true, Some("/opensearch"))
        );
        assert_eq!(h.url(), "https://proxy.example.com:443/opensearch");
        assert_eq!(p("HTTPS://X").tls, true);
        let v6 = p("[::1]:9200");
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 9200));
        assert_eq!(v6.url(), "https://[::1]:9200");
        assert_eq!(p(" os.example.com ").host, "os.example.com");
    }

    #[test]
    fn what_is_not_an_address_is_refused_with_the_reason() {
        let e = |s: &str| OsAddress::parse(s).unwrap_err();
        assert!(e("").contains("empty"));
        assert!(e("os example.com").contains("whitespace"));
        assert!(e("ftp://os.example.com").contains("http or https"));
        assert!(e("https://").contains("no host"));
        assert!(e("os.example.com:abc").contains("port"));
        assert!(e("os.example.com:0").contains("port"));
        assert!(e("os.example.com:70000").contains("port"));
        assert!(e("https://u:p@os.example.com").contains("credentials"));
        assert!(e("https://os.example.com/x?y=1").contains("query"));
        assert!(e("::1").contains("brackets"));
        assert!(e("[::1").contains("bracket"));
        assert!(e("os.example.com:9200:1").contains("brackets"));
        assert!(e("os,example.com").contains("characters"));
    }

    const BODY: &str = "[INPUT]\n    Name tail\n    Path /var/log/auth.log\n\n[OUTPUT]\n    Name opensearch\n    Match *\n    Host os.svc\n    Port 9200\n    HTTP_User ${VELOX_OS_USER}\n    tls On\n    tls.verify Off\n    Suppress_Type_Name On\n    Index veloxagent-ssh-logs-v1\n";

    /// The user's output: the host had `:443` glued on and the port stayed
    /// 9200. Bound, Fluent Bit gets the four keys it reads, and nothing else
    /// in the stanza — or outside it — moves. The INPUT's `Path` is a file
    /// path and must survive.
    #[test]
    fn binding_rewrites_only_the_connection_keys_of_the_opensearch_output() {
        let a = p("https://proxy.example.com/opensearch");
        let b = bind_fluent_bit_output(BODY, &a);
        assert_eq!(
            b,
            "[INPUT]\n    Name tail\n    Path /var/log/auth.log\n\n[OUTPUT]\n    Name opensearch\n    Host proxy.example.com\n    Port 443\n    tls On\n    Path /opensearch\n    Match *\n    HTTP_User ${VELOX_OS_USER}\n    tls.verify Off\n    Suppress_Type_Name On\n    Index veloxagent-ssh-logs-v1\n"
        );
        let plain = bind_fluent_bit_output(BODY, &p("http://10.0.0.5"));
        assert!(
            plain.contains("    Host 10.0.0.5\n    Port 80\n    tls Off\n"),
            "{plain}"
        );
        assert!(!plain.contains("Path /opensearch"));
        assert!(plain.contains("Path /var/log/auth.log"));
        // Idempotent: binding a bound body again changes nothing.
        assert_eq!(bind_fluent_bit_output(&b, &a), b);
    }

    /// Two outputs, only the OpenSearch one is touched; a stanza whose Name
    /// comes last is still found; keys match regardless of case.
    #[test]
    fn other_outputs_and_sections_pass_through() {
        let body = "[SERVICE]\n    Flush 5\n[OUTPUT]\n    Name stdout\n    Match *\n    Host nothing\n[OUTPUT]\n    host x:443\n    PORT 1\n    match *\n    NAME OpenSearch\n";
        let b = bind_fluent_bit_output(body, &p("os.example.com:443"));
        assert!(
            b.contains("[OUTPUT]\n    Name stdout\n    Match *\n    Host nothing\n"),
            "{b}"
        );
        assert!(b.ends_with("[OUTPUT]\n    match *\n    NAME OpenSearch\n    Host os.example.com\n    Port 443\n    tls On\n"), "{b}");
        // No section at all: unchanged.
        assert_eq!(bind_fluent_bit_output("# nothing", &p("h")), "# nothing");
    }
}
