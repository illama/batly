//! LED lighting of Logitech devices, without I/O: effect tables, parameter
//! encoding, and the onboard profile sector (HID++ feature 0x8100) with
//! its safety checks. The protocol calls live in [`crate::hidpp`].
//!
//! Two HID++ features drive the LEDs:
//! * 0x8070 Color LED Effects (older G mice): zones with a list of effects,
//!   `setZoneEffect` once the host has taken control (`setSWControl`);
//! * 0x8071 RGB Effects (LIGHTSYNC mice and keyboards): same zone / effect
//!   model, but the host must claim the LEDs (`setSWControl(1, 3, flags)`)
//!   and give them back, or the firmware stops managing their power.
//!
//! Saving on the device writes the effect into the active onboard profile
//! (0x8100), the way G HUB's onboard memory manager does: the device then
//! plays it on its own, without any software. The profile layout comes from
//! Solaar (lighting at byte 208, four 11-byte entries, CRC-16/CCITT at the
//! end); it is only written when everything read matches that layout.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

pub const FEAT_COLOR_LED_EFFECTS: u16 = 0x8070;
pub const FEAT_RGB_EFFECTS: u16 = 0x8071;
pub const FEAT_ONBOARD_PROFILES: u16 = 0x8100;
/// Profile management of recent keyboards (format undocumented).
pub const FEAT_PROFILE_MANAGEMENT: u16 = 0x8101;

/// Effect ids (Solaar's `LEDEffects`).
pub const ID_DISABLED: u16 = 0x00;
pub const ID_STATIC: u16 = 0x01;
pub const ID_PULSE: u16 = 0x02;
pub const ID_CYCLE: u16 = 0x03;
pub const ID_BREATHE: u16 = 0x0A;
pub const ID_CYCLE_SAT: u16 = 0x15;

/// Lighting entries of a profile sector: offset, size, count (the first
/// two are the zones in normal use, the last two their power-saving
/// variants).
pub const PROFILE_LIGHTING_OFFSET: usize = 208;
pub const PROFILE_LIGHTING_ENTRY: usize = 11;
pub const PROFILE_LIGHTING_ENTRIES: usize = 4;
/// Zones that can be saved in a profile.
pub const PROFILE_ZONES: usize = 2;
/// Profile format of the G502 X family (libratbag: PROFILE_TYPE_G502X).
pub const FORMAT_G502X: u8 = 5;
/// G502 X format: index of a custom animation stored on the device, played
/// instead of the lighting entries when not 0 (libratbag clears it).
pub const ANIMATION_INDEX: usize = PROFILE_LIGHTING_OFFSET + PROFILE_LIGHTING_ENTRY * PROFILE_LIGHTING_ENTRIES;
/// Smallest sector holding the lighting entries and the CRC.
const MIN_SECTOR: usize = PROFILE_LIGHTING_OFFSET + PROFILE_LIGHTING_ENTRY * PROFILE_LIGHTING_ENTRIES + 2;
/// Largest sector we accept (sizes seen are 255 to 1024 bytes).
const MAX_SECTOR: usize = 4096;
/// Highest sector number of writable (flash) profiles; ROM ones start at 0x100.
const ROM_SECTORS: u16 = 0x100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedApi {
    /// 0x8070 Color LED Effects.
    Zones,
    /// 0x8071 RGB Effects.
    Rgb,
}

/// Effects offered in the interface; each maps to one of several wire ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    Off,
    Static,
    Breathe,
    Cycle,
}

impl EffectKind {
    pub const ALL: [Self; 4] = [Self::Off, Self::Static, Self::Breathe, Self::Cycle];

