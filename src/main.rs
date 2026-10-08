// SPDX-License-Identifier: Apache-2.0 OR MIT
// SPDX-FileCopyrightText: 2026 Denis Yermakou <connect@axonos.org>

//! The four organs, wired together, running one deterministic session.
//!
//! Until 0.3.0 this session went from the converter straight to the vault,
//! skipping every stage that turns a sample into evidence. A reference session
//! that references half the system is worth less than no reference at all,
//! because it looks like coverage. `axonos-signal-pipeline` now sits in the
//! middle: each frame is re-referenced against the common average, screened for
//! five independent artifact findings, and — once a window has accumulated —
//! reduced to narrowband power at the paradigm's target frequencies.
//!
//! Each organ is tested against the one below it. Nothing tested the chain —
//! and a chain is where the interesting failures live, because each component
//! is correct about its own contract and wrong about its neighbour's
//! assumptions. This binary is the chain, and its output is the artifact:
//!
//! ```text
//! cargo run --locked -- --seed 7 --frames 3000
//! ```
//!
//! Same seed, same transcript, byte for byte, on any machine. CI diffs the
//! output against `reference/session-7.txt`, so a change in any of the three
//! dependencies that alters observable behaviour fails the build with a diff
//! showing exactly what moved. That is a stronger statement than a checksum
//! over a binary: it says *what the system does*, not what it compiled to.
//!
//! It also completes a roadmap item that has been open since the ecosystem
//! table was written — "a deterministic simulator, so a developer can run the
//! full path without hardware". `SimDevice` was half of it. This is the rest.

use axonos_hal::{
    sim::{FaultProfile, SimDevice},
    AcqError, AcquisitionDevice, Frontend, TimingBudget, CHANNELS,
};
use axonos_pipeline_core::artifact::{artifact_screen, ArtifactReport, ScreenLimits};
use axonos_pipeline_core::spatial::{rereference, Reference};
use axonos_pipeline_core::spectral::{goertzel_coeff_q14, goertzel_power};
use axonos_supervisor::Supervisor;
use axonos_vault::{ContactQuality, Denial, Grant, Purpose, Vault, WINDOW};

/// Artifact limits for the canonical front end, in ADC counts.
///
/// The published defaults rather than numbers chosen to make this session look
/// good: a session that tunes its own thresholds demonstrates tuning.
const LIMITS: ScreenLimits = ScreenLimits::CANONICAL;

/// SSVEP-shaped targets, in millihertz. Four flicker rates is the shape a real
/// decision has, and the argmax is deliberately not taken here — choosing a
/// target is a decision, and decisions do not belong in a transport session.
const TARGETS_MILLI_HZ: [u32; 4] = [8_000, 10_000, 12_000, 15_000];

/// How often a quality reading is requested, in frames.
const RELEASE_EVERY: u64 = 250;

/// Samples kept for the spectral stage — one second at 250 SPS.
const RECENT: usize = 250;

fn main() {
    let mut seed = 7u64;
    let mut frames = 3_000u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(seed),
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    std::process::exit(run(seed, frames));
}

fn t(us: u64) -> String {
    format!("t={:>8.3}s", us as f64 / 1_000_000.0)
}

