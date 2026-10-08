# Changelog

All notable changes to axonos-stack are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] — 2026-10-08

### Added
- **The reference BCI** (`cargo run --locked --release --bin reference_bci`).
  One chain from synthetic EEG to application intent with the consent
  boundary in the middle: `axonos-hal` → `axonos-signal-pipeline` →
  `axonos-supervisor` → `axonos-vault` for derived data and `axonos-consent`'s
  publication gate for intents, delivered as `axonos-sdk` types. A signed
  withdrawal at frame 9 000 stops both channels from that frame; malformed,
  forged, replayed and re-granting consent frames are refused as the
  specification requires. The binary verifies zero post-withdrawal leakage,
  the gate and vault counts against what the application received, and the
  accounting identity, then replays the whole session and compares SHA-256
  digests of the raw input, the application's input and the event trace.
  `--json` prints the same run in machine-readable form; the exit code is the
  verdict.
- `reference/reference-bci-7.txt` (FIELD) and
  `reference/reference-bci-7-clean.txt` (CLEAN), diffed byte for byte in CI.
- 18 tests: granted, withdrawn before the first frame, withdrawn mid-session,
  every malformed frame shape, replay, hash reproducibility, the two
  transcripts, the JSON report, a 100 000-frame session, argument checking —
  and two deliberate defects (publishing without the gate, withdrawing without
  revoking the grant) that the verifier must catch.
- Dependencies: `axonos-consent` v0.9.2 and `axonos-sdk` v0.3.5, pinned by tag
  and watched by `verify_pins.py`; `ed25519-dalek` to sign the simulated
  trusted path and `sha2` for the digests, both already in the graph through
  `axonos-consent`.

### Found
- **A withdrawal stopped half the system.** Nothing in the organisation
  connected a consent withdrawal to a vault grant, so derived data would have
  kept flowing under a live grant after intents stopped. The reference BCI
  revokes the grant in the same step and keeps the unconnected version as a
  failing test.
- **One refusal, two numbers.** `axonos-consent` documents the suppression
  codes the SDK delivers as `0x05`/`0x06`; `axonos-sdk` numbers the same
  errors `0x0301`/`0x0302`. Mapped by variant here; not reconciled.

### Changed
- `rust-version` is 1.85, the floor `axonos-consent` and `axonos-sdk` declare.

### Fixed
- The README's organ table listed pins three releases old (`hal v0.2.0`,
  `vault v0.2.0`, `supervisor v0.1.1`) and its heading still said three organs.
  `CITATION.cff` still said 0.3.2. All now match `Cargo.toml`.
- A compiled `scripts/__pycache__/verify_pins.cpython-312.pyc` had been
  committed. Removed, and `__pycache__/` is ignored.
- SPDX licence and copyright headers on every source file.

No timing is measured by anything in this release. The 972 µs both binaries
print is the response time `axonos-hal` admits configurations against.

## [0.3.3] — 2026-08-01

### Fixed
- **Two copies of `axonos-hal` in one dependency graph.** The stack moved to
  HAL 0.3.0 for operating points while `axonos-vault` and `axonos-supervisor`
  stayed on 0.2.0. Cargo resolves that without complaint and the compiler then
  refuses to unify the two `SampleFrame` types — producing an error that names
  the same type twice, which is as confusing as it sounds.

  No individual crate can observe this about itself; only the integration build
  sees it, which is the argument for having one. Fixed by re-pinning the
  dependents rather than by lowering the stack.
- **An unused import failed CI.** `ArtifactReport` was imported and never used.
  It is now used for what it was imported for: the summary names the findings a
  session met rather than counting them, because "1826 frames screened" is a
  number and "slew" is what the recording did.

## [0.3.2] — 2026-08-01

### Added
- Licence texts (`LICENSE-APACHE`, `LICENSE-MIT`), the `NOTICE` that
  Apache-2.0 section 4(d) obliges a redistributor to retain, `CITATION.cff`
  with the author listed first, and this changelog.

  The crate declared `Apache-2.0 OR MIT` in its manifest and its README from
  the first release and shipped neither text. A dual-licence declaration
  without the licences does not grant what it announces: a reader who wants to
  depend on this had nothing to read, and Apache-2.0's attribution clause
  cannot be honoured against a `NOTICE` that does not exist. That is a defect
  in what the repository *is*, not in what it does, which is why it is recorded
  here rather than quietly added.

  No code changed. The version moves because the artefact a consumer receives
  is materially different: it can now be depended on under the terms it always
  claimed.

---

<sub>**axonos-stack v0.3.2** · © 2026 Denis Yermakou · Apache-2.0 OR MIT ·
authored for [The AxonOS Project](https://axonos.org) · connect@axonos.org</sub>
