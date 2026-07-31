# Changelog

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

All notable changes to axonos-stack are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
