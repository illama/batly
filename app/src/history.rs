//! Battery history: every reading is appended to a small CSV file per
//! device, and the statistics derived from it (time left, time to full,
//! average charge duration, charge cycles, battery wear).
//!
//! Files live in `<settings dir>/history/`, one line per change:
//! `unix_seconds,percent,status,millivolts`.

use crate::model::{Device, Status};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const HOUR: f64 = 3600.0;
const DAY: u64 = 86_400;
/// A reading is written at least this often while nothing changes, so the
/// file also tells when the device was alive.
const HEARTBEAT: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Unix time, seconds.
    pub t: u64,
    pub percent: f32,
    pub status: Status,
    pub millivolts: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Average discharge speed, % per hour.
    pub drain_per_hour: Option<f64>,
    /// Estimated time until empty (when discharging).
    pub time_left_hours: Option<f64>,
    /// Estimated time until full (when charging).
    pub time_to_full_hours: Option<f64>,
    /// How long a full charge lasts at the usual pace.
    pub full_runtime_hours: Option<f64>,
    /// Average duration of a charge that ended full.
    pub avg_charge_hours: Option<f64>,
    /// Number of full-charge equivalents recorded.
    pub cycles: f64,
    /// Current full-charge runtime compared with the first weeks (100 = as new).
    pub health_percent: Option<f64>,
    /// Typical voltage when full, first weeks and lately (millivolts).
    pub full_mv_early: Option<u16>,
    pub full_mv_recent: Option<u16>,
    /// First reading (unix seconds).
    pub since: Option<u64>,
    pub samples: usize,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Thread-safe history store. With no directory it only keeps the readings
/// in memory (history disabled on disk, or tests).
pub struct History {
    dir: Option<PathBuf>,
    retention: u64,
    data: Mutex<HashMap<String, Vec<Sample>>>,
}

impl History {
    pub fn open(dir: Option<PathBuf>, retention_days: u32) -> Self {
        if let Some(d) = &dir {
            let _ = fs::create_dir_all(d);
        }
        Self { dir, retention: retention_days.max(1) as u64 * DAY, data: Mutex::new(HashMap::new()) }
    }

    fn file(&self, id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(file_name(id)))
    }

    /// Loads a device's file on first use; returns its samples.
    fn with_samples<R>(&self, id: &str, f: impl FnOnce(&mut Vec<Sample>, Option<PathBuf>) -> R) -> R {
        let mut data = self.data.lock().unwrap();
        let path = self.file(id);
        let samples = data.entry(id.to_string()).or_insert_with(|| {
            let Some(path) = &path else { return Vec::new() };
            let mut list = fs::read_to_string(path).map(|text| parse(&text)).unwrap_or_default();
            let total = list.len();
            let cutoff = now().saturating_sub(self.retention);
            list.retain(|s| s.t >= cutoff);
            if list.len() < total {
                // Compact the file once old readings were dropped.
                let _ = fs::write(path, serialize(id, &list));
            }
            list
        });
        f(samples, path)
    }

    /// Records the device's current state when it differs enough from the
    /// last reading. Returns true when a sample was added.
    pub fn record(&self, d: &Device) -> bool {
        self.record_at(d, now())
    }

    pub fn record_at(&self, d: &Device, t: u64) -> bool {
        let Some(b) = d.battery.filter(|_| d.online && d.has_battery) else { return false };
        let sample = Sample { t, percent: b.percent.clamp(0.0, 100.0), status: b.status, millivolts: b.millivolts };
        self.with_samples(&d.id, |samples, path| {
            if let Some(last) = samples.last() {
                let same = last.status == sample.status
                    && (last.percent - sample.percent).abs() < 1.0
                    && last.millivolts.zip(sample.millivolts).is_none_or(|(a, b)| a.abs_diff(b) < 30);
                if t < last.t || (same && t - last.t < HEARTBEAT) {
                    return false;
                }
            }
            if let Some(path) = path {
                let new_file = !path.exists();
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
                    if new_file {
                        let _ = f.write_all(header(&d.id).as_bytes());
                    }
                    let _ = f.write_all(line(&sample).as_bytes());
                }
            }
            samples.push(sample);
            true
        })
    }

    /// Readings at or after `since` (unix seconds).
    pub fn samples(&self, id: &str, since: u64) -> Vec<Sample> {
        self.with_samples(id, |s, _| {
            let start = s.partition_point(|x| x.t < since);
            s[start..].to_vec()
        })
    }

    pub fn stats(&self, id: &str) -> Stats {
        self.with_samples(id, |s, _| compute(s, now()))
    }

    /// Forgets a device (and deletes its file).
    pub fn forget(&self, id: &str) {
        self.data.lock().unwrap().remove(id);
        if let Some(path) = self.file(id) {
            let _ = fs::remove_file(path);
        }
    }
}