    /// Wire ids able to render this effect, preferred first.
    pub fn ids(self) -> &'static [u16] {
        match self {
            Self::Off => &[ID_DISABLED],
            Self::Static => &[ID_STATIC],
            Self::Breathe => &[ID_BREATHE, ID_PULSE],
            Self::Cycle => &[ID_CYCLE, ID_CYCLE_SAT],
        }
    }

    pub fn has_color(self) -> bool {
        matches!(self, Self::Static | Self::Breathe)
    }

    pub fn has_speed(self) -> bool {
        matches!(self, Self::Breathe | Self::Cycle)
    }

    fn of_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.ids().contains(&id))
    }
}

/// An effect with its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedEffect {
    pub kind: EffectKind,
    /// 0xRRGGBB.
    pub color: u32,
    /// Duration of one breath / color cycle.
    pub period_ms: u16,
    /// Brightness, 1..=100 %.
    pub intensity: u8,
}

impl Default for LedEffect {
    fn default() -> Self {
        Self { kind: EffectKind::Static, color: 0x00A8FF, period_ms: 4000, intensity: 100 }
    }
}

pub const PERIOD_MIN_MS: u16 = 1000;
pub const PERIOD_MAX_MS: u16 = 20000;

impl LedEffect {
    /// The 10 parameter bytes of `id` (layout of Solaar's `LEDEffects`,
    /// big-endian multi-byte values).
    pub fn params(&self, id: u16) -> [u8; 10] {
        let mut p = [0u8; 10];
        let [_, r, g, b] = self.color.to_be_bytes();
        let [ph, pl] = self.period_ms.clamp(PERIOD_MIN_MS, PERIOD_MAX_MS).to_be_bytes();
        let intensity = self.intensity.clamp(1, 100);
        match id {
            ID_STATIC => p[..3].copy_from_slice(&[r, g, b]),
            ID_PULSE => {
                p[..3].copy_from_slice(&[r, g, b]);
                // Speed byte: faster for a shorter period.
                p[3] = (255 - (self.period_ms.clamp(PERIOD_MIN_MS, PERIOD_MAX_MS) - PERIOD_MIN_MS) / 75) as u8;
            }
            ID_CYCLE => {
                p[5..7].copy_from_slice(&[ph, pl]);
                p[7] = intensity;
            }
            ID_BREATHE => {
                p[..3].copy_from_slice(&[r, g, b]);
                p[3..5].copy_from_slice(&[ph, pl]);
                p[6] = intensity;
            }
            ID_CYCLE_SAT => {
                p[1] = 0xFF;
                p[6..8].copy_from_slice(&[ph, pl]);
                p[8] = intensity;
            }
            _ => {}
        }
        p
    }

    /// Reads an effect back from its id and parameter bytes.
    pub fn parse(id: u16, p: &[u8]) -> Option<Self> {
        let kind = EffectKind::of_id(id)?;
        if p.len() < 9 {
            return None;
        }
        let rgb = |i: usize| u32::from_be_bytes([0, p[i], p[i + 1], p[i + 2]]);
        let be = |i: usize| u16::from_be_bytes([p[i], p[i + 1]]);
        let d = Self::default();
        let (color, period_ms, intensity) = match id {
            ID_STATIC => (rgb(0), d.period_ms, 100),
            ID_PULSE => (rgb(0), PERIOD_MIN_MS + (255 - p[3] as u16) * 75, 100),
            ID_CYCLE => (d.color, be(5), p[7]),
            ID_BREATHE => (rgb(0), be(3), p[6]),
            ID_CYCLE_SAT => (d.color, be(6), p[8]),
            _ => (d.color, d.period_ms, 100),
        };
        let period_ms = if period_ms == 0 { d.period_ms } else { period_ms.clamp(PERIOD_MIN_MS, PERIOD_MAX_MS) };
        let intensity = if intensity == 0 { 100 } else { intensity.min(100) };
        Some(Self { kind, color, period_ms, intensity })
    }

    /// 11-byte onboard profile entry: effect id then parameters.
    pub fn profile_entry(&self, id: u16) -> [u8; PROFILE_LIGHTING_ENTRY] {
        let mut e = [0u8; PROFILE_LIGHTING_ENTRY];
        e[0] = id as u8;
        e[1..].copy_from_slice(&self.params(id));
        e
    }
}

