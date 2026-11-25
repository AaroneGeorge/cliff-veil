# Full run in Logos Basecamp 0.3.2: recipient and creator views (local-dev, dev mode)

Same Basecamp 0.3.2 (released AppImage, linux x86-64, on a virtual display), wallet and network as
`../20261125T101712Z/` (local-dev, LEZ v0.4.0-rc3, `RISC0_DEV_MODE=1`: a private claim carries a dev-mode receipt,
not a real proof). Every screen below was driven by mouse and keyboard in Basecamp's window and captured from the
display at full resolution. Run 10:45 to 11:03 UTC.

Packages, built from the working tree (`nix build path:./miniapp#lgx-portable`, `path:./miniapp/ui#lgx-portable`)
and installed over the earlier ones with **Package Manager > Install Local Package > Reinstall**:

| Package | sha256 | What is new since `../20261125T103042Z/` |
|---|---|---|
| core `logos-cliff_veil_module-module-lib.lgx` | `c96108b187f582c7fbdaa2a0df8f7d1875045a5155ac61b2f1790d988437c02d` | the fee reserve in the pre-claim summary and the gas check; `prepare_signal_request` |
| view `logos-cliff_veil_ui-module.lgx` | `1404d60368f9cb948ffa752a83bbe178b4430119afd8a48cb76968fffbf7ea20` | the signal sent as one JSON request; the bridge's own error text shown; the fee payer field states the SDK's choice |

Accounts: creator `CMwTgenX6yX44ti1NVhtf49GjhJC4P8KtwtSMahZehkF`, beneficiary
`BfCG3EUqqT9NzxghbtPCmr2ByyLmstpAN2ggUh6ctfdw`.

## Screens