fn file_name(id: &str) -> String {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(48).collect();
    let mut hash: u32 = 0x811C_9DC5;
    for b in id.bytes() {
        hash = (hash ^ b as u32).wrapping_mul(0x0100_0193);
    }
    format!("{safe}-{hash:08x}.csv")
}

fn header(id: &str) -> String {
    format!("# Batly history of {id}\n# unix_seconds,percent,status,millivolts\n")
}

fn line(s: &Sample) -> String {
    format!("{},{:.0},{},{}\n", s.t, s.percent, s.status.as_str(), s.millivolts.map(|m| m.to_string()).unwrap_or_default())
}

fn serialize(id: &str, samples: &[Sample]) -> String {
    let mut out = header(id);
    for s in samples {
        out.push_str(&line(s));
    }
    out
}

fn parse(text: &str) -> Vec<Sample> {
    let mut out: Vec<Sample> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.trim().split(',');
            let t = f.next()?.parse().ok()?;
            let percent: f32 = f.next()?.parse().ok()?;
            let status = match f.next()? {
                "discharging" => Status::Discharging,
                "charging" => Status::Charging,
                "full" => Status::Full,
                "not_charging" => Status::NotCharging,
                _ => Status::Unknown,
            };
            let millivolts = f.next().and_then(|m| m.parse().ok());
            Some(Sample { t, percent: percent.clamp(0.0, 100.0), status, millivolts })
        })
        .collect();
    out.sort_by_key(|s| s.t);
    out
}

// ------------------------------------------------------------ statistics

fn plugged(s: Status) -> bool {
    matches!(s, Status::Charging | Status::Full)
}

/// A run of readings in the same direction.
#[derive(Debug)]
struct Segment {
    start: Sample,
    end: Sample,
}

impl Segment {
    fn hours(&self) -> f64 {
        (self.end.t.saturating_sub(self.start.t)) as f64 / HOUR
    }
    fn delta(&self) -> f64 {
        (self.end.percent - self.start.percent) as f64
    }
}

/// Splits the readings into discharge and charge segments. A discharge
/// segment ends when the device is plugged in or the level jumps up (it was
/// charged while Batly was not looking); a charge segment ends when the
/// device is unplugged or full.
fn segments(samples: &[Sample]) -> (Vec<Segment>, Vec<Segment>, Vec<(Segment, bool)>) {
    let mut discharge = Vec::new();
    let mut charge = Vec::new();
    // Charge sessions with "ended full".
    let mut sessions = Vec::new();
    let mut i = 0;
    while i < samples.len() {
        let first = samples[i];
        let mut j = i;
        if first.status == Status::Charging {
            while j + 1 < samples.len() && samples[j + 1].status == Status::Charging {
                j += 1;
            }
            let full = samples.get(j + 1).is_some_and(|n| n.status == Status::Full || n.percent >= 99.5);
            let end = if full { samples[j + 1] } else { samples[j] };
            charge.push(Segment { start: first, end: samples[j] });
            sessions.push((Segment { start: first, end }, full));
        } else if !plugged(first.status) {
            while j + 1 < samples.len()
                && !plugged(samples[j + 1].status)
                && samples[j + 1].percent <= samples[j].percent + 2.0
            {
                j += 1;
            }
            discharge.push(Segment { start: first, end: samples[j] });
        }
        i = j + 1;
    }
    (discharge, charge, sessions)
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// Drain rate (%/h) of the meaningful discharge segments.
fn drain_rates<'a>(segs: impl Iterator<Item = &'a Segment>) -> Vec<f64> {
    segs.filter(|s| s.hours() >= 6.0 && -s.delta() >= 5.0).map(|s| -s.delta() / s.hours()).collect()
}

