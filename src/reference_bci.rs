// SPDX-License-Identifier: Apache-2.0 OR MIT
// SPDX-FileCopyrightText: 2026 Denis Yermakou <connect@axonos.org>

//! AxonOS Reference BCI: one chain from synthetic EEG to application intent,
//! with the consent boundary in the middle and a SHA-256 over everything that
//! happened.
//!
//! ```text
//! SimDevice ─→ pipeline ─→ supervisor ─┬─→ vault ──── grant 1 ─────────→ DERIVED ─┐
//! axonos-hal   re-reference  posture   │   sealed RAW, bits budget                ├─→ application
//!              screen                  └─→ decoder ─── consent gate ──→ INTENT ───┘   axonos-sdk types
//!              band power                              axonos-consent
//! ```
//!
//! Three kinds of data, kept apart:
//!
//! - **RAW** — sample frames. They are admitted to the vault's sealed window
//!   and go nowhere else. The application has no raw channel; that is a
//!   property of the wiring, not a count.
//! - **DERIVED** — a contact-quality reduction the vault releases under grant
//!   1, charged in bits and written to the vault's audit log.
//! - **INTENT** — an [`IntentObservation`], published only through the
//!   [`axonos_consent::PublicationGate`].
//!
//! A signed withdrawal from the simulated trusted path is handed to the
//! [`ConsentMachine`] at the chosen frame. From that frame on the gate refuses
//! every intent, and the vault grant is revoked so derived releases are refused
//! as `Revoked`. The verifier counts what the application received at or after
//! the withdrawal frame. That number has to be zero, and the run fails if it
//! is not.
//!
//! Nothing here is new machinery. The permission model is `axonos-consent`'s;
//! the disclosure model is `axonos-vault`'s; the application types are
//! `axonos-sdk`'s. What this file adds is the wiring between them — before it,
//! nothing in the organisation connected a consent withdrawal to a vault
//! grant — and a verifier that checks the wiring.
//!
//! ```text
//! cargo run --locked --release --bin reference_bci
//! ```
//!
//! # Trace format
//!
//! Every event is folded into the trace hash as
//! `frame u64 LE | tag u8 | len u16 LE | payload`. Tags:
//!
//! | tag  | event                        | payload                                              |
//! |:-----|:-----------------------------|:-----------------------------------------------------|
//! | 0x01 | RAW frame read               | seq u32 · t_us u64 · 8 × code i32 · lead_off u8 (45 B) |
//! | 0x02 | acquisition error            | `Debug` text of the `AcqError`                       |
//! | 0x03 | artifact findings            | finding bits u8                                      |
//! | 0x04 | posture transition           | `Debug` text of from, to and cause                   |
//! | 0x10 | consent frame handled        | the frame as received · `Debug` text of the result   |
//! | 0x11 | vault grant revoked          | grant id u16                                         |
//! | 0x20 | DERIVED released             | grant u16 · support u32 · bits u32 · values i32…     |
//! | 0x21 | DERIVED refused              | `Debug` text of the `Denial`                         |
//! | 0x30 | INTENT generated             | 28-byte intent record (below)                        |
//! | 0x31 | INTENT published             | gate index u32                                       |
//! | 0x32 | INTENT suppressed            | suppression ABI code u8                              |
//! | 0x33 | decision held                | none — posture forbids classification                |
//!
//! The intent record is a trace encoding, not the kernel ABI:
//! `timestamp_us u64 · kind u8 · value u8 · confidence Q0.16 u16 · session u64 · attestation 8 B`.
//!
//! The RAW hash covers only tag-0x01 payloads. The application hash covers
//! exactly what the application received, in order.

use std::fmt::Write as _;

use axonos_consent::wire::{assemble, ConsentRecord, FRAME_LEN};
use axonos_consent::{ConsentError, ConsentMachine, ConsentState, Ed25519Strict, Suppressed};
use axonos_hal::{
    sim::{FaultProfile, SimDevice},
    AcquisitionDevice, Frontend, SampleFrame, TimingBudget, CHANNELS,
};
use axonos_pipeline_core::artifact::{artifact_screen, ScreenLimits};
use axonos_pipeline_core::spatial::{rereference, Reference};
use axonos_pipeline_core::spectral::{goertzel_coeff_q14, goertzel_power};
use axonos_sdk::{
    Capability, Direction, IntentKind, IntentObservation, Manifest, MonotonicTimestamp, Quality,
};
use axonos_supervisor::Supervisor;
use axonos_vault::{ContactQuality, Denial, Disclosure, Grant, Purpose, Vault, WINDOW};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// Samples per second. The canonical operating point of `axonos-hal`.
const SPS: u32 = 250;

/// Artifact limits: the published defaults, not numbers chosen for this run.
const LIMITS: ScreenLimits = ScreenLimits::CANONICAL;

/// SSVEP-shaped targets in millihertz, one per direction.
const TARGETS_MILLI_HZ: [u32; 4] = [8_000, 10_000, 12_000, 15_000];
const DIRECTIONS: [Direction; 4] = [
    Direction::Up,
    Direction::Right,
    Direction::Down,
    Direction::Left,
];

/// One second of one channel, for the spectral stage.
const RECENT: usize = 250;

/// A decision every half second: 2 Hz.
const DECIDE_EVERY: u64 = 125;
const DECISION_RATE_HZ: u32 = SPS / DECIDE_EVERY as u32;

// The application declares SessionQuality, whose kernel limit is the lowest
// of the two it declares. The decision rate must fit under it, or the SDK
// refuses the manifest — checked here at compile time and there at run time.
const _: () = assert!(DECISION_RATE_HZ <= Capability::SessionQuality.kernel_rate_limit_hz());
const _: () = assert!(DECISION_RATE_HZ <= Capability::Navigation.kernel_rate_limit_hz());

/// A quality reading is requested once a second, as in the reference session.
const RELEASE_EVERY: u64 = 250;