/// One effect a zone can play: its index in the zone list and its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneEffect {
    pub index: u8,
    pub id: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LedZone {
    pub index: u8,
    /// Location code (1 primary, 2 logo, 3 left side…).
    pub location: u16,
    pub effects: Vec<ZoneEffect>,
    /// Effect stored in the active onboard profile, when known.
    pub current: Option<LedEffect>,
}

impl LedZone {
    /// Effects of the interface this zone can play.
    pub fn kinds(&self) -> Vec<EffectKind> {
        EffectKind::ALL.into_iter().filter(|k| self.wire(*k).is_some()).collect()
    }

    /// Wire effect used for `kind`.
    pub fn wire(&self, kind: EffectKind) -> Option<ZoneEffect> {
        kind.ids().iter().find_map(|id| self.effects.iter().find(|e| e.id == *id).copied())
    }
}

/// Whether the lighting can be saved on the device, and why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardStatus {
    /// The active profile can be rewritten.
    Writable { sector: u16, size: u16 },
    /// No onboard memory (feature 0x8100 absent).
    Absent,
    /// The device runs from host settings, not from a profile.
    HostMode,
    /// The active profile is a factory (read-only) one.
    ReadOnly,
    /// What was read does not match the known layout: no write.
    UnknownFormat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lighting {
    pub api: LedApi,
    pub zones: Vec<LedZone>,
    pub onboard: OnboardStatus,
    /// Zone and effect Batly keeps re-applying (no writable memory).
    pub kept: Option<(u8, LedEffect)>,
}

/// Location names, for the interface (English; translated by the caller).
pub fn location_key(location: u16) -> &'static str {
    match location {
        1 => "primary",
        2 => "logo",
        3 => "left",
        4 => "right",
        5 => "combined",
        6..=11 => "primary",
        _ => "other",
    }
}

// ------------------------------------------------------- onboard memory

/// Answer of 0x8100 getDescription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnboardDescription {
    pub memory_model: u8,
    pub profile_format: u8,
    pub profile_count: u8,
    pub buttons: u8,
    pub sectors: u8,
    pub sector_size: u16,
}

impl OnboardDescription {
    pub fn parse(p: &[u8]) -> Option<Self> {
        (p.len() >= 9).then(|| Self {
            memory_model: p[0],
            profile_format: p[1],
            profile_count: p[3],
            buttons: p[5],
            sectors: p[6],
            sector_size: u16::from_be_bytes([p[7], p[8]]),
        })
    }

    /// Layout understood here: memory model 1 and profile formats 1..=5,
    /// as Solaar requires, and a sector large enough for the lighting.
    pub fn supported(&self) -> bool {
        self.memory_model == 1 && (1..=5).contains(&self.profile_format) && (MIN_SECTOR..=MAX_SECTOR).contains(&(self.sector_size as usize))
    }

    /// Whether `sector` is a flash profile we may rewrite.
    pub fn writable_sector(&self, sector: u16) -> bool {
        sector > 0 && sector < ROM_SECTORS && sector < self.sectors as u16
    }
}

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF), as in the profile sectors.
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0xFFFF, |mut crc: u16, &b| {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
        crc
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// Wrong size for the description.
    Size,
    /// Stored CRC does not match: the layout or the read is wrong.
    Crc,
    /// A lighting entry holds an unknown effect: not the expected layout.
    Layout,
    /// Zone without a slot in the profile.
    Zone,
}

