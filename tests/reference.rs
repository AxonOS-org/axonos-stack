//! The artifact, tested as the artifact.
//!
//! These run the real binary rather than re-implementing its wiring, because a
//! test that re-implements the thing it checks tends to re-implement the bug
//! too.

use std::process::Command;

fn session(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_session"))
        .args(args)
        .output()
        .expect("the session binary must run");
    assert!(
        out.status.success(),
        "session exited {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("transcript is UTF-8")
}

#[test]
fn the_reference_transcript_is_reproduced_exactly() {
    let produced = session(&["--seed", "7", "--frames", "3000"]);
    let golden = include_str!("../reference/session-7.txt");
    assert_eq!(
        produced, golden,
        "the session no longer reproduces reference/session-7.txt — a dependency \
         changed observable behaviour. Read the diff before regenerating it."
    );
}

#[test]
fn the_same_seed_is_the_same_session_twice() {
    assert_eq!(
        session(&["--seed", "42", "--frames", "800"]),
        session(&["--seed", "42", "--frames", "800"])
    );
}

#[test]
fn a_different_seed_is_a_different_session() {
    assert_ne!(
        session(&["--seed", "42", "--frames", "800"]),
        session(&["--seed", "43", "--frames", "800"])
    );
}

#[test]
fn the_accounting_identity_holds_across_seeds() {
    // delivered + lost = produced, for every seed. If this ever fails, one of
    // the three organs is lying about what it saw — which is the single
    // property the whole stack exists to keep.
    for seed in ["1", "7", "42", "999", "31337"] {
        let t = session(&["--seed", seed, "--frames", "1500"]);
        assert!(
            t.contains("produced ✓"),
            "accounting failed for seed {seed}:\n{}",
            t.lines().rev().take(6).collect::<Vec<_>>().join("\n")
        );
    }
}

#[test]
fn a_degraded_chain_stops_actuating_and_keeps_recording() {
    // The FIELD profile lifts an electrode and never restores it, so a long
    // enough session must end unable to act — and still able to record.
    let t = session(&["--seed", "7", "--frames", "3000"]);
    assert!(
        t.contains("actuate=false"),
        "a lifted electrode must stop actuation"
    );
    assert!(t.contains("acquire=true"), "recording must never stop");
}

#[test]
fn the_two_bounds_now_agree() {
    // This test records a defect and its closure.
    //
    // axonos-vault once issued a 3 200-bit grant against a 64-entry audit log,
    // so the effective ceiling was 2 048 bits and the figure written in the
    // grant was not the one that stopped it. Both behaviours were individually
    // correct and individually tested; only running the organs together
    // surfaced the disagreement, which is the argument for this repository
    // existing at all.
    //
    // RFC-0009 N5 closed it: a grant whose budget cannot be recorded is
    // refused at issue. Budget and log capacity now reach zero together.
    let t = session(&["--seed", "3", "--frames", "60000"]);
    let disclosed: u32 = t
        .lines()
        .find(|l| l.contains("disclosed"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .expect("the summary reports disclosed bits");
    assert!(
        t.contains("DENY  budget exhausted") || t.contains("DENY  LogFull"),
        "a long session must be refused and say by which bound"
    );
    assert_eq!(
        disclosed, 2_048,
        "64 releases of 32 bits: the two bounds now coincide"
    );
    assert!(
        t.contains("of 2048 bits left"),
        "the grant must advertise the figure it enforces"
    );
}