/// A target must hold at least this share of the power in the four bands to
/// become a direction. Below it the decision is `Neutral`. Fixed, not tuned.
const DOMINANCE_PERCENT: u128 = 50;

const MANIFEST_ID: u16 = 1;
const GRANT: u16 = 1;
const GRANT_BITS: u32 = 2_048;
const APP_ID: &str = "org.axonos.reference-bci";

/// The canonical run: what `cargo run --bin reference_bci` does with no
/// arguments, and what `reference/reference-bci-7.txt` records.
const DEFAULT_SEED: u64 = 7;
const DEFAULT_FRAMES: u64 = 12_000;
const DEFAULT_WITHDRAW_AT: u64 = 9_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    /// `FaultProfile::FIELD`: overruns, corrupt frames, a lifted electrode.
    Field,
    /// `FaultProfile::CLEAN`: a device behaving perfectly.
    Clean,
}

impl Profile {
    fn faults(self) -> FaultProfile {
        match self {
            Self::Field => FaultProfile::FIELD,
            Self::Clean => FaultProfile::CLEAN,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Field => "FIELD",
            Self::Clean => "CLEAN",
        }
    }
}

/// A deliberate defect, used only by the tests to show that the verifier can
/// see a leak. Not reachable from the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum Mutation {
    None,
    /// Publish intents without asking the consent gate.
    BypassGate,
    /// Admit the withdrawal but leave the vault grant live.
    SkipRevoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Config {
    seed: u64,
    frames: u64,
    withdraw_at: Option<u64>,
    profile: Profile,
    mutation: Mutation,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            frames: DEFAULT_FRAMES,
            withdraw_at: Some(DEFAULT_WITHDRAW_AT),
            profile: Profile::Field,
            mutation: Mutation::None,
        }
    }
}

const USAGE: &str = "usage: reference_bci [--seed N] [--frames N] [--withdraw-at FRAME|never] \
                     [--profile field|clean] [--json]";

fn parse(args: &[String]) -> Result<(Config, bool), String> {
    let mut c = Config::default();
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--seed" => c.seed = value()?.parse().map_err(|_| "--seed: not a number")?,
            "--frames" => c.frames = value()?.parse().map_err(|_| "--frames: not a number")?,
            "--withdraw-at" => {
                let v = value()?;
                c.withdraw_at = if v == "never" {
                    None
                } else {
                    Some(v.parse().map_err(|_| "--withdraw-at: a frame or `never`")?)
                };
            }
            "--profile" => {
                c.profile = match value()?.as_str() {
                    "field" => Profile::Field,
                    "clean" => Profile::Clean,
                    _ => return Err("--profile: field or clean".into()),
                }
            }
            "--json" => json = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if c.frames == 0 {
        return Err("--frames must be at least 1".into());
    }
    if matches!(c.withdraw_at, Some(w) if w >= c.frames) {
        return Err("--withdraw-at must fall inside the session, or be `never`".into());
    }
    Ok((c, json))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cfg, json) = match parse(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    // Deterministic replay is checked by doing it: the whole session runs a
    // second time from the same seed and every digest must match.
    let first = run(cfg);
    let second = run(cfg);
    let mismatches = replay_mismatches(&first, &second);
    let verified = first.verified() && mismatches == 0;
    if json {
        print!("{}", render_json(&first, mismatches, verified));
    } else {
        print!("{}", render_text(&first, mismatches, verified));
    }
    std::process::exit(if verified { 0 } else { 1 });
}

/// What one session produced. Every field is a count or a digest; nothing in
/// it depends on the clock of the machine that ran it.
#[derive(Debug, Default)]
struct Report {
    seed: u64,
    frames: u64,
    profile: &'static str,
    withdraw_at: Option<u64>,
    period_us: u64,
    wcrt_us: u64,
    utilisation_ppm: u64,

    // input
    delivered: u64,
    lost: u64,
    integrity: u64,
    supervisor_frames: u64,
    supervisor_lost: u64,
    screened: u64,
    disqualifying: u64,
    posture_final: String,
    transitions: u32,

    // RAW
    sealed: usize,

    // DERIVED
    derived_released: u64,
    derived_bits: u64,
    derived_refused_revoked: u64,
    derived_refused_budget: u64,
    derived_refused_other: u64,
    vault_released_bits: u64,
    grant_revoked: bool,

    // INTENT
    decisions: u64,
    held: u64,
    generated: u64,
    generated_direction: u64,
    generated_neutral: u64,
    generated_quality: u64,
    capability_refused: u64,
    suppressed: u64,
    gate_published: u64,
    app: AppView,

    // PERMISSION
    consent_final: String,
    consent_admitted: u64,
    consent_refused: u64,
    consent_unexpected: u64,

    // verifier
    leaked_intents: u64,
    leaked_derived: u64,

    events: String,
    raw_sha256: [u8; 32],
    app_sha256: [u8; 32],
    trace_sha256: [u8; 32],
}

/// What the application saw, from its side of the boundary.
#[derive(Debug, Default)]
struct AppView {
    intents: u64,
    directions: u64,
    qualities: u64,
    derived: u64,
    derived_bits: u64,
    /// Frame at which the application received its first terminal error.
    closed_at: Option<u64>,
}

impl Report {
    fn accounting_holds(&self) -> bool {
        self.supervisor_frames == self.delivered && self.supervisor_lost == self.lost
    }
    fn gate_matches_app(&self) -> bool {
        self.gate_published == self.app.intents
    }
    fn vault_matches_app(&self) -> bool {
        self.vault_released_bits == self.app.derived_bits
    }
    fn leakage(&self) -> u64 {
        self.leaked_intents + self.leaked_derived
    }
    fn withdrawal_took_effect(&self) -> bool {
        match self.withdraw_at {
            Some(_) => self.consent_final == "Withdrawn" && self.grant_revoked,
            None => self.consent_final == "Granted",
        }
    }
    fn verified(&self) -> bool {
        self.accounting_holds()
            && self.gate_matches_app()
            && self.vault_matches_app()
            && self.leakage() == 0
            && self.consent_unexpected == 0
            && self.withdrawal_took_effect()
    }
}