fn run(seed: u64, frames: u64) -> i32 {
    // The budget is closed before anything is configured. A rate whose deadline
    // the measured chain cannot meet has no TimingBudget to hand over, so the
    // session cannot start in an unmeetable configuration.
    let budget = match TimingBudget::canonical(250) {
        Ok(b) => b,
        Err(e) => {
            println!("FATAL  the canonical chain does not close: {e:?}");
            return 1;
        }
    };

    let mut dev = SimDevice::new(seed, FaultProfile::FIELD);
    if let Err(e) = dev.configure(budget, Frontend::CANONICAL) {
        println!("FATAL  device refused configuration: {e:?}");
        return 1;
    }

    let mut sup = Supervisor::default();
    let mut vault = Vault::new();

    // 2 048 bits: sixty-four single-scalar readings, which is every entry the
    // audit log can hold at the minimum charge. RFC-0009 N5 refuses anything
    // larger, because a budget that cannot be recorded is not the binding
    // constraint and no reader of the grant could tell.
    //
    // The return value is checked. An earlier revision of this session ignored
    // it, and when vault 0.2.0 began refusing the old 3 200-bit grant the
    // session ran to completion with no grant installed, reporting NoSuchGrant
    // for every reading — a silent misconfiguration wearing the appearance of
    // a working run. That is precisely the failure this repository exists to
    // catch, and it was caught by the repository catching it.
    if !vault.issue(Grant::new(1, Purpose::QualityFeedback, 2_048, u64::MAX)) {
        println!("FATAL  the vault refused the grant — budget exceeds recordable capacity");
        return 1;
    }

    println!("AxonOS reference session");
    println!(
        "  seed {seed} · {} SPS · {} µs period · {} frames · fault profile FIELD",
        budget.sps(),
        budget.period_ns() / 1_000,
        frames
    );
    println!(
        "  chain WCRT {} µs, {}.{}% of the period",
        budget.wcrt_ns() / 1_000,
        budget.utilisation_ppm() / 10_000,
        (budget.utilisation_ppm() / 1_000) % 10
    );
    println!("  grant 1 · QualityFeedback · 2048 bits (= log capacity × 32)");
    println!();

    let mut posture = sup.posture();
    println!("{}  posture {:?}", t(0), posture);

    let (mut delivered, mut lost, mut integrity, mut releases, mut refusals) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut screened, mut disqualified, mut band_reports) = (0u64, 0u64, 0u64);
    let mut seen_findings = 0u8;
    let mut recent = [0i32; RECENT];
    let mut recent_len = 0usize;
    let mut now_us = 0u64;

    for i in 0..frames {
        now_us = i * (budget.period_ns() / 1_000);
        match dev.read_frame() {
            Ok(mut f) => {
                delivered += 1;

                // Conditioning, in the order the contract declares. The
                // re-reference comes before anything reads an amplitude: a
                // common-mode offset screened as signal would be stored as
                // evidence of one, and nothing downstream could tell.
                if rereference(&mut f.codes, CHANNELS, Reference::CommonAverage).is_err() {
                    println!("{}  COND  re-reference refused a frame", t(now_us));
                }
                let report = artifact_screen(&f.codes, LIMITS).unwrap_or_default();
                if !report.is_clean() {
                    screened += 1;
                    if report.disqualifying() {
                        disqualified += 1;
                    }
                    // Each distinct finding set is announced once. Printing on
                    // every change would flood the transcript with one
                    // alternation and bury the sets that appear later.
                    if seen_findings & report.bits() != report.bits() {
                        seen_findings |= report.bits();
                        println!(
                            "{}  COND  findings {:05b}{}",
                            t(now_us),
                            report.bits(),
                            if report.disqualifying() {
                                "  [disqualifying]"
                            } else {
                                ""
                            }
                        );
                    }
                }

                // One channel's recent history, for the spectral stage. Fixed
                // size, overwritten in place: a spectrum needs a window, and a
                // window that grows is an allocation this crate does not make.
                recent[recent_len % RECENT] = f.codes[0];
                recent_len += 1;

                let p = sup.observe_frame(&f);
                vault.admit(f);
                if p != posture {
                    let tr = sup
                        .last_transition()
                        .expect("a change records a transition");
                    println!(
                        "{}  posture {:?} → {:?}   ({:?})",
                        t(now_us),
                        tr.from,
                        tr.to,
                        tr.cause
                    );
                    posture = p;
                }
            }
            Err(e) => {
                match e {
                    AcqError::Overrun { lost: n } => lost += n as u64,
                    AcqError::Integrity => integrity += 1,
                    _ => {}
                }
                println!("{}  ACQ  {:?}", t(now_us), e);
                let p = sup.observe_error(&e, now_us);
                if p != posture {
                    let tr = sup
                        .last_transition()
                        .expect("a change records a transition");
                    println!(
                        "{}  posture {:?} → {:?}   ({:?})",
                        t(now_us),
                        tr.from,
                        tr.to,
                        tr.cause
                    );
                    posture = p;
                }
            }
        }

        // Ask for a quality reading on a fixed cadence, but only once the vault
        // holds a full window — a reading over a partial window is a number
        // whose support the reader would have to think about.
        if i > 0 && i % RELEASE_EVERY == 0 && vault.sealed_len() == WINDOW {
            let caps = sup.capabilities();
            if !caps.may_classify {
                println!(
                    "{}  HOLD  posture {:?} forbids classification — no reading requested",
                    t(now_us),
                    posture
                );
                continue;
            }
            // Narrowband power at the paradigm's targets, computed on the
            // conditioned signal before anything leaves the boundary. Reported,
            // not decided on: the argmax is the caller's, because choosing a
            // target is a decision and a transport session must not make one.
            let mut bands = [0u64; 4];
            let mut have_bands = true;
            for (slot, &f_mhz) in bands.iter_mut().zip(TARGETS_MILLI_HZ.iter()) {
                match goertzel_coeff_q14(f_mhz, budget.sps()) {
                    Some(c) => match goertzel_power(&recent[..recent_len.min(RECENT)], c, 10) {
                        Ok(power) => *slot = power,
                        Err(_) => have_bands = false,
                    },
                    None => have_bands = false,
                }
            }
            if have_bands {
                band_reports += 1;
                println!(
                    "{}  BAND  8Hz {} · 10Hz {} · 12Hz {} · 15Hz {}",
                    t(now_us),
                    bands[0],
                    bands[1],
                    bands[2],
                    bands[3]
                );
            }

            let reduction = vault.reduce(ContactQuality::new());
            match vault.release(reduction, Purpose::QualityFeedback, 1, now_us) {
                Ok(d) => {
                    releases += 1;
                    println!(
                        "{}  READ  bad {} of {} frames · {} bits · {} left{}",
                        t(now_us),
                        d.reduction.values()[0],
                        d.reduction.support(),
                        d.reduction.cost_bits(),
                        d.remaining_bits,
                        if caps.signal_trustworthy {
                            ""
                        } else {
                            "  [untrusted]"
                        }
                    );
                }
                Err(Denial::BudgetExhausted {
                    needed_bits,
                    remaining_bits,
                }) => {
                    refusals += 1;
                    println!(
                        "{}  DENY  budget exhausted: needed {needed_bits}, {remaining_bits} left",
                        t(now_us)
                    );
                }
                Err(other) => {
                    refusals += 1;
                    println!("{}  DENY  {:?}", t(now_us), other);
                }
            }
        }
    }

    // A device that stops delivering raises no error at all, so the supervisor
    // is told the time explicitly at the end of the session.
    let final_posture = sup.tick(now_us);

    let diag = sup.diagnostics();
    let caps = final_posture.capabilities();
    println!();
    println!("── session summary ──");
    println!("  delivered {delivered} · lost {lost} · integrity failures {integrity}");
    println!("  conditioned {delivered} · screened {screened} · disqualifying {disqualified}");
    println!(
        "  band reports {band_reports} at {} targets",
        TARGETS_MILLI_HZ.len()
    );
    if seen_findings != 0 {
        // Name the findings rather than counting them. "1826 frames screened"
        // is a number; "slew, drift" is what the recording did, and that
        // difference is the reason the screener reports independent bits
        // instead of one severity.
        let all = ArtifactReport::from_bits(seen_findings);
        let mut names = [""; 5];
        let mut n = 0;
        for (bit, name) in [
            (ArtifactReport::SATURATED, "saturated"),
            (ArtifactReport::AMPLITUDE, "amplitude"),
            (ArtifactReport::SLEW, "slew"),
            (ArtifactReport::FLATLINE, "flatline"),
            (ArtifactReport::DRIFT, "drift"),
        ] {
            if all.has(bit) {
                names[n] = name;
                n += 1;
            }
        }
        println!("  findings seen: {}", names[..n].join(", "));
    }
    if screened * 2 > delivered {
        println!(
            "  NOTE  most frames tripped the slew limit. The simulator emits white\n\
             \x20       noise, whose consecutive samples are independent; a real\n\
             \x20       acquisition path is band-limited and does not step that far\n\
             \x20       between samples. The detector is right and the source is\n\
             \x20       unrealistic — the threshold is the published one and is not\n\
             \x20       tuned to make this session look better than it is."
        );
    }
    println!(
        "  lead-off frames {} · saturated frames {}",
        diag.lead_off_frames, diag.saturated_frames
    );
    println!(
        "  stream integrity {}.{}%",
        diag.integrity_ppm() / 10_000,
        (diag.integrity_ppm() / 1_000) % 10
    );
    println!(
        "  posture {:?} after {} transition(s)",
        final_posture,
        sup.transitions()
    );
    println!(
        "  capabilities: acquire={} classify={} actuate={} trustworthy={}",
        caps.may_acquire, caps.may_classify, caps.may_actuate, caps.signal_trustworthy
    );
    println!(
        "  disclosed {} bits over {releases} release(s) · {refusals} refused",
        vault.total_released_bits()
    );
    let (left, of) = vault
        .grant(1)
        .map(|g| (g.remaining_bits(), g.budget_bits))
        .unwrap_or((0, 0));
    println!(
        "  vault holds {} frames; grant 1 has {left} of {of} bits left",
        vault.sealed_len()
    );

    // The accounting identity the whole stack exists to keep. If this ever
    // fails, one of the three organs is lying about what it saw.
    let produced = delivered + lost;
    if diag.frames != delivered {
        println!(
            "  FAIL  supervisor counted {} frames, session delivered {delivered}",
            diag.frames
        );
        return 1;
    }
    if diag.frames_lost != lost {
        println!(
            "  FAIL  supervisor counted {} lost, session saw {lost}",
            diag.frames_lost
        );
        return 1;
    }
    println!("  accounting: {delivered} delivered + {lost} lost = {produced} produced ✓");
    0
}