/// Checks a profile sector read from the device against the layout.
pub fn check_profile(sector: &[u8], size: usize) -> Result<(), ProfileError> {
    if sector.len() != size || !(MIN_SECTOR..=MAX_SECTOR).contains(&size) {
        return Err(ProfileError::Size);
    }
    let stored = u16::from_be_bytes([sector[size - 2], sector[size - 1]]);
    if crc16(&sector[..size - 2]) != stored {
        return Err(ProfileError::Crc);
    }
    for i in 0..PROFILE_LIGHTING_ENTRIES {
        let id = sector[PROFILE_LIGHTING_OFFSET + i * PROFILE_LIGHTING_ENTRY];
        // Known effect, or an unused (erased) entry.
        let known = matches!(id, 0x00..=0x04 | 0x08..=0x0B | 0x0E..=0x10 | 0x15..=0x17 | 0xFF);
        if !known {
            return Err(ProfileError::Layout);
        }
    }
    Ok(())
}

/// Effect stored in lighting entry `slot` of a checked profile sector.
pub fn profile_effect(sector: &[u8], slot: usize) -> Option<LedEffect> {
    if slot >= PROFILE_LIGHTING_ENTRIES {
        return None;
    }
    let at = PROFILE_LIGHTING_OFFSET + slot * PROFILE_LIGHTING_ENTRY;
    let e = sector.get(at..at + PROFILE_LIGHTING_ENTRY)?;
    LedEffect::parse(e[0] as u16, &e[1..])
}

/// Whether the G502 X format plays a custom animation instead of the
/// lighting entries.
fn plays_animation(sector: &[u8], format: u8) -> bool {
    format == FORMAT_G502X && sector.len() > ANIMATION_INDEX + 2 && sector[ANIMATION_INDEX] != 0
}

/// Effect the device shows for `zone` (of `zones`), as far as known: a
/// single-zone G502 X uses the second entry; a custom animation is not an
/// effect we can describe.
pub fn profile_current(sector: &[u8], zone: usize, zones: usize, format: u8) -> Option<LedEffect> {
    if zone >= PROFILE_ZONES || plays_animation(sector, format) {
        return None;
    }
    let slot = if zones == 1 && format == FORMAT_G502X { 1 } else { zone };
    profile_effect(sector, slot)
}

impl LedEffect {
    /// The same effect at half brightness, for the power-saving entries
    /// (factory profiles use half the intensity there).
    pub fn dimmed(&self) -> Self {
        let [_, r, g, b] = self.color.to_be_bytes();
        let color = u32::from_be_bytes([0, r / 2, g / 2, b / 2]);
        Self { color, intensity: (self.intensity / 2).max(1), ..*self }
    }
}

/// Profile entries to write for `effect` (wire id `id`) on `zone`, on a
/// device with `zones` LED zones: the zone's entry and its power-saving
/// twin (two slots further, dimmed). A device with a single zone gets both
/// normal entries, as its factory profile has them identical.
pub fn profile_writes(zone: usize, zones: usize, effect: &LedEffect, id: u16) -> Vec<(usize, [u8; PROFILE_LIGHTING_ENTRY])> {
    let slots: Vec<usize> = if zones == 1 { vec![0, 1] } else { vec![zone] };
    let dim = effect.dimmed();
    let mut writes: Vec<(usize, [u8; PROFILE_LIGHTING_ENTRY])> = slots.iter().map(|&s| (s, effect.profile_entry(id))).collect();
    writes.extend(slots.iter().map(|&s| (s + PROFILE_ZONES, dim.profile_entry(id))));
    writes
}

/// The sector with `entries` written (slot, entry) and the CRC updated.
/// The sector must pass [`check_profile`]; slots are 0..4. On the G502 X
/// format the custom animation is switched off, or it would keep playing
/// over the entries (as libratbag does).
pub fn patch_profile(sector: &[u8], entries: &[(usize, [u8; PROFILE_LIGHTING_ENTRY])], format: u8) -> Result<Vec<u8>, ProfileError> {
    check_profile(sector, sector.len())?;
    if entries.iter().any(|(slot, _)| *slot >= PROFILE_LIGHTING_ENTRIES) {
        return Err(ProfileError::Zone);
    }
    let mut out = sector.to_vec();
    for (slot, entry) in entries {
        let at = PROFILE_LIGHTING_OFFSET + slot * PROFILE_LIGHTING_ENTRY;
        out[at..at + PROFILE_LIGHTING_ENTRY].copy_from_slice(entry);
    }
    if plays_animation(&out, format) {
        out[ANIMATION_INDEX] = 0;
    }
    Ok(with_crc(out))
}

