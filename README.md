<div align="center">

# axonos-stack

### Three organs, wired together, running one deterministic session.

[![Tests](https://img.shields.io/badge/tests-6%20passing-0d7a5f?style=flat-square)](tests/reference.rs)
[![Locked](https://img.shields.io/badge/build-%2D%2Dlocked-0a4a8f?style=flat-square)](#what-ci-checks-and-why)
[![Transcript](https://img.shields.io/badge/transcript-byte--exact-0a4a8f?style=flat-square)](reference/session-7.txt)
[![License](https://img.shields.io/badge/License-Apache--2.0%20OR%20MIT-475569?style=flat-square)](#licensing)

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
t=   0.000s  posture Nominal
t=   2.000s  ACQ  Overrun { lost: 2 }
t=   2.000s  posture Nominal → Degraded   (FrameLoss { lost: 2 })
t=   2.512s  posture Degraded → Nominal   (Recovered)
t=   4.828s  posture Nominal → Degraded   (LeadOff { frames: 8 })
t=   4.924s  posture Degraded → Restricted   (LeadOff { frames: 32 })
t=   5.000s  READ  bad 51 of 250 frames · 32 bits · 3040 left  [untrusted]

── session summary ──
  delivered 2992 · lost 10 · integrity failures 3
  posture Restricted after 6 transition(s)
  capabilities: acquire=true classify=true actuate=false trustworthy=false
  disclosed 352 bits over 11 release(s) · 0 refused
  accounting: 2992 delivered + 10 lost = 3002 produced ✓
```

An electrode lifts at 4.8 s; within 96 ms the system stops actuating, keeps
recording, and marks every subsequent reading untrusted. The last line is the
identity the whole stack exists to keep: **delivered + lost = produced**. If it
ever fails, one of the three organs is lying about what it saw.

## What the chain found on its first run

The reference session immediately surfaced a defect that no component test could
have: **`axonos-vault` has two bounds that disagree.**

A grant declares a budget in bits — 3 200, or one hundred 32-bit readings. The
audit log holds 64 entries, and the vault refuses to release anything it cannot
record. So the effective ceiling is **2 048 bits, not the 3 200 the grant
states**. Both behaviours are individually correct and individually tested; put
together they make the grant's declared figure not the binding one.

The fix belongs upstream — the vault should refuse to *issue* a grant whose
budget cannot be spent within its own log capacity, rather than silently
enforcing a stricter ceiling than it advertises. It is the first item for vault
0.2, and it is exactly the kind of thing this repository exists to find.

Until then, `tests/reference.rs` asserts the true behaviour and names it, rather
than asserting the assumption.

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
electrodes → axonos-hal ─┬─→ axonos-vault      (what leaves, and how much)
                         └─→ axonos-supervisor (whether anything may act)
```

| Organ | Pinned | Role |
|:--|:--|:--|
| [`axonos-hal`](https://github.com/AxonOS-org/axonos-hal) | `v0.1.1` | the contract with silicon |
| [`axonos-vault`](https://github.com/AxonOS-org/axonos-vault) | `v0.1.1` | the privacy boundary |
| [`axonos-supervisor`](https://github.com/AxonOS-org/axonos-supervisor) | `v0.1.0` | the right to act |

## Licensing

Apache-2.0 OR MIT, matching the AxonOS core.

---

<div align="center">

**© The AxonOS Project / Denis Yermakou**

[axonos.org](https://axonos.org) · [medium.com/@AxonOS](https://medium.com/@AxonOS) · connect@axonos.org · security@axonos.org

</div>
