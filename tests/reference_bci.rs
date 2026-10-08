// SPDX-License-Identifier: Apache-2.0 OR MIT
// SPDX-FileCopyrightText: 2026 Denis Yermakou <connect@axonos.org>

//! The reference BCI, tested as the artifact: the real binary, its exit code
//! and its two committed transcripts.

use std::process::{Command, Output};

fn bci(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reference_bci"))
        .args(args)
        .output()
        .expect("the reference_bci binary must run")
}

fn verified(args: &[&str]) -> String {
    let out = bci(args);
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    assert_eq!(out.status.code(), Some(0), "not verified:\n{text}");
    text
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    let at = text.find(key).unwrap_or_else(|| panic!("{key} missing"));
    let rest = &text[at + key.len()..];
    rest.trim_start_matches([' ', ':', '"'])
        .split(['"', ',', '}', '\n'])
        .next()
        .unwrap_or("")
}

/// The value on the transcript line that begins with `label`.
fn line<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix(label))
        .map(|rest| rest.split_whitespace().next().unwrap_or(""))
        .unwrap_or_else(|| panic!("no line starting with {label:?}"))
}

#[test]
fn the_field_transcript_is_reproduced_exactly() {
    assert_eq!(
        verified(&[]),
        include_str!("../reference/reference-bci-7.txt"),
        "reference_bci no longer reproduces reference/reference-bci-7.txt. \
         Read the diff before regenerating it."
    );
}

#[test]
fn the_clean_transcript_is_reproduced_exactly() {
    assert_eq!(
        verified(&["--profile", "clean"]),
        include_str!("../reference/reference-bci-7-clean.txt"),
    );
}

#[test]
fn the_json_report_agrees_with_the_transcript() {
    let json = verified(&["--json"]);
    let text = include_str!("../reference/reference-bci-7.txt");
    assert_eq!(field(&json, "\"result\""), "VERIFIED");
    assert_eq!(field(&json, "\"post_withdrawal_leakage\""), "0");
    assert_eq!(field(&json, "\"replay_mismatches\""), "0");
    assert_eq!(field(&json, "\"timing_measured\""), "false");
    assert_eq!(
        field(&json, "\"trace_sha256\""),
        field(text, "trace SHA-256"),
        "the two outputs describe the same run"
    );
}

#[test]
fn a_long_session_with_an_exhausted_budget_still_leaks_nothing() {
    let t = verified(&["--frames", "100000", "--withdraw-at", "60000"]);
    assert_eq!(line(&t, "post-withdrawal leakage"), "0");
    assert_ne!(line(&t, "refused: budget exhausted"), "0");
    assert_ne!(line(&t, "refused: grant revoked"), "0");
}

#[test]
fn without_a_withdrawal_nothing_is_suppressed() {
    let t = verified(&["--withdraw-at", "never", "--frames", "3000"]);
    assert_eq!(line(&t, "suppressed by consent gate"), "0");
    assert_eq!(line(&t, "final"), "Granted");
    assert_eq!(line(&t, "refused: grant revoked"), "0");
}

#[test]
fn bad_arguments_exit_2_and_say_why() {
    for args in [
        &["--frames", "0"][..],
        &["--frames", "100", "--withdraw-at", "100"],
        &["--profile", "lab"],
        &["--bogus"],
    ] {
        let out = bci(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
        assert!(
            out.stdout.is_empty(),
            "nothing is reported for a run that did not happen"
        );
    }
}