fn replay_mismatches(a: &Report, b: &Report) -> u32 {
    u32::from(a.trace_sha256 != b.trace_sha256)
        + u32::from(a.raw_sha256 != b.raw_sha256)
        + u32::from(a.app_sha256 != b.app_sha256)
        + u32::from(a.events != b.events)
}

/// The trace: every event, in order, folded into one SHA-256.
struct Trace {
    all: Sha256,
    raw: Sha256,
}

impl Trace {
    fn new() -> Self {
        Self {
            all: Sha256::new(),
            raw: Sha256::new(),
        }
    }
    fn event(&mut self, frame: u64, tag: u8, payload: &[u8]) {
        self.all.update(frame.to_le_bytes());
        self.all.update([tag]);
        self.all.update((payload.len() as u16).to_le_bytes());
        self.all.update(payload);
    }
    fn raw(&mut self, frame: u64, f: &SampleFrame) {
        let mut b = [0u8; 45];
        b[0..4].copy_from_slice(&f.seq.to_le_bytes());
        b[4..12].copy_from_slice(&f.t_us.to_le_bytes());
        for (ch, code) in f.codes.iter().enumerate() {
            b[12 + ch * 4..16 + ch * 4].copy_from_slice(&code.to_le_bytes());
        }
        b[44] = f.lead_off.0;
        self.raw.update(b);
        self.event(frame, 0x01, &b);
    }
}

/// The trace encoding of an intent. Not the kernel ABI, which is not public
/// from the SDK; this records every field the SDK exposes.
fn intent_record(o: &IntentObservation) -> [u8; 28] {
    let (kind, value) = match o.kind() {
        IntentKind::Direction(d) => (1u8, d as u8),
        IntentKind::Load(l) => (2, l as u8),
        IntentKind::Quality(q) => (3, q as u8),
        IntentKind::Unknown => (0, 0),
    };
    let mut b = [0u8; 28];
    b[0..8].copy_from_slice(&o.timestamp_us().to_le_bytes());
    b[8] = kind;
    b[9] = value;
    b[10..12].copy_from_slice(&o.confidence_raw().to_le_bytes());
    b[12..20].copy_from_slice(&o.session_id().to_le_bytes());
    b[20..28].copy_from_slice(o.attestation());
    b
}

