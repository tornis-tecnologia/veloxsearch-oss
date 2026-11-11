// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! What the agent and its engine cost the host, reported on every check-in
//! the way Fleet shows it per agent: CPU as a percentage of one core over
//! the interval since the previous check-in, memory as resident bytes now.
//! Read from `/proc/<pid>/stat` (utime + stime, in clock ticks) and
//! `/proc/<pid>/statm` (resident pages); no crate, no sampling thread — the
//! poll loop already has the cadence, so the meter is two reads per round.
//!
//! The pair is the agent's own process plus the engine child it supervises,
//! because that is what "the agent" costs the operator; a host where the
//! engine runs hot shows it on the agent's row, which is where they look.
//! Outside Linux (`/proc` absent) nothing is reported rather than a zero
//! that would read as idle.

use std::time::Instant;

/// One process, one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sample {
    /// utime + stime, in `CLK_TCK` ticks.
    pub cpu_ticks: u64,
    pub rss_bytes: u64,
}

impl Sample {
    fn add(self, o: Sample) -> Sample {
        Sample {
            cpu_ticks: self.cpu_ticks + o.cpu_ticks,
            rss_bytes: self.rss_bytes + o.rss_bytes,
        }
    }
}

/// `/proc/<pid>/stat` and `statm` for one pid; `None` when either cannot be
/// read (not Linux, the process is gone, a hardened /proc).
pub fn sample(pid: u32) -> Option<Sample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let cpu_ticks = parse_stat_ticks(&stat)?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(Sample {
        cpu_ticks,
        rss_bytes: rss_pages * page_size(),
    })
}

/// utime (field 14) + stime (field 15). The comm field (2) is in parentheses
/// and may itself contain spaces or parentheses, so the fields are counted
/// from the LAST `)` rather than split from the start.
fn parse_stat_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let mut f = after.split_whitespace();
    // after `)` the next field is 3 (state); utime is 14, stime 15.
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some(utime + stime)
}

fn page_size() -> u64 {
    // SAFETY: sysconf is a plain libc query with no pointer arguments.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

fn clock_ticks_per_second() -> f64 {
    // SAFETY: as above.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as f64
    } else {
        100.0
    }
}

/// Turns two samples of the same process set into a CPU percentage: the
/// ticks spent between them over the wall time between them, as a share of
/// one core (so 200% is two cores, as `top` reads it).
#[derive(Debug, Default)]
pub struct Meter {
    last: Option<(Instant, u64)>,
}

/// What a round reports: `cpu_percent` is `None` on the first round (no
/// interval yet) and whenever a sample is missing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub cpu_percent: Option<f64>,
    pub rss_bytes: Option<u64>,
}

impl Meter {
    /// Sample the agent and, when it has one, the engine; fold them into one
    /// usage. A missing engine sample (it just exited) still reports the
    /// agent's own, with the CPU meter reset so the next interval is honest.
    pub fn round(&mut self, engine_pid: Option<u32>) -> Usage {
        let own = sample(std::process::id());
        let eng = engine_pid.map(sample);
        let total = match (own, eng) {
            (Some(a), Some(Some(e))) => Some(a.add(e)),
            (Some(a), None) => Some(a),
            (Some(a), Some(None)) => {
                self.last = None;
                Some(a)
            }
            (None, _) => None,
        };
        let Some(total) = total else {
            self.last = None;
            return Usage::default();
        };
        let now = Instant::now();
        let cpu_percent = self.last.and_then(|(t0, ticks0)| {
            let secs = now.duration_since(t0).as_secs_f64();
            let ticks = total.cpu_ticks.checked_sub(ticks0)?;
            (secs > 0.0).then(|| percent(ticks, secs))
        });
        self.last = Some((now, total.cpu_ticks));
        Usage {
            cpu_percent,
            rss_bytes: Some(total.rss_bytes),
        }
    }
}

fn percent(ticks: u64, secs: f64) -> f64 {
    let cpu_secs = ticks as f64 / clock_ticks_per_second();
    (100.0 * cpu_secs / secs * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's own line, with a comm that has a space AND a parenthesis
    /// in it — the thing a naive split-by-space gets wrong by one field.
    #[test]
    fn stat_ticks_are_counted_from_the_last_paren() {
        let line = "1234 (fluent bit (x)) S 1 1234 1234 0 -1 4194304 100 0 0 0 350 150 0 0 20 0 3 0 1000 10000000 500 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_stat_ticks(line), Some(500));
        assert_eq!(parse_stat_ticks("garbage"), None);
        assert_eq!(parse_stat_ticks("1 (x) S 1"), None);
    }

    /// Our own process always has a readable /proc on Linux; the first
    /// round reports memory and no CPU (no interval yet), the second both.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_meter_reports_memory_at_once_and_cpu_from_the_second_round() {
        let mut m = Meter::default();
        let first = m.round(None);
        assert!(first.rss_bytes.is_some_and(|b| b > 0));
        assert_eq!(first.cpu_percent, None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = m.round(None);
        assert!(second
            .cpu_percent
            .is_some_and(|p| (0.0..=10_000.0).contains(&p)));
        // A pid that cannot exist resets the meter but keeps the agent's own.
        let third = m.round(Some(u32::MAX));
        assert!(third.rss_bytes.is_some());
        assert_eq!(third.cpu_percent, None);
    }

    #[test]
    fn percent_is_one_decimal_of_one_core() {
        // 100 ticks at CLK_TCK over 1s on a 100 Hz kernel is one core.
        let p = percent(100, 1.0);
        assert!(
            (p - 100.0 * 100.0 / clock_ticks_per_second()).abs() < 0.11,
            "{p}"
        );
        assert_eq!(percent(0, 5.0), 0.0);
    }
}