fn with_crc(mut out: Vec<u8>) -> Vec<u8> {
    let n = out.len();
    let crc = crc16(&out[..n - 2]).to_be_bytes();
    out[n - 2..].copy_from_slice(&crc);
    out
}

/// The current sector with the lighting part (entries and what follows
/// them up to the CRC: the G502 X animation index) taken from `backup`.
/// Both must pass [`check_profile`] and have the same size; the rest of
/// the current sector (DPI, buttons…) is kept.
pub fn restore_lighting(current: &[u8], backup: &[u8]) -> Result<Vec<u8>, ProfileError> {
    check_profile(current, current.len())?;
    check_profile(backup, current.len())?;
    let mut out = current.to_vec();
    let n = out.len();
    out[PROFILE_LIGHTING_OFFSET..n - 2].copy_from_slice(&backup[PROFILE_LIGHTING_OFFSET..n - 2]);
    Ok(with_crc(out))
}

// ------------------------------------------------- lighting kept by Batly

/// Effects Batly re-applies itself (devices without writable memory), by
/// device id: zone and effect. Stored as JSON next to the settings.
pub type Kept = BTreeMap<String, (u8, LedEffect)>;

fn kind_key(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Off => "off",
        EffectKind::Static => "static",
        EffectKind::Breathe => "breathe",
        EffectKind::Cycle => "cycle",
    }
}

pub fn kept_to_json(kept: &Kept) -> String {
    let map: serde_json::Map<String, serde_json::Value> = kept
        .iter()
        .map(|(id, (zone, e))| {
            let v = serde_json::json!({
                "zone": zone,
                "effect": kind_key(e.kind),
                "color": format!("{:06X}", e.color & 0xFF_FFFF),
                "period_ms": e.period_ms,
                "intensity": e.intensity,
            });
            (id.clone(), v)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}

/// Entries that do not parse are left out.
pub fn kept_from_json(text: &str) -> Kept {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text) else { return Kept::new() };
    map.into_iter()
        .filter_map(|(id, v)| {
            let kind = EffectKind::ALL.into_iter().find(|k| v["effect"].as_str() == Some(kind_key(*k)))?;
            let color = u32::from_str_radix(v["color"].as_str()?, 16).ok()? & 0xFF_FFFF;
            let period_ms = (v["period_ms"].as_u64()? as u16).clamp(PERIOD_MIN_MS, PERIOD_MAX_MS);
            let intensity = (v["intensity"].as_u64()? as u8).clamp(1, 100);
            let zone = u8::try_from(v["zone"].as_u64()?).ok()?;
            Some((id, (zone, LedEffect { kind, color, period_ms, intensity })))
        })
        .collect()
}

pub fn load_kept(path: &Path) -> Kept {
    std::fs::read_to_string(path).map(|t| kept_from_json(&t)).unwrap_or_default()
}

/// Atomic write (temporary file, then rename).
pub fn save_kept(path: &Path, kept: &Kept) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, kept_to_json(kept))?;
    std::fs::rename(tmp, path)
}

