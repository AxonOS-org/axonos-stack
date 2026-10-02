<div align="center">

# axonos-stack

### Three organs, wired together, running one deterministic session.

[![Tests](https://img.shields.io/badge/tests-6%20passing-0d7a5f?style=flat-square)](tests/reference.rs)
[![Locked](https://img.shields.io/badge/build-%2D%2Dlocked-0a4a8f?style=flat-square)](#what-ci-checks-and-why)
[![Transcript](https://img.shields.io/badge/transcript-byte--exact-0a4a8f?style=flat-square)](reference/session-7.txt)
[![License](https://img.shields.io/badge/License-Apache--2.0%20OR%20MIT-475569?style=flat-square)](#licensing)
[![AxonOS Radar](https://img.shields.io/badge/AxonOS%20Radar-open%20neurotech%20map-1f8fae?style=flat-square&labelColor=0b1220)](https://axonos-bci.github.io/axonos-community-radar/)

</div>

---

Each organ is tested against the one below it. `axonos-vault` is tested with
`axonos-hal`; `axonos-supervisor` is tested with `axonos-hal`. **Nothing tested
the chain** — and a chain is where the interesting failures live, because every
component is correct about its own contract and wrong about its neighbour's
assumptions.

```bash
cargo run --locked --bin session -- --seed 7 --frames 3000
```

Same seed, same transcript, byte for byte, on any machine. CI diffs it against
[`reference/session-7.txt`](reference/session-7.txt), so a change in any
dependency that alters observable behaviour fails the build with a diff showing
exactly what moved.

This also closes a roadmap item that has been open since the ecosystem table was
written — *"a deterministic simulator, so a developer can run the full path
without hardware"*. `SimDevice` was half of it. This is the rest.

## What the session says

```text
  seed 7 · 250 SPS · 4000 µs period · 3000 frames · fault profile FIELD
  chain WCRT 972 µs, 17.3% of the period
  grant 1 · QualityFeedback · 2048 bits (= log capacity × 32)

t=   0.000s  posture Nominal
t=   2.000s  ACQ  Overrun { lost: 2 }
t=   2.000s  posture Nominal → Degraded   (FrameLoss { lost: 2 })
t=   2.512s  posture Degraded → Nominal   (Recovered)
t=   4.828s  posture Nominal → Degraded   (LeadOff { frames: 8 })
t=   4.924s  posture Degraded → Restricted   (LeadOff { frames: 32 })
t=   5.000s  READ  bad 51 of 250 frames · 32 bits · 1856 left  [untrusted]

── session summary ──
  delivered 2992 · lost 10 · integrity failures 3
  posture Restricted after 6 transition(s)
  capabilities: acquire=true classify=true actuate=false trustworthy=false
  disclosed 352 bits over 11 release(s) · 0 refused
  vault holds 250 frames; grant 1 has 1696 of 2048 bits left
  accounting: 2992 delivered + 10 lost = 3002 produced ✓
```

An electrode lifts at 4.8 s; within 96 ms the system stops actuating, keeps
recording, and marks every subsequent reading untrusted. The last line is the
identity the whole stack exists to keep: **delivered + lost = produced**. If it
ever fails, one of the three organs is lying about what it saw.

## What the chain has found so far

Three defects, none of which a component test could reach. This is the record,
because a repository that only reports its successes is a repository whose
failures went somewhere else.

**The two bounds that disagreed.** `axonos-vault` issued 3 200-bit grants
against a 64-entry audit log, so the effective ceiling was 2 048 bits and the
figure written in the grant was not the one that stopped it. Both behaviours
were individually correct and individually tested. Closed by RFC-0009 N5 in
vault 0.2.0; budget and log capacity now reach zero together.

**A silently swallowed refusal.** This session called `vault.issue(...)` and
ignored its return value. When vault 0.2.0 began refusing the oversized grant,
the session ran to completion with no grant installed, reporting `NoSuchGrant`
for every reading — a misconfiguration wearing the appearance of a working
run. Found by re-pinning, and fixed here: the return is checked and the
session fails loudly.

**A simulator that is not band-limited.** Adding the conditioning stage in
0.3.0 put 61 % of frames over the slew limit — consecutive samples stepping
further than physiology allows. The detector is right: `SimDevice` emits white
noise, whose consecutive samples are independent, while a real acquisition path
is band-limited and cannot step that far between samples. The threshold is the
published one and stays; the transcript says so in a `NOTE` line rather than
being quietly tuned until the demonstration looked better than the thing it
demonstrates.

**Numbers that were never measured.** `axonos-hal` v0.1.1 carried a
seven-entry stage table documented as measured on the reference hardware, and
a utilisation ceiling of 0.80 against a published 0.25 — which admitted
500 SPS, a configuration this project's own RFC-0001 forbids. Corrected in
0.2.0 and recorded as RFC-0008 D1 and D2. The session's utilisation line moved
from 24.4 % to the published 17.3 % as a result, which is why this
repository's transcript is versioned rather than regenerated quietly.

## What CI checks, and why

| Check | Why it is not redundant |
|:--|:--|
| **`verify_pins.py`** | `--locked` proves the *lockfile* is unchanged. It does **not** notice a tag repointed on the remote: cargo fetches the recorded revision and builds happily. A tag is a mutable pointer with an immutable-sounding name, and this stack is assembled from three of them. Runs first, and on a daily schedule, because a tag can move on a day nobody pushes. |
| **`cargo build --locked`** | A library correctly ignores its lockfile; an application must commit one. This is the application. |
| **transcript diff** | The artifact here is a *behaviour*, not a binary. Diffing the transcript states what the system does; a checksum over a binary would only state what it compiled to — and a `no_std` library stack has no binary worth hashing. |
| **`cargo clippy -D warnings`** | — |
| **`cargo doc -D warnings`** | A doc link that resolves in one feature configuration and not another has already broken a build in this project once. |
| **actions pinned to SHA** | A tag on an action is the same mutable pointer, with write access to this repository. |

## Regenerating the transcript

Only when a behaviour change is intended, and never to make CI green:

```bash
cargo run --locked --bin session -- --seed 7 --frames 3000 > reference/session-7.txt
```

The diff belongs in the commit message. A transcript regenerated without one is
a silent behaviour change wearing a green check.

## The stack

```
electrodes → axonos-hal ─→ axonos-signal-pipeline ─┬─→ axonos-vault
                           re-reference · screen   │    (what leaves, and how much)
                           · narrowband power      └─→ axonos-supervisor
                                                        (whether anything may act)
```

| Organ | Pinned | Role |
|:--|:--|:--|
| [`axonos-hal`](https://github.com/AxonOS-org/axonos-hal) | `v0.2.0` | the contract with silicon |
| [`axonos-signal-pipeline`](https://github.com/AxonOS-org/axonos-signal-pipeline) | `v0.9.2` | conditioning: re-referencing, artifact screening, spectral power |
| [`axonos-vault`](https://github.com/AxonOS-org/axonos-vault) | `v0.2.0` | the privacy boundary |
| [`axonos-supervisor`](https://github.com/AxonOS-org/axonos-supervisor) | `v0.1.1` | the right to act |

## Licensing

Apache-2.0 OR MIT, matching the AxonOS core.

---

<div align="center">

**© 2026 Denis Yermakou** — authored for The AxonOS Project

[axonos.org](https://axonos.org) · connect@axonos.org · security@axonos.org

</div>