pub fn compute(samples: &[Sample], now: u64) -> Stats {
    let mut st = Stats { since: samples.first().map(|s| s.t), samples: samples.len(), ..Default::default() };
    if samples.len() < 2 {
        return st;
    }
    let (discharge, charge, sessions) = segments(samples);
    let last = *samples.last().unwrap();

    // Cycles: every rise of the level adds up.
    st.cycles = samples.windows(2).map(|w| (w[1].percent - w[0].percent).max(0.0) as f64).sum::<f64>() / 100.0;

    // Usual pace: median of the recent finished segments.
    let finished = discharge.len().saturating_sub(if plugged(last.status) { 0 } else { 1 });
    let history_rate = median(drain_rates(discharge[..finished].iter().rev().take(5)));

    // Current segment: trust it in proportion to how much it has moved.
    let current = discharge.last().filter(|_| !plugged(last.status));
    let rate = match current {
        Some(seg) if seg.hours() >= 1.0 && -seg.delta() >= 2.0 => {
            let own = -seg.delta() / seg.hours();
            match history_rate {
                Some(h) => {
                    let w = (-seg.delta() / 15.0).clamp(0.2, 1.0);
                    Some(own * w + h * (1.0 - w))
                }
                None => Some(own),
            }
        }
        _ => history_rate,
    };
    st.drain_per_hour = rate.filter(|r| *r > 0.0);
    st.full_runtime_hours = st.drain_per_hour.map(|r| 100.0 / r);

    if !plugged(last.status) {
        if let Some(r) = st.drain_per_hour {
            // Time already spent since the last reading counts.
            let elapsed = now.saturating_sub(last.t) as f64 / HOUR;
            st.time_left_hours = Some((last.percent as f64 / r - elapsed).max(0.0));
        }
    }

    // Charging speed.
    let charge_rates: Vec<f64> =
        charge.iter().filter(|s| s.hours() >= 0.1 && s.delta() >= 3.0).map(|s| s.delta() / s.hours()).collect();
    if last.status == Status::Charging {
        if let Some(r) = median(charge_rates.iter().rev().take(5).copied().collect()) {
            let elapsed = now.saturating_sub(last.t) as f64 / HOUR;
            st.time_to_full_hours = Some(((100.0 - last.percent as f64) / r - elapsed).max(0.0));
        }
    }
    let full_sessions: Vec<f64> =
        sessions.iter().filter(|(s, full)| *full && s.hours() > 0.05 && s.delta() >= 20.0).map(|(s, _)| s.hours()).collect();
    if !full_sessions.is_empty() {
        st.avg_charge_hours = Some(full_sessions.iter().sum::<f64>() / full_sessions.len() as f64);
    }

    // Wear: compare the first 30 days with the last 30, given 60+ days.
    let first = samples[0].t;
    if last.t.saturating_sub(first) >= 60 * DAY {
        let early = median(drain_rates(discharge.iter().filter(|s| s.end.t < first + 30 * DAY)));
        let recent = median(drain_rates(discharge.iter().filter(|s| s.start.t > last.t - 30 * DAY)));
        if let (Some(e), Some(r)) = (early, recent) {
            st.health_percent = Some((e / r * 100.0).clamp(1.0, 100.0));
        }
        let full_mv = |range: &dyn Fn(u64) -> bool| {
            median(
                samples
                    .iter()
                    .filter(|s| s.percent >= 95.0 && range(s.t))
                    .filter_map(|s| s.millivolts.map(f64::from))
                    .collect(),
            )
            .map(|v| v as u16)
        };
        st.full_mv_early = full_mv(&|t| t < first + 30 * DAY);
        st.full_mv_recent = full_mv(&|t| t > last.t - 30 * DAY);
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DeviceKind, Source};

    fn s(hours: f64, percent: f32, status: Status) -> Sample {
        Sample { t: 1_700_000_000 + (hours * HOUR) as u64, percent, status, millivolts: None }
    }

    #[test]
    fn steady_discharge_gives_time_left() {
        // 2 %/h, 30 h so far.
        let v: Vec<Sample> = (0..=30).map(|h| s(h as f64, 100.0 - 2.0 * h as f32, Status::Discharging)).collect();
        let st = compute(&v, v.last().unwrap().t);
        assert!((st.drain_per_hour.unwrap() - 2.0).abs() < 0.01, "{st:?}");
        assert!((st.time_left_hours.unwrap() - 20.0).abs() < 0.1);
        assert!((st.full_runtime_hours.unwrap() - 50.0).abs() < 0.5);
        // Two hours later without a new reading: two hours less.
        let st = compute(&v, v.last().unwrap().t + 7200);
        assert!((st.time_left_hours.unwrap() - 18.0).abs() < 0.1);
        assert_eq!(st.time_to_full_hours, None);
    }

    #[test]
    fn charging_and_history_blend() {
        let mut v = Vec::new();
        // Past discharge at 1 %/h over 60 h, then a charge 40 → 100 in 2 h.
        for h in 0..=60 {
            v.push(s(h as f64, 100.0 - h as f32, Status::Discharging));
        }
        for k in 0..=4 {
            v.push(s(61.0 + k as f64 * 0.5, 40.0 + 15.0 * k as f32, Status::Charging));
        }
        v.push(s(63.1, 100.0, Status::Full));
        // New discharge, only 1 % down so far: history rate is used.
        v.push(s(64.0, 100.0, Status::Discharging));
        v.push(s(65.5, 99.0, Status::Discharging));
        let st = compute(&v, v.last().unwrap().t);
        assert!((st.drain_per_hour.unwrap() - 1.0).abs() < 0.01, "{st:?}");
        assert!((st.time_left_hours.unwrap() - 99.0).abs() < 0.5);
        let avg = st.avg_charge_hours.unwrap();
        assert!((avg - 2.1).abs() < 0.05, "{avg}");
        assert!((st.cycles - 0.6).abs() < 0.01, "{}", st.cycles);

        // While charging: time to full from the charge speed (30 %/h).
        let mut c = v.clone();
        c.push(s(70.0, 95.0, Status::Discharging));
        c.push(s(70.5, 70.0, Status::Charging));
        c.push(s(71.0, 85.0, Status::Charging));
        let st = compute(&c, c.last().unwrap().t);
        assert!((st.time_to_full_hours.unwrap() - 0.5).abs() < 0.02, "{st:?}");
        assert_eq!(st.time_left_hours, None);
    }

    #[test]
    fn wear_compares_first_and_last_month() {
        let mut v = Vec::new();
        // 90 days of 2-day cycles; the battery drains 25 % faster at the end.
        let mut t = 0.0;
        for cycle in 0..45 {
            let rate = if cycle < 15 { 2.0 } else if cycle < 30 { 2.2 } else { 2.5 };
            let mut p = 100.0f32;
            while p > 10.0 {
                v.push(Sample { millivolts: (p >= 95.0).then_some(if cycle < 15 { 4150 } else { 4050 }), ..s(t, p, Status::Discharging) });
                t += 1.0;
                p -= rate;
            }
            v.push(s(t, 50.0, Status::Charging));
            t = (cycle + 1) as f64 * 48.0;
            v.push(s(t - 0.5, 100.0, Status::Full));
        }
        let st = compute(&v, v.last().unwrap().t);
        let health = st.health_percent.unwrap();
        assert!((health - 80.0).abs() < 1.0, "{health}");
        assert_eq!((st.full_mv_early, st.full_mv_recent), (Some(4150), Some(4050)));
    }

    #[test]
    fn too_little_data() {
        assert_eq!(compute(&[], 0), Stats::default());
        let v = [s(0.0, 80.0, Status::Discharging), s(0.5, 80.0, Status::Discharging)];
        let st = compute(&v, v[1].t);
        assert_eq!((st.drain_per_hour, st.time_left_hours), (None, None));
    }

    #[test]
    fn store_on_disk() {
        let dir = std::env::temp_dir().join(format!("batly-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut d = Device {
            id: "dev/1 é".into(),
            name: "M".into(),
            kind: DeviceKind::Mouse,
            source: Source::Native,
            has_battery: true,
            battery: Some(Battery { percent: 80.0, status: Status::Discharging, millivolts: Some(3900), mileage: None }),
            online: true,
            last_update: None,
            info: Default::default(),
        };
        let t0 = now();
        let h = History::open(Some(dir.clone()), 365);
        assert!(h.record_at(&d, t0));
        assert!(!h.record_at(&d, t0 + 60), "same reading");
        d.battery.as_mut().unwrap().percent = 79.0;
        assert!(h.record_at(&d, t0 + 120));
        assert!(!h.record_at(&d, t0 + 60), "time going backwards");
        assert!(h.record_at(&d, t0 + 120 + HEARTBEAT), "heartbeat");
        d.online = false;
        assert!(!h.record_at(&d, t0 + 9999));

        let h2 = History::open(Some(dir.clone()), 365);
        let all = h2.samples("dev/1 é", 0);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].percent, all[0].millivolts, all[1].t), (80.0, Some(3900), t0 + 120));
        assert_eq!(h2.samples("dev/1 é", t0 + 100).len(), 2);
        h2.forget("dev/1 é");
        assert!(History::open(Some(dir.clone()), 365).samples("dev/1 é", 0).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_compacts_old_lines() {
        let dir = std::env::temp_dir().join(format!("batly-history-ret-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = now() - 400 * DAY;
        let text = format!("# x\n{},50,discharging,\n{},40,discharging,\nbad line\n", old, now() - DAY);
        fs::write(dir.join(file_name("x")), text).unwrap();
        let h = History::open(Some(dir.clone()), 365);
        assert_eq!(h.samples("x", 0).len(), 1);
        let content = fs::read_to_string(dir.join(file_name("x"))).unwrap();
        assert_eq!(content.lines().filter(|l| !l.starts_with('#')).count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