/// Hex dump, 16 bytes per line, for the diagnostic.
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut s = String::new();
    for (i, line) in bytes.chunks(16).enumerate() {
        let _ = write!(s, "  {:04X}:", i * 16);
        for b in line {
            let _ = write!(s, " {b:02X}");
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid 255-byte profile sector with lighting entries.
    pub fn sample_sector() -> Vec<u8> {
        let mut s = vec![0xFFu8; 255];
        s[..20].copy_from_slice(&[1, 2, 2, 0x20, 0x03, 0x40, 0x06, 0x80, 0x0C, 0, 0x19, 0, 0, 0xFF, 0, 0, 0, 0, 3, 0]);
        let logo = LedEffect { kind: EffectKind::Breathe, color: 0x00FF00, period_ms: 3000, intensity: 80 };
        let primary = LedEffect { kind: EffectKind::Static, color: 0xFF0000, ..LedEffect::default() };
        s[208..219].copy_from_slice(&primary.profile_entry(ID_STATIC));
        s[219..230].copy_from_slice(&logo.profile_entry(ID_BREATHE));
        s[230..241].copy_from_slice(&[0u8; 11]);
        s[241..252].copy_from_slice(&[0u8; 11]);
        let crc = crc16(&s[..253]).to_be_bytes();
        s[253..].copy_from_slice(&crc);
        s
    }

    #[test]
    fn crc_matches_ccitt_false() {
        // Standard check value of CRC-16/CCITT-FALSE.
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc16(&[]), 0xFFFF);
    }

    #[test]
    fn params_follow_solaar_layout() {
        let e = LedEffect { kind: EffectKind::Breathe, color: 0x112233, period_ms: 3000, intensity: 80 };
        assert_eq!(e.params(ID_STATIC), [0x11, 0x22, 0x33, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(e.params(ID_BREATHE), [0x11, 0x22, 0x33, 0x0B, 0xB8, 0, 80, 0, 0, 0]);
        assert_eq!(e.params(ID_CYCLE), [0, 0, 0, 0, 0, 0x0B, 0xB8, 80, 0, 0]);
        assert_eq!(e.params(ID_CYCLE_SAT), [0, 0xFF, 0, 0, 0, 0, 0x0B, 0xB8, 80, 0]);
        // Out-of-range values are clamped.
        let wild = LedEffect { period_ms: 60000, intensity: 0, ..e };
        assert_eq!(wild.params(ID_BREATHE)[3..7], [0x4E, 0x20, 0, 1]);
    }

    #[test]
    fn parse_round_trips() {
        for (kind, id) in [
            (EffectKind::Static, ID_STATIC),
            (EffectKind::Breathe, ID_BREATHE),
            (EffectKind::Cycle, ID_CYCLE),
            (EffectKind::Cycle, ID_CYCLE_SAT),
        ] {
            let e = LedEffect { kind, color: 0xABCDEF, period_ms: 5000, intensity: 60 };
            let back = LedEffect::parse(id, &e.params(id)).unwrap();
            assert_eq!(back.kind, kind);
            if kind.has_color() {
                assert_eq!(back.color, 0xABCDEF);
            }
            if kind.has_speed() {
                assert_eq!((back.period_ms, back.intensity), (5000, 60));
            }
        }
        assert_eq!(LedEffect::parse(ID_DISABLED, &[0; 10]).unwrap().kind, EffectKind::Off);
        assert!(LedEffect::parse(0x0B, &[0; 10]).is_none());
        assert!(LedEffect::parse(ID_STATIC, &[0; 4]).is_none());
    }

    #[test]
    fn zone_picks_supported_wire_effects() {
        let zone = LedZone {
            index: 0,
            location: 2,
            effects: vec![
                ZoneEffect { index: 0, id: ID_DISABLED },
                ZoneEffect { index: 1, id: ID_STATIC },
                ZoneEffect { index: 2, id: ID_PULSE },
                ZoneEffect { index: 3, id: ID_CYCLE_SAT },
            ],
            current: None,
        };
        assert_eq!(zone.kinds(), EffectKind::ALL);
        assert_eq!(zone.wire(EffectKind::Breathe), Some(ZoneEffect { index: 2, id: ID_PULSE }));
        assert_eq!(zone.wire(EffectKind::Cycle), Some(ZoneEffect { index: 3, id: ID_CYCLE_SAT }));
        let plain = LedZone { effects: vec![ZoneEffect { index: 0, id: ID_STATIC }], ..zone };
        assert_eq!(plain.kinds(), [EffectKind::Static]);
    }

    #[test]
    fn description_limits() {
        let d = OnboardDescription::parse(&[1, 3, 1, 5, 0, 16, 16, 0, 255, 2]).unwrap();
        assert_eq!((d.profile_count, d.buttons, d.sectors, d.sector_size), (5, 16, 16, 255));
        assert!(d.supported());
        assert!(d.writable_sector(1) && d.writable_sector(15));
        assert!(!d.writable_sector(0) && !d.writable_sector(16) && !d.writable_sector(0x101));
        assert!(!OnboardDescription { memory_model: 2, ..d }.supported());
        assert!(!OnboardDescription { profile_format: 6, ..d }.supported());
        assert!(!OnboardDescription { sector_size: 128, ..d }.supported());
        assert!(OnboardDescription::parse(&[1, 3, 1]).is_none());
    }

    #[test]
    fn profile_check_patch_and_read_back() {
        let s = sample_sector();
        assert_eq!(check_profile(&s, 255), Ok(()));
        assert_eq!(profile_effect(&s, 0).unwrap().color, 0xFF0000);
        let logo = profile_effect(&s, 1).unwrap();
        assert_eq!((logo.kind, logo.color, logo.period_ms, logo.intensity), (EffectKind::Breathe, 0x00FF00, 3000, 80));

        let cycle = LedEffect { kind: EffectKind::Cycle, period_ms: 8000, intensity: 50, ..LedEffect::default() };
        let patched = patch_profile(&s, &[(1, cycle.profile_entry(ID_CYCLE))], 3).unwrap();
        assert_eq!(check_profile(&patched, 255), Ok(()));
        assert_eq!(profile_effect(&patched, 1).unwrap().kind, EffectKind::Cycle);
        // Only the entry and the CRC changed.
        let diff: Vec<usize> = (0..255).filter(|&i| s[i] != patched[i]).collect();
        assert!(diff.iter().all(|&i| (219..230).contains(&i) || i >= 253), "{diff:?}");
        assert_eq!(patch_profile(&s, &[(4, cycle.profile_entry(ID_CYCLE))], 3), Err(ProfileError::Zone));
    }

    /// Lighting entries of the G502 X PLUS profile sent by a user: effect
    /// 0x04 at intensity 100 (normal) and 50 (power saving), then an extra
    /// byte before the CRC.
    fn g502_x_plus_sector() -> Vec<u8> {
        let mut s = vec![0xFFu8; 255];
        s[..4].copy_from_slice(&[0x01, 0x00, 0x00, 0x20]);
        for (i, intensity) in [0x64u8, 0x64, 0x32, 0x32].iter().enumerate() {
            let at = 208 + i * 11;
            s[at..at + 11].copy_from_slice(&[0x04, 0, 0, 0, 0, 0, 0, 0x88, 0x01, *intensity, 0x13]);
        }
        s[252] = 0x03;
        let crc = crc16(&s[..253]).to_be_bytes();
        s[253..].copy_from_slice(&crc);
        s
    }

    #[test]
    fn single_zone_mouse_gets_both_entries_and_dimmed_power_saving() {
        let s = g502_x_plus_sector();
        assert_eq!(check_profile(&s, 255), Ok(()));
        let red = LedEffect { kind: EffectKind::Static, color: 0xFF2000, ..LedEffect::default() };
        let writes = profile_writes(0, 1, &red, ID_STATIC);
        assert_eq!(writes.iter().map(|w| w.0).collect::<Vec<_>>(), [0, 1, 2, 3]);
        // The factory profile plays custom animation 3: no known effect.
        assert_eq!(profile_current(&s, 0, 1, FORMAT_G502X), None);
        let out = patch_profile(&s, &writes, FORMAT_G502X).unwrap();
        assert_eq!(check_profile(&out, 255), Ok(()));
        assert_eq!(out[208..219], [0x01, 0xFF, 0x20, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(out[219..230], out[208..219]);
        assert_eq!(out[230..241], [0x01, 0x7F, 0x10, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(out[241..252], out[230..241]);
        // The custom animation is switched off so the entries show.
        assert_eq!(out[252], 0);
        assert_eq!(profile_current(&out, 0, 1, FORMAT_G502X).unwrap().color, 0xFF2000);
        // Everything before the lighting is untouched.
        assert!((0..208).all(|i| out[i] == s[i]));
        // Other formats keep that byte.
        assert_eq!(patch_profile(&s, &writes, 3).unwrap()[252], 3);
        // Restoring brings back the factory lighting and animation only.
        let mut later = out.clone();
        later[3] = 0x40; // DPI changed since
        let later = with_crc(later);
        let restored = restore_lighting(&later, &s).unwrap();
        assert_eq!(restored[208..253], s[208..253]);
        assert_eq!(restored[3], 0x40);
        assert_eq!(check_profile(&restored, 255), Ok(()));
        assert_eq!(restore_lighting(&later, &s[..254]), Err(ProfileError::Size));
        // Two zones: only the zone and its power-saving twin.
        assert_eq!(profile_writes(1, 2, &red, ID_STATIC).iter().map(|w| w.0).collect::<Vec<_>>(), [1, 3]);
        let breathe = LedEffect { kind: EffectKind::Breathe, intensity: 80, ..red };
        assert_eq!(breathe.dimmed().intensity, 40);
    }

    #[test]
    fn profile_refused_when_layout_is_not_the_expected_one() {
        let s = sample_sector();
        let mut bad_crc = s.clone();
        bad_crc[100] ^= 1;
        assert_eq!(check_profile(&bad_crc, 255), Err(ProfileError::Crc));
        assert_eq!(patch_profile(&bad_crc, &[(0, [0; 11])], 3), Err(ProfileError::Crc));
        // Valid CRC but an unknown byte where an effect id should be.
        let mut odd = s.clone();
        odd[219] = 0x42;
        let crc = crc16(&odd[..253]).to_be_bytes();
        odd[253..].copy_from_slice(&crc);
        assert_eq!(check_profile(&odd, 255), Err(ProfileError::Layout));
        assert_eq!(check_profile(&s[..200], 200), Err(ProfileError::Size));
        assert_eq!(check_profile(&s, 256), Err(ProfileError::Size));
    }

    #[test]
    fn kept_round_trip() {
        let mut kept = Kept::new();
        kept.insert("K1".into(), (0, LedEffect { kind: EffectKind::Static, color: 0xFF8000, period_ms: 4000, intensity: 100 }));
        kept.insert("K2".into(), (1, LedEffect { kind: EffectKind::Cycle, color: 0x00A8FF, period_ms: 7000, intensity: 40 }));
        let text = kept_to_json(&kept);
        assert!(text.contains("\"color\": \"FF8000\""), "{text}");
        assert_eq!(kept_from_json(&text), kept);
        // Broken entries and files are ignored.
        let mixed =
            r#"{"A": {"zone": 0, "effect": "static", "color": "00FF00", "period_ms": 4000, "intensity": 100}, "B": {"effect": "laser"}}"#;
        assert_eq!(kept_from_json(mixed).len(), 1);
        assert!(kept_from_json("not json").is_empty());
        let dir = std::env::temp_dir().join(format!("batly-kept-{}", std::process::id()));
        let path = dir.join("lighting.json");
        save_kept(&path, &kept).unwrap();
        assert_eq!(load_kept(&path), kept);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_dump_lines() {
        assert_eq!(hex_dump(&[0xAB; 17]), format!("  0000:{}\n  0010: AB\n", " AB".repeat(16)));
    }
}