| File | What it shows |
|---|---|
| `00a-reinstall-core.png` | "Reinstall Package?" for the core package |
| `00b-package-manager-installed.png` | Package Manager, Installed: both packages under "local" |
| `00c-module-loaded.png` | Module Inspector: `cliff_veil_module` LOADED |
| `00d-settings.png` | The view's Settings: command line path and environment applied, the disclosure check passing |
| `01-recipient-list.png` | Recipient view, list from a block scan: 4 schedules, total locked and claimable now, per schedule position, next unlock, terms |
| `02-public-claim-panel.png` | **Claim...** on `DLi2VtXvqSvt8XgUYdG2zV32L9Fmso15rYXaY7WzoMeZ`, public destination |
| `03-public-preclaim-summary.png` | Pre-claim summary: claimable at least 76,315,790, estimated network fee ~803,704, fee reserve 1,003,192 held at admission (charged: the actual fee), fee payer and why, gas check passed against the reserve |
| `04-public-claim-sent.png` | "Claimed: transaction `503e64f8b6874d79ce8c3041147e960e17e42ae4ddb6b9e256eccc9b7eb604cd` in block 5844" |
| `05-private-claim-panel.png` | **Claim...** on `HyEG9G6by3RyKDXMio61yQHj9AUZQ9Rop1TqZ8Uz4Joy` (cancelled earlier; its vested part stays claimable), "To a private account of this wallet", the fee payer field stating the SDK's automatic choice |
| `06-private-summary-disclosure-not-ready.png` | Summary: 52,105,264 claimable, destination a new private account `DN2kbJZj7U61NcbzLeuW67uMLHrhrdCTJ9wr3z6Dyp7m` (the transaction names only a one-time address), estimated network fee ~5,532,192, **Fee reserve (max charged) 16,596,576, fixed in the proof**, fee payer `Hx3QyGCxpbD66nyjVRGCdnwMk3Kk21ERkY5Y7uydxGc7` chosen by the SDK (smallest private balance that covers the reserve), gas check against the reserve, the dev-mode notice, the privacy disclosure, the box unticked, "Not ready: ...", **Confirm claim** greyed out |
| `07-private-acknowledged-confirm-active.png` | Box ticked: **Confirm claim** active |
| `08-private-claim-sent.png` | "Claimed: transaction `b9bfcc34a9f55bb6af5d4fd470f442fc1cd8160d8eb8398e6bb065428e9e5d05` in block 5854" |
| `09-creator-active-schedules.png` | Creator tab, **Load schedules** (block scan): the creator's schedules with Cancel and Signal per schedule |
| `10-create-form.png` | Create form with every parameter: beneficiary, asset native, kind linear, total 150,000,000, start, cliff, end, cancelable, transferable, cancel authority, milestone authority, refund account, label `basecamp-demo-linear` |
| `11-create-review.png` | **Review creation**: all terms, schedule id from the label, predicted outcome (schedule `E4bAjeNV6TwTMVUW8R9oSWxx815uFm7gJFHKjXzWjE5f`, escrowed 150,000,000, creation fee 0), estimated fee ~781,864 |
| `12-create-sent.png` | "Sent: transaction `78cc1ef24c0c8c58425e86281f73ed506db0d769b88511082291c6ec241caa91` in block 5867" |
| `13-creator-list-after-create.png` | The list reloaded with the new schedule |
| `14-cancel-review.png` | **Cancel schedule...** on `3gfDeTFbPnm3hDD9f319CtP8PnC8MU8FZ3tVzE7hKgp8`: vested part frozen (47,528,266, stays claimable), refund 152,471,734 to the refund account, estimated fee |
| `15-cancel-sent.png` | "Sent: transaction `fda3369ec57d8377caaeef2c307cb18f279f5f0034b720793e6166ed7d05cd5a` in block 5876" |
| `16-milestone-schedule-before-signal.png` | `3JaZUVbrFfuY94DPThFTcgWkDVg9JVUg7GjNqDM63773`: milestone #1 (200,000,000) pending, index 1 selected |
| `17-signal-review.png` | **Signal milestone...**: "Review: Signal milestone 1", unlocks 200,000,000, estimated fee ~397,104 (the call that Basecamp refused in `../20261125T103042Z/`, now one JSON request) |
| `18-signal-sent.png` | "Sent: transaction `a834ce0507a3742bbbe71b9e4eefa039b307fdf92dde815708d4a99b83dcb58a` in block 5883" |
| `19-recipient-after-signal.png` | Recipient view: `3JaZ...` 2 of 2 signalled, 200,000,000 claimable now |
| `20-milestone-claim-panel.png`, `21-milestone-public-summary.png` | Public claim of milestone #1: summary (at least 200,000,000, estimated fee ~809,624, reserve 1,010,592, gas check) |
| `22-milestone-claim-sent.png` | "Claimed: transaction `0b98964ebf5812f9886d803729c958dc47950ca096e7122cace654f8c6f99808` in block 5895" |
| `basecamp-log-excerpt.txt` | Basecamp's log lines for the reinstall and the loads (times local, UTC+05:30) |

## Checked afterwards (command line `show` and the wallet, block 5895+)

| What | Result |
|---|---|
| Private claim, fee charged vs reserve | Fee payer `Hx3Q...` held 26,174,602 before and 9,578,026 after: 16,596,576 charged, exactly the reserve the summary showed (estimated fee 5,532,192). Destination `DN2k...` holds 52,105,264, the claimable amount shown |
| Public claims, fee charged vs reserve | Beneficiary 223,411,043 before the run, 301,554,668 after the first claim (+78,947,369 claimed, 803,744 fee; estimate 803,704, reserve 1,003,192), 500,745,044 after the milestone claim (+200,000,000, 809,624 fee; estimate 809,624, reserve 1,010,592) |
| `DLi2...` | active, claimed 203,966,228 |
| `HyEG...` | settled (cancelled, fully claimed), claimed 100,396,842 |
| `E4bA...` (created here) | active, linear start 10:52Z, cliff 10:59Z, end 12:24Z, total 150,000,000, cancelable, transferable |
| `3gfDe...` (cancelled here) | cancelled, 48,728,266 left in escrow for the beneficiary |
| `3JaZ...` | settled, both milestones signalled, claimed 500,000,000 |

So in Basecamp: a private claim charges its fee payer the reserve fixed in the proof, which the summary now shows as
"Fee reserve (max charged)"; a public claim charges the actual fee, within the reserve held at admission.
