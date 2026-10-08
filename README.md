<div align="center">

# axonos-stack

### The organs wired together: one deterministic session, and one reference BCI.

[![Tests](https://img.shields.io/badge/tests-24%20passing-0d7a5f?style=flat-square)](tests/)
[![Reference BCI](https://img.shields.io/badge/reference%20BCI-leakage%200-0d7a5f?style=flat-square)](reference/reference-bci-7.txt)
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

## AxonOS Reference BCI

```bash
cargo run --locked --release --bin reference_bci
```

One chain from synthetic EEG to an application intent, with the consent
boundary in the middle. It answers one question with a number: after consent is
withdrawn, does anything still reach the application? The number is **0**, and
the run exits non-zero if it is not.

> **Read the long read:** [*Zero After Withdrawal — the AxonOS Reference BCI*](https://gist.github.com/AxonOS-BCI/b5cf55b5ce6a901bbeb0a34faaa1fd8a)
> walks through the run second by second, what building it found, and what it
> does not show. The requirement it led to is [RFC-0012](https://github.com/AxonOS-org/axonos-rfcs/blob/main/rfcs/0012-consent-withdrawal-reaches-every-disclosure-channel.md).

### What is demonstrated

```
SimDevice ─→ pipeline ─→ supervisor ─┬─→ vault ──── grant 1 ─────────→ DERIVED ─┐
axonos-hal   re-reference  posture   │   sealed RAW, bits budget                ├─→ application
             screen                  └─→ decoder ─── consent gate ──→ INTENT ───┘   axonos-sdk types
             band power                              axonos-consent
```

- **RAW, DERIVED and INTENT are kept apart.** Raw frames are sealed in
  `axonos-vault` and go nowhere else; the application has no raw channel.
  Derived data (contact quality) leaves only under a vault grant, charged in
  bits and written to the vault's audit log. Intents (`IntentObservation`)
  leave only through the `axonos-consent` publication gate.
- **Withdrawal.** At frame 9 000 the simulated trusted path sends a signed
  withdrawal. `ConsentMachine` admits it; from that frame the gate refuses
  every intent and the application receives the SDK's terminal
  `ConsentWithdrawn`. The session revokes the vault grant in the same step, so
  every later derived release is refused as `Revoked`.
- **Input that must change nothing.** Before the withdrawal, a truncated frame
  and a withdrawal signed by an unknown key are refused and the state stays
  `Granted`. After it, the replayed withdrawal is refused as `Replay` and a
  correctly signed re-grant as `InadmissibleTransition`: `Withdrawn` is terminal.
- **Verification.** The binary counts what the application received at or
  after the withdrawal frame; checks that the gate's publication count equals
  the intents the application received, and the vault's released bits equal
  the bits it received; checks the accounting identity; then runs the whole
  session a second time and compares SHA-256 digests of the raw input, of
  everything the application received, and of the full event trace.

Two deliberate defects are kept as tests — publishing without the gate, and
admitting a withdrawal without revoking the grant — to show the verifier sees
a leak when there is one.

### Components

| Organ | What this run uses |
|:--|:--|
| `axonos-hal` | `SimDevice` (FIELD or CLEAN fault profile), `TimingBudget::canonical(250)` |
| `axonos-signal-pipeline` | common-average re-reference, artifact screen, Goertzel power at 8 / 10 / 12 / 15 Hz |
| `axonos-supervisor` | posture and capabilities: whether a window may be classified, and whether it is trustworthy |
| `axonos-vault` | the sealed window, `ContactQuality`, grant 1, `revoke` |
| `axonos-consent` | `ConsentMachine`, `PublicationGate`, `Ed25519Strict` |
| `axonos-sdk` | `Manifest` with `Navigation` + `SessionQuality`, `IntentObservation`, `Error::ConsentWithdrawn` |

The decision rule is fixed and published here rather than tuned: the target
with the most power becomes Up, Right, Down or Left if it holds at least half
the power of the four bands, otherwise Neutral. A window the supervisor marks
untrustworthy becomes a `Quality::Low` observation instead of a direction.
Decisions run at 2 Hz because that is the kernel limit for `SessionQuality`,
and the SDK refuses a manifest that asks for more.

### What is measured, and what is not

| Level | In this repository |
|:--|:--|
| **L1 — derived/formally verified** | Not produced here. `axonos-consent` carries the Kani harnesses and loom models for replay refusal, terminal withdrawal and the gate. |
| **L2 — runtime measured** | Behaviour only: counts and SHA-256 digests of a deterministic session on a host, from synthetic input. **No timing is measured.** The 972 µs printed by both binaries is the response time `axonos-hal` admits configurations against; RFC-0001 states it as L2, and its soak trace is not yet published. |
| **L3 — independently instrumented hardware** | None. |

What this does not show:

- **Real EEG.** `SimDevice` emits white noise. The intents are deterministic
  and mean nothing; what is shown is where they may and may not flow, not
  decoding accuracy.
- **Attestation.** In the SDK's design the kernel attaches a truncated
  HMAC-SHA256 to each intent. This session does not run a kernel, so the tag
  is zero rather than an imitation.
- **Concurrency.** The session is single-threaded; withdrawal and revocation
  happen in one step. The gate under concurrent publication is
  `axonos-consent`'s loom models, not this run.
- **Erasure.** The sealed raw window is not destroyed on withdrawal. The
  consent specification does not require it, and this session does not invent
  the rule.

Under the FIELD profile an electrode lifts at 4.8 s and never returns, so from
then on almost every decision is a `Quality::Low` observation. That is the
intended behaviour, and it is why the CLEAN profile is committed as a second
transcript, where the same chain produces directions.

### Expected output

From [`reference/reference-bci-7.txt`](reference/reference-bci-7.txt):

```text
t=  36.000s  CONSENT  signed withdrawal, sequence 1 admitted → Withdrawn · state Withdrawn
t=  36.000s  VAULT    grant 1 revoked
t=  36.000s  GATE     intent suppressed (Withdrawn); application receives ConsentWithdrawn, terminal
t=  36.000s  VAULT    derived release refused: Revoked
t=  36.004s  CONSENT  the same withdrawal again refused: Replay · state Withdrawn
t=  36.008s  CONSENT  signed re-grant, sequence 2 refused: InadmissibleTransition · state Withdrawn

DERIVED DATA · contact quality, released under grant 1
  released                              35   1120 bits
  refused: grant revoked                12
  received at/after withdrawal           0

APPLICATION INTENT · axonos-sdk IntentObservation
  generated                             94   direction 1 · neutral 3 · quality 90
  delivered                             70
  suppressed by consent gate            24
  received at/after withdrawal           0

CHECKS
  ✓ accounting                      11965 delivered + 46 lost = 12011 produced, supervisor agrees
  ✓ gate count = intents received   70 = 70
  ✓ vault bits = bits received      1120 = 1120
  ✓ post-withdrawal leakage = 0
  ✓ every consent frame answered as the specification requires
  ✓ deterministic replay            2 runs, 0 mismatch(es)

trace SHA-256        cfaa12273ba1de9093cf9ea6a044d7fc9b47480cf0f893a69d5b623265a20629

RESULT: VERIFIED
```

### Reproduce

```bash
cargo run --locked --release --bin reference_bci                          # FIELD, withdrawal at frame 9000
cargo run --locked --release --bin reference_bci -- --profile clean       # a device behaving perfectly
cargo run --locked --release --bin reference_bci -- --json                # the same run, machine-readable
cargo run --locked --release --bin reference_bci -- --frames 100000 --withdraw-at 60000
cargo run --locked --release --bin reference_bci -- --withdraw-at 0       # consent withdrawn before the first frame
cargo test --locked
```

Exit code `0` is VERIFIED, `1` is FAILED, `2` is a bad argument. CI diffs both
transcripts byte for byte, so the trace hash above is checked on every push.

## What the chain has found so far

Defects none of which a component test could reach. This is the record,
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

**A withdrawal that stopped half the system.** `axonos-consent` stops
intents at its gate; `axonos-vault` stops disclosures when a grant is revoked.
Nothing connected the two, so a withdrawal would have stopped the intents while
derived data kept flowing under a live grant. The reference BCI is the first
code in the organisation that revokes the grant when consent is withdrawn, and
it keeps the unconnected version as a test that must fail. The requirement
that closes it for every implementation is [RFC-0012](https://github.com/AxonOS-org/axonos-rfcs/blob/main/rfcs/0012-consent-withdrawal-reaches-every-disclosure-channel.md).

**One refusal, two numbers.** `axonos-consent` documents
`Suppressed::abi_code()` as the error the SDK delivers — `0x05` suspended,
`0x06` withdrawn. `axonos-sdk` numbers the same two errors `0x0301` and
`0x0302`. The reference BCI maps the refusal by variant, not by code, and the
two numbering schemes are left for one of the two crates to reconcile.

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
| **`verify_pins.py`** | `--locked` proves the *lockfile* is unchanged. It does **not** notice a tag repointed on the remote: cargo fetches the recorded revision and builds happily. A tag is a mutable pointer with an immutable-sounding name, and this stack is assembled from six of them. Runs first, and on a daily schedule, because a tag can move on a day nobody pushes. |
| **`cargo build --locked`** | A library correctly ignores its lockfile; an application must commit one. This is the application. |
| **transcript diffs** | The artifact here is a *behaviour*, not a binary. Three transcripts are diffed: the session and the reference BCI under both fault profiles. Diffing the transcript states what the system does; a checksum over a binary would only state what it compiled to — and a `no_std` library stack has no binary worth hashing. |
| **`cargo clippy -D warnings`** | — |
| **`cargo doc -D warnings`** | A doc link that resolves in one feature configuration and not another has already broken a build in this project once. |
| **actions pinned to SHA** | A tag on an action is the same mutable pointer, with write access to this repository. |

## Regenerating the transcript

Only when a behaviour change is intended, and never to make CI green:

```bash
cargo run --locked --bin session -- --seed 7 --frames 3000 > reference/session-7.txt
cargo run --locked --release --bin reference_bci > reference/reference-bci-7.txt
cargo run --locked --release --bin reference_bci -- --profile clean > reference/reference-bci-7-clean.txt
```

The diff belongs in the commit message. A transcript regenerated without one is
a silent behaviour change wearing a green check.

## The stack

```
electrodes → axonos-hal ─→ axonos-signal-pipeline ─┬─→ axonos-vault ──────────→ derived ─┐
                           re-reference · screen   │    (what leaves, and how much)       │
                           · narrowband power      ├─→ axonos-supervisor                  ├─→ application
                                                   │    (whether anything may act)        │   (axonos-sdk)
                                                   └─→ axonos-consent ────────→ intents ──┘
                                                        (whether anything may flow)
```

| Organ | Pinned | Role |
|:--|:--|:--|
| [`axonos-hal`](https://github.com/AxonOS-org/axonos-hal) | `v0.3.0` | the contract with silicon |
| [`axonos-signal-pipeline`](https://github.com/AxonOS-org/axonos-signal-pipeline) | `v0.9.2` | conditioning: re-referencing, artifact screening, spectral power |
| [`axonos-vault`](https://github.com/AxonOS-org/axonos-vault) | `v0.2.2` | the privacy boundary |
| [`axonos-supervisor`](https://github.com/AxonOS-org/axonos-supervisor) | `v0.1.3` | the right to act |
| [`axonos-consent`](https://github.com/AxonOS-org/axonos-consent) | `v0.9.2` | the consent boundary — reference BCI only |
| [`axonos-sdk`](https://github.com/AxonOS-org/axonos-sdk) | `v0.3.5` | the application's types — reference BCI only |

The session binary uses the first four; the reference BCI uses all six.
`ed25519-dalek` signs the simulated trusted path's frames and `sha2` computes
the trace digests; both were already in the graph through `axonos-consent`.

## Licensing

Apache-2.0 OR MIT, matching the AxonOS core.

---

<div align="center">

**© 2026 Denis Yermakou** — authored for The AxonOS Project

[axonos.org](https://axonos.org) · connect@axonos.org · security@axonos.org

</div>