fn disclosure_record(d: &Disclosure) -> Vec<u8> {
    let mut b = Vec::with_capacity(10 + d.reduction.values().len() * 4);
    b.extend_from_slice(&d.grant.to_le_bytes());
    b.extend_from_slice(&d.reduction.support().to_le_bytes());
    b.extend_from_slice(&d.reduction.cost_bits().to_le_bytes());
    for v in d.reduction.values() {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

/// A key for the simulated trusted path, derived from the seed so the run is
/// reproducible. It is a demonstration key and is not secret.
fn demo_key(seed: u64, role: &[u8]) -> SigningKey {
    let mut h = Sha256::new();
    h.update(b"axonos-reference-bci demo key, not a secret");
    h.update(role);
    h.update(seed.to_le_bytes());
    let bytes: [u8; 32] = h.finalize().into();
    SigningKey::from_bytes(&bytes)
}

fn signed(key: &SigningKey, state: ConsentState, sequence: u64, t_us: u64) -> [u8; FRAME_LEN] {
    let record = ConsentRecord::new(state, MANIFEST_ID, sequence, t_us);
    assemble(&record, &key.sign(&record.encode()).to_bytes())
}

/// One frame from the trusted path (or from someone pretending to be it), and
/// what the consent machine must answer.
struct Probe {
    at: u64,
    what: &'static str,
    frame: Vec<u8>,
    expect: Result<ConsentState, ConsentError>,
}

fn schedule(
    cfg: &Config,
    trusted: &SigningKey,
    impostor: &SigningKey,
    period_us: u64,
) -> Vec<Probe> {
    let t = |frame: u64| frame * period_us;
    let mut p = Vec::new();
    // Before any withdrawal: input that must change nothing.
    let probe_at = cfg.withdraw_at.unwrap_or(cfg.frames) / 2;
    if cfg.withdraw_at.is_none_or(|w| w >= 2) {
        let mut truncated = signed(trusted, ConsentState::Withdrawn, 1, t(probe_at)).to_vec();
        truncated.pop();
        p.push(Probe {
            at: probe_at,
            what: "truncated frame (95 bytes)",
            frame: truncated,
            expect: Err(ConsentError::WireFormatLength),
        });
        p.push(Probe {
            at: probe_at,
            what: "withdrawal signed by an unknown key",
            frame: signed(impostor, ConsentState::Withdrawn, 1, t(probe_at)).to_vec(),
            expect: Err(ConsentError::SignatureInvalid),
        });
    }
    if let Some(w) = cfg.withdraw_at {
        let withdrawal = signed(trusted, ConsentState::Withdrawn, 1, t(w)).to_vec();
        p.push(Probe {
            at: w,
            what: "signed withdrawal, sequence 1",
            frame: withdrawal.clone(),
            expect: Ok(ConsentState::Withdrawn),
        });
        if w + 1 < cfg.frames {
            p.push(Probe {
                at: w + 1,
                what: "the same withdrawal again",
                frame: withdrawal,
                expect: Err(ConsentError::Replay),
            });
        }
        if w + 2 < cfg.frames {
            p.push(Probe {
                at: w + 2,
                what: "signed re-grant, sequence 2",
                frame: signed(trusted, ConsentState::Granted, 2, t(w + 2)).to_vec(),
                expect: Err(ConsentError::InadmissibleTransition),
            });
        }
    }
    p
}

fn t(us: u64) -> String {
    format!("t={:>8.3}s", us as f64 / 1_000_000.0)
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// Unroll the ring so the spectral stage sees samples in the order they
/// arrived. For these four targets a rotated window would give the same
/// magnitude — each is an exact bin of a one-second window — but a target
/// between bins would see a discontinuity where the ring wraps.
fn unrolled(ring: &[i32; RECENT], written: usize) -> [i32; RECENT] {
    let mut w = [0i32; RECENT];
    for (k, slot) in w.iter_mut().enumerate() {
        *slot = ring[(written + k) % RECENT];
    }
    w
}

/// Argmax over the four bands. Confidence is the winner's share of the total
/// power in Q0.16; below `DOMINANCE_PERCENT` the decision is `Neutral`.
fn decide(power: &[u64; 4]) -> (Direction, u16) {
    let sum: u128 = power.iter().map(|&p| u128::from(p)).sum();
    if sum == 0 {
        return (Direction::Neutral, 0);
    }
    let mut best = 0;
    for k in 1..4 {
        if power[k] > power[best] {
            best = k;
        }
    }
    let max = u128::from(power[best]);
    let confidence = (max * u128::from(u16::MAX) / sum) as u16;
    if max * 100 < sum * DOMINANCE_PERCENT {
        (Direction::Neutral, confidence)
    } else {
        (DIRECTIONS[best], confidence)
    }
}

fn run(cfg: Config) -> Report {
    let mut r = Report {
        seed: cfg.seed,
        frames: cfg.frames,
        profile: cfg.profile.name(),
        withdraw_at: cfg.withdraw_at,
        ..Report::default()
    };
    let mut log = String::new();
    let mut trace = Trace::new();
    let mut app_digest = Sha256::new();

    // The timing budget closes before anything is configured, exactly as in
    // the reference session. Failure here is a defect in a pinned organ.
    let budget = TimingBudget::canonical(SPS).expect("the canonical chain closes at 250 SPS");
    r.period_us = budget.period_ns() / 1_000;
    r.wcrt_us = budget.wcrt_ns() / 1_000;
    r.utilisation_ppm = budget.utilisation_ppm() as u64;

    let mut dev = SimDevice::new(cfg.seed, cfg.profile.faults());
    dev.configure(budget, Frontend::CANONICAL)
        .expect("the simulator accepts the canonical front end");
    let mut sup = Supervisor::default();
    let mut vault = Vault::new();
    assert!(
        vault.issue(Grant::new(
            GRANT,
            Purpose::QualityFeedback,
            GRANT_BITS,
            u64::MAX
        )),
        "the vault refused a grant within its recordable capacity"
    );

    let manifest = Manifest::builder()
        .app_id(APP_ID)
        .expect("a valid app id")
        .capability(Capability::Navigation)
        .capability(Capability::SessionQuality)
        .max_rate_hz(DECISION_RATE_HZ)
        .build()
        .expect("the manifest fits the kernel's rate limits");

    let trusted = demo_key(cfg.seed, b"trusted path");
    let impostor = demo_key(cfg.seed, b"impostor");
    let mut consent = ConsentMachine::new(
        MANIFEST_ID,
        trusted.verifying_key().to_bytes(),
        Ed25519Strict,
    )
    .expect("a valid trusted-path key");
    let probes = schedule(&cfg, &trusted, &impostor, r.period_us);
    let mut next_probe = 0;

    let _ = writeln!(
        log,
        "{}  CONSENT  {:?} — fresh installation, manifest {MANIFEST_ID}",
        t(0),
        consent.state()
    );

    let mut posture = sup.posture();
    let mut recent = [0i32; RECENT];
    let mut written = 0usize;
    let mut now_us = 0u64;
    let mut first_suppression_logged = false;
    let mut first_revoked_logged = false;

    for i in 0..cfg.frames {
        now_us = i * r.period_us;

        // The trusted path speaks before the frame is read. A withdrawal
        // admitted at frame W therefore governs frame W itself.
        while next_probe < probes.len() && probes[next_probe].at == i {
            let probe = &probes[next_probe];
            next_probe += 1;
            let got = consent.handle(&probe.frame);
            let mut fb = probe.frame.clone();
            fb.extend_from_slice(format!("{got:?}").as_bytes());
            trace.event(i, 0x10, &fb);
            match got {
                Ok(_) => r.consent_admitted += 1,
                Err(_) => r.consent_refused += 1,
            }
            if got != probe.expect {
                r.consent_unexpected += 1;
            }
            let verdict = match got {
                Ok(s) => format!("admitted → {s:?}"),
                Err(e) => format!("refused: {e:?}"),
            };
            let _ = writeln!(
                log,
                "{}  CONSENT  {} {} · state {:?}{}",
                t(now_us),
                probe.what,
                verdict,
                consent.state(),
                if got == probe.expect {
                    ""
                } else {
                    "  ✗ UNEXPECTED"
                }
            );
            if got == Ok(ConsentState::Withdrawn) && cfg.mutation != Mutation::SkipRevoke {
                // The step this file exists for: a consent withdrawal reaches
                // the vault in the same step, before any further release.
                r.grant_revoked = vault.revoke(GRANT);
                trace.event(i, 0x11, &GRANT.to_le_bytes());
                let _ = writeln!(log, "{}  VAULT    grant {GRANT} revoked", t(now_us));
            }
        }

        match dev.read_frame() {
            Ok(mut f) => {
                r.delivered += 1;
                trace.raw(i, &f);
                if rereference(&mut f.codes, CHANNELS, Reference::CommonAverage).is_err() {
                    let _ = writeln!(log, "{}  COND     re-reference refused a frame", t(now_us));
                }
                let report = artifact_screen(&f.codes, LIMITS).unwrap_or_default();
                if !report.is_clean() {
                    r.screened += 1;
                    if report.disqualifying() {
                        r.disqualifying += 1;
                    }
                    trace.event(i, 0x03, &[report.bits()]);
                }
                recent[written % RECENT] = f.codes[0];
                written += 1;
                let p = sup.observe_frame(&f);
                vault.admit(f);
                if p != posture {
                    let tr = sup
                        .last_transition()
                        .expect("a change records a transition");
                    let text = format!("{:?} → {:?}   ({:?})", tr.from, tr.to, tr.cause);
                    trace.event(i, 0x04, text.as_bytes());
                    let _ = writeln!(log, "{}  POSTURE  {text}", t(now_us));
                    posture = p;
                }
            }
            Err(e) => {
                match e {
                    axonos_hal::AcqError::Overrun { lost } => r.lost += u64::from(lost),
                    axonos_hal::AcqError::Integrity => r.integrity += 1,
                    _ => {}
                }
                trace.event(i, 0x02, format!("{e:?}").as_bytes());
                let p = sup.observe_error(&e, now_us);
                if p != posture {
                    let tr = sup
                        .last_transition()
                        .expect("a change records a transition");
                    let text = format!("{:?} → {:?}   ({:?})", tr.from, tr.to, tr.cause);
                    trace.event(i, 0x04, text.as_bytes());
                    let _ = writeln!(log, "{}  POSTURE  {text}", t(now_us));
                    posture = p;
                }
            }
        }

        // ── INTENT ──────────────────────────────────────────────────────────
        if i > 0 && i % DECIDE_EVERY == 0 && written >= RECENT {
            r.decisions += 1;
            let caps = sup.capabilities();
            if !caps.may_classify {
                r.held += 1;
                trace.event(i, 0x33, &[]);
            } else {
                let window = unrolled(&recent, written);
                let mut power = [0u64; 4];
                for (slot, &f_mhz) in power.iter_mut().zip(TARGETS_MILLI_HZ.iter()) {
                    *slot = goertzel_coeff_q14(f_mhz, SPS)
                        .and_then(|c| goertzel_power(&window, c, 10).ok())
                        .unwrap_or(0);
                }
                let ts = MonotonicTimestamp::from_micros_unchecked(now_us);
                // The kernel would attach a truncated HMAC here. It holds the
                // key; this session does not run the kernel, so the tag is
                // zero and says so rather than imitating one.
                let attestation = [0u8; 8];
                let (obs, cap) = if caps.signal_trustworthy {
                    let (dir, confidence) = decide(&power);
                    if dir == Direction::Neutral {
                        r.generated_neutral += 1;
                    } else {
                        r.generated_direction += 1;
                    }
                    (
                        IntentObservation::new_direction(
                            ts,
                            dir,
                            confidence,
                            cfg.seed,
                            attestation,
                        ),
                        Capability::Navigation,
                    )
                } else {
                    // An untrusted window does not become a guess. It becomes
                    // what the SDK has a kind for: a statement about quality.
                    r.generated_quality += 1;
                    (
                        IntentObservation::new_quality(ts, Quality::Low, cfg.seed, attestation),
                        Capability::SessionQuality,
                    )
                };
                r.generated += 1;
                let record = intent_record(&obs);
                trace.event(i, 0x30, &record);

                if !manifest.allows(cap) {
                    r.capability_refused += 1;
                } else {
                    let committed = match cfg.mutation {
                        Mutation::BypassGate => Ok(u32::MAX),
                        _ => consent.gate().try_publish(),
                    };
                    match committed {
                        Ok(index) => {
                            trace.event(i, 0x31, &index.to_le_bytes());
                            app_digest.update(record);
                            r.app.intents += 1;
                            match obs.kind() {
                                IntentKind::Quality(_) => r.app.qualities += 1,
                                _ => r.app.directions += 1,
                            }
                            if cfg.withdraw_at.is_some_and(|w| i >= w) {
                                r.leaked_intents += 1;
                            }
                        }
                        Err(why) => {
                            r.suppressed += 1;
                            trace.event(i, 0x32, &[why.abi_code()]);
                            // What the application is handed instead: the
                            // SDK's own error for this case, which is terminal.
                            let err = match why {
                                Suppressed::Suspended => axonos_sdk::Error::ConsentSuspended,
                                Suppressed::Withdrawn => axonos_sdk::Error::ConsentWithdrawn,
                            };
                            if !first_suppression_logged {
                                first_suppression_logged = true;
                                let _ = writeln!(
                                    log,
                                    "{}  GATE     intent suppressed ({why:?}); application receives {err:?}{}",
                                    t(now_us),
                                    if err.is_terminal() { ", terminal" } else { "" }
                                );
                            }
                            if err.is_terminal() && r.app.closed_at.is_none() {
                                r.app.closed_at = Some(i);
                            }
                        }
                    }
                }
            }
        }

        // ── DERIVED ─────────────────────────────────────────────────────────
        if i > 0
            && i % RELEASE_EVERY == 0
            && vault.sealed_len() == WINDOW
            && sup.capabilities().may_classify
        {
            let reduction = vault.reduce(ContactQuality::new());
            match vault.release(reduction, Purpose::QualityFeedback, GRANT, now_us) {
                Ok(d) => {
                    let rec = disclosure_record(&d);
                    trace.event(i, 0x20, &rec);
                    app_digest.update(&rec);
                    r.derived_released += 1;
                    r.app.derived += 1;
                    r.app.derived_bits += u64::from(d.reduction.cost_bits());
                    if cfg.withdraw_at.is_some_and(|w| i >= w) {
                        r.leaked_derived += 1;
                    }
                }
                Err(denial) => {
                    trace.event(i, 0x21, format!("{denial:?}").as_bytes());
                    match denial {
                        Denial::Revoked => {
                            r.derived_refused_revoked += 1;
                            if !first_revoked_logged {
                                first_revoked_logged = true;
                                let _ = writeln!(
                                    log,
                                    "{}  VAULT    derived release refused: Revoked",
                                    t(now_us)
                                );
                            }
                        }
                        Denial::BudgetExhausted { .. } => r.derived_refused_budget += 1,
                        _ => r.derived_refused_other += 1,
                    }
                }
            }
        }
    }

    let final_posture = sup.tick(now_us);
    let diag = sup.diagnostics();
    r.supervisor_frames = diag.frames;
    r.supervisor_lost = diag.frames_lost;
    r.posture_final = format!("{final_posture:?}");
    r.transitions = sup.transitions();
    r.sealed = vault.sealed_len();
    r.vault_released_bits = u64::from(vault.total_released_bits());
    r.gate_published = u64::from(consent.gate().published());
    r.consent_final = format!("{:?}", consent.state());
    r.derived_bits = r.app.derived_bits;
    r.events = log;
    r.raw_sha256 = trace.raw.finalize().into();
    r.app_sha256 = app_digest.finalize().into();
    r.trace_sha256 = trace.all.finalize().into();
    r
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "✓"
    } else {
        "✗"
    }
}

fn render_text(r: &Report, mismatches: u32, verified: bool) -> String {
    let mut s = String::new();
    let secs = r.frames * r.period_us;
    let _ = writeln!(s, "AXONOS REFERENCE BCI");
    let _ = writeln!(
        s,
        "  seed {} · {SPS} SPS · {} frames ({}.{:03} s) · fault profile {} · synthetic input",
        r.seed,
        r.frames,
        secs / 1_000_000,
        (secs / 1_000) % 1_000,
        r.profile
    );
    let _ = writeln!(
        s,
        "  application {APP_ID} · Navigation + SessionQuality · {DECISION_RATE_HZ} Hz"
    );
    let _ = writeln!(
        s,
        "  grant {GRANT} · QualityFeedback · {GRANT_BITS} bits · trusted-path key derived from the seed"
    );
    let _ = writeln!(s);
    s.push_str(&r.events);
    let _ = writeln!(s);

    let (w_frame, w_time) = match r.withdraw_at {
        Some(w) => (
            w.to_string(),
            format!(
                "   t={}.{:03} s",
                w * r.period_us / 1_000_000,
                (w * r.period_us / 1_000) % 1_000
            ),
        ),
        None => ("none".to_string(), String::new()),
    };
    let produced = r.delivered + r.lost;
    let _ = writeln!(s, "INPUT · axonos-hal SimDevice, synthetic");
    let _ = writeln!(s, "  reads                         {:>10}", r.frames);
    let _ = writeln!(s, "  frames delivered              {:>10}", r.delivered);
    let _ = writeln!(s, "  frames lost (sequence gaps)   {:>10}", r.lost);
    let _ = writeln!(s, "  reads failed integrity        {:>10}", r.integrity);
    let _ = writeln!(
        s,
        "  frames over an artifact limit {:>10}   (slew: the simulator emits white noise)",
        r.screened
    );
    let _ = writeln!(
        s,
        "  posture at the end            {:>10}   after {} transition(s)",
        r.posture_final, r.transitions
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "RAW NEURAL DATA · stays in axonos-vault");
    let _ = writeln!(s, "  frames hashed                 {:>10}", r.delivered);
    let _ = writeln!(s, "  sealed window at the end      {:>10}", r.sealed);
    let _ = writeln!(
        s,
        "  bytes to the application      {:>10}   (there is no raw channel)",
        0
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "DERIVED DATA · contact quality, released under grant {GRANT}"
    );
    let _ = writeln!(
        s,
        "  released                      {:>10}   {} bits",
        r.derived_released, r.derived_bits
    );
    let _ = writeln!(
        s,
        "  refused: grant revoked        {:>10}",
        r.derived_refused_revoked
    );
    let _ = writeln!(
        s,
        "  refused: budget exhausted     {:>10}",
        r.derived_refused_budget
    );
    let _ = writeln!(
        s,
        "  received at/after withdrawal  {:>10}",
        r.leaked_derived
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "APPLICATION INTENT · axonos-sdk IntentObservation");
    let _ = writeln!(
        s,
        "  decisions                     {:>10}   held by posture {}",
        r.decisions, r.held
    );
    let _ = writeln!(
        s,
        "  generated                     {:>10}   direction {} · neutral {} · quality {}",
        r.generated, r.generated_direction, r.generated_neutral, r.generated_quality
    );
    let _ = writeln!(s, "  delivered                     {:>10}", r.app.intents);
    let _ = writeln!(s, "  suppressed by consent gate    {:>10}", r.suppressed);
    let _ = writeln!(
        s,
        "  received at/after withdrawal  {:>10}",
        r.leaked_intents
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "PERMISSION · axonos-consent");
    let _ = writeln!(s, "  initial                       {:>10}", "Granted");
    let _ = writeln!(s, "  withdrawal at frame           {w_frame:>10}{w_time}");
    let _ = writeln!(
        s,
        "  consent frames admitted       {:>10}",
        r.consent_admitted
    );
    let _ = writeln!(
        s,
        "  consent frames refused        {:>10}   {} answered otherwise than specified",
        r.consent_refused, r.consent_unexpected
    );
    let _ = writeln!(s, "  final                         {:>10}", r.consent_final);
    let _ = writeln!(s, "  post-withdrawal leakage       {:>10}", r.leakage());
    let _ = writeln!(s);
    let _ = writeln!(s, "CHECKS");
    let _ = writeln!(
        s,
        "  {} accounting                      {} delivered + {} lost = {produced} produced, supervisor agrees",
        mark(r.accounting_holds()),
        r.delivered,
        r.lost
    );
    let _ = writeln!(
        s,
        "  {} gate count = intents received   {} = {}",
        mark(r.gate_matches_app()),
        r.gate_published,
        r.app.intents
    );
    let _ = writeln!(
        s,
        "  {} vault bits = bits received      {} = {}",
        mark(r.vault_matches_app()),
        r.vault_released_bits,
        r.app.derived_bits
    );
    let _ = writeln!(
        s,
        "  {} post-withdrawal leakage = 0",
        mark(r.leakage() == 0)
    );
    let _ = writeln!(
        s,
        "  {} every consent frame answered as the specification requires",
        mark(r.consent_unexpected == 0)
    );
    let _ = writeln!(
        s,
        "  {} deterministic replay            2 runs, {mismatches} mismatch(es)",
        mark(mismatches == 0)
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "EVIDENCE");
    let _ = writeln!(s, "  timing   not measured by this run.");
    let _ = writeln!(
        s,
        "           {} µs ({}.{}% of the period) is the response time axonos-hal admits against;",
        r.wcrt_us,
        r.utilisation_ppm / 10_000,
        (r.utilisation_ppm / 1_000) % 10
    );
    let _ = writeln!(
        s,
        "           RFC-0001 states it as L2 and its soak trace is not yet published."
    );
    let _ = writeln!(
        s,
        "  L1       none produced here. axonos-consent carries the Kani and loom"
    );
    let _ = writeln!(
        s,
        "           models for replay, absorbing withdrawal and the gate."
    );
    let _ = writeln!(
        s,
        "  L2       behaviour only: counts and digests on a host, from synthetic input."
    );
    let _ = writeln!(s, "  L3       none.");
    let _ = writeln!(s);
    let _ = writeln!(s, "raw SHA-256          {}", hex(&r.raw_sha256));
    let _ = writeln!(s, "application SHA-256  {}", hex(&r.app_sha256));
    let _ = writeln!(s, "trace SHA-256        {}", hex(&r.trace_sha256));
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "RESULT: {}",
        if verified { "VERIFIED" } else { "FAILED" }
    );
    s
}

fn render_json(r: &Report, mismatches: u32, verified: bool) -> String {
    let opt = |v: Option<u64>| v.map_or("null".to_string(), |x| x.to_string());
    let mut s = String::new();
    let _ = writeln!(s, "{{");
    let _ = writeln!(s, "  \"schema\": \"axonos-reference-bci/1\",");
    let _ = writeln!(
        s,
        "  \"run\": {{\"seed\": {}, \"frames\": {}, \"sps\": {SPS}, \"profile\": \"{}\", \"input\": \"synthetic\", \"withdraw_at\": {}}},",
        r.seed,
        r.frames,
        r.profile,
        opt(r.withdraw_at)
    );
    let _ = writeln!(
        s,
        "  \"input\": {{\"reads\": {}, \"delivered\": {}, \"lost\": {}, \"integrity_failures\": {}, \"artifact_screened\": {}, \"posture\": \"{}\", \"transitions\": {}}},",
        r.frames, r.delivered, r.lost, r.integrity, r.screened, r.posture_final, r.transitions
    );
    let _ = writeln!(
        s,
        "  \"raw\": {{\"frames_hashed\": {}, \"sealed\": {}, \"to_application_bytes\": 0, \"sha256\": \"{}\"}},",
        r.delivered,
        r.sealed,
        hex(&r.raw_sha256)
    );
    let _ = writeln!(
        s,
        "  \"derived\": {{\"released\": {}, \"bits\": {}, \"refused_revoked\": {}, \"refused_budget\": {}, \"refused_other\": {}, \"after_withdrawal\": {}}},",
        r.derived_released,
        r.derived_bits,
        r.derived_refused_revoked,
        r.derived_refused_budget,
        r.derived_refused_other,
        r.leaked_derived
    );
    let _ = writeln!(
        s,
        "  \"intent\": {{\"decisions\": {}, \"held\": {}, \"generated\": {}, \"direction\": {}, \"neutral\": {}, \"quality\": {}, \"delivered\": {}, \"suppressed\": {}, \"after_withdrawal\": {}, \"application_closed_at\": {}}},",
        r.decisions,
        r.held,
        r.generated,
        r.generated_direction,
        r.generated_neutral,
        r.generated_quality,
        r.app.intents,
        r.suppressed,
        r.leaked_intents,
        opt(r.app.closed_at)
    );
    let _ = writeln!(
        s,
        "  \"consent\": {{\"initial\": \"Granted\", \"withdrawal_frame\": {}, \"admitted\": {}, \"refused\": {}, \"unexpected\": {}, \"final\": \"{}\", \"grant_revoked\": {}}},",
        opt(r.withdraw_at),
        r.consent_admitted,
        r.consent_refused,
        r.consent_unexpected,
        r.consent_final,
        r.grant_revoked
    );
    let _ = writeln!(
        s,
        "  \"checks\": {{\"accounting\": {}, \"gate_matches_application\": {}, \"vault_matches_application\": {}, \"post_withdrawal_leakage\": {}, \"replay_mismatches\": {mismatches}}},",
        r.accounting_holds(),
        r.gate_matches_app(),
        r.vault_matches_app(),
        r.leakage()
    );
    let _ = writeln!(
        s,
        "  \"evidence\": {{\"timing_measured\": false, \"wcrt_us_admitted_against\": {}, \"l1\": \"none produced here\", \"l2\": \"behaviour on a host, synthetic input\", \"l3\": \"none\"}},",
        r.wcrt_us
    );
    let _ = writeln!(s, "  \"application_sha256\": \"{}\",", hex(&r.app_sha256));
    let _ = writeln!(s, "  \"trace_sha256\": \"{}\",", hex(&r.trace_sha256));
    let _ = writeln!(
        s,
        "  \"result\": \"{}\"",
        if verified { "VERIFIED" } else { "FAILED" }
    );
    let _ = writeln!(s, "}}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(frames: u64, withdraw_at: Option<u64>) -> Config {
        Config {
            seed: 7,
            frames,
            withdraw_at,
            profile: Profile::Clean,
            mutation: Mutation::None,
        }
    }

    #[test]
    fn permission_granted_delivers_and_nothing_is_withheld() {
        let r = run(cfg(3_000, None));
        assert!(r.verified(), "{}", render_text(&r, 0, r.verified()));
        assert!(r.app.intents > 0, "a granted session delivers intents");
        assert!(r.app.derived > 0, "a granted session releases derived data");
        assert_eq!(r.suppressed, 0);
        assert_eq!(r.derived_refused_revoked, 0);
        assert_eq!(r.consent_final, "Granted");
    }

    #[test]
    fn permission_denied_from_the_first_frame_delivers_nothing() {
        let r = run(cfg(3_000, Some(0)));
        assert!(r.verified());
        assert_eq!(r.app.intents, 0);
        assert_eq!(r.app.derived, 0);
        assert!(
            r.generated > 0,
            "intents are still generated inside the boundary"
        );
        assert_eq!(r.suppressed, r.generated);
    }

    #[test]
    fn withdrawal_stops_both_channels_at_the_frame_it_lands_on() {
        let r = run(cfg(6_000, Some(3_000)));
        assert!(r.verified());
        assert!(r.app.intents > 0 && r.suppressed > 0);
        assert!(r.derived_refused_revoked > 0);
        assert_eq!(r.leakage(), 0);
        assert_eq!(r.consent_final, "Withdrawn");
        assert!(r.grant_revoked);
        assert_eq!(
            r.app.closed_at,
            Some(3_000),
            "the first decision after W is refused"
        );
    }

    #[test]
    fn the_verifier_sees_a_gate_that_is_bypassed() {
        let mut c = cfg(6_000, Some(3_000));
        c.mutation = Mutation::BypassGate;
        let r = run(c);
        assert!(
            r.leaked_intents > 0,
            "a bypassed gate must show up as leakage"
        );
        assert!(!r.verified());
    }

    #[test]
    fn the_verifier_sees_a_grant_that_is_never_revoked() {
        let mut c = cfg(6_000, Some(3_000));
        c.mutation = Mutation::SkipRevoke;
        let r = run(c);
        assert!(r.leaked_derived > 0, "a live grant must show up as leakage");
        assert!(!r.verified());
    }

    #[test]
    fn malformed_and_forged_consent_frames_change_nothing() {
        let r = run(cfg(4_000, Some(2_000)));
        // truncated, unknown key, replay, re-grant: four refusals, one admission
        assert_eq!((r.consent_admitted, r.consent_refused), (1, 4));
        assert_eq!(r.consent_unexpected, 0);
        assert!(r
            .events
            .contains("truncated frame (95 bytes) refused: WireFormatLength · state Granted"));
        assert!(r
            .events
            .contains("unknown key refused: SignatureInvalid · state Granted"));
        assert!(r.events.contains("refused: Replay"));
        assert!(r
            .events
            .contains("refused: InadmissibleTransition · state Withdrawn"));
    }

    #[test]
    fn every_malformed_frame_shape_is_refused_by_the_machine() {
        let key = demo_key(1, b"trusted path");
        let good = signed(&key, ConsentState::Withdrawn, 1, 0);
        let mut m = ConsentMachine::new(MANIFEST_ID, key.verifying_key().to_bytes(), Ed25519Strict)
            .expect("key");
        let mut flipped_sig = good;
        flipped_sig[FRAME_LEN - 1] ^= 1;
        let mut flipped_body = good;
        flipped_body[8] ^= 1; // the sequence number, now unsigned
        let mut bad_magic = good;
        bad_magic[0] = b'X';
        for frame in [
            &good[..0],
            &good[..95],
            &flipped_sig[..],
            &flipped_body[..],
            &bad_magic[..],
        ] {
            assert!(m.handle(frame).is_err());
            assert_eq!(
                m.state(),
                ConsentState::Granted,
                "a refused frame changes nothing"
            );
        }
        assert_eq!(m.handle(&good), Ok(ConsentState::Withdrawn));
    }

    #[test]
    fn the_same_seed_is_the_same_trace() {
        let a = run(cfg(3_000, Some(1_500)));
        let b = run(cfg(3_000, Some(1_500)));
        assert_eq!(replay_mismatches(&a, &b), 0);
        assert_eq!(render_text(&a, 0, true), render_text(&b, 0, true));
    }

    #[test]
    fn the_hash_moves_with_the_seed_and_with_the_withdrawal() {
        let base = run(cfg(3_000, Some(1_500)));
        let mut other_seed = cfg(3_000, Some(1_500));
        other_seed.seed = 8;
        assert_ne!(base.trace_sha256, run(other_seed).trace_sha256);
        assert_ne!(base.trace_sha256, run(cfg(3_000, Some(1_501))).trace_sha256);
        // RAW does not depend on consent: the same frames were read.
        assert_eq!(base.raw_sha256, run(cfg(3_000, Some(1_501))).raw_sha256);
    }

    #[test]
    fn a_window_is_unrolled_oldest_first() {
        let mut ring = [0i32; RECENT];
        for n in 0..(RECENT + 3) {
            ring[n % RECENT] = n as i32;
        }
        let w = unrolled(&ring, RECENT + 3);
        assert_eq!(w[0], 3);
        assert_eq!(w[RECENT - 1], (RECENT + 2) as i32);
    }

    #[test]
    fn a_decision_needs_half_the_power() {
        assert_eq!(decide(&[10, 0, 0, 0]).0, Direction::Up);
        assert_eq!(decide(&[1, 1, 1, 1]).0, Direction::Neutral);
        assert_eq!(decide(&[0, 0, 0, 0]), (Direction::Neutral, 0));
        assert_eq!(decide(&[1, 1, 2, 0]).0, Direction::Down);
    }

    #[test]
    fn arguments_are_checked() {
        let a = |v: &[&str]| parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&[]).is_ok());
        assert!(a(&["--frames", "0"]).is_err());
        assert!(a(&["--frames", "100", "--withdraw-at", "100"]).is_err());
        assert!(a(&["--withdraw-at", "soon"]).is_err());
        assert!(a(&["--profile", "lab"]).is_err());
        assert!(a(&["--seed"]).is_err());
        assert_eq!(
            a(&["--withdraw-at", "never"]).map(|(c, _)| c.withdraw_at),
            Ok(None)
        );
    }
}
