# The mini-app in Logos Basecamp 0.3.2, against the local-dev sequencer

Both packages installed through Basecamp's own Package Manager, the core module loaded in the Module Inspector, the
view opened from the sidebar, and a public and a private claim made from the view's screens. Network: local-dev
(LEZ v0.4.0-rc3, `RISC0_DEV_MODE=1`: a private claim carries a dev-mode receipt, not a real proof).

## Setup

| What | Value |
|---|---|
| Basecamp | `LogosBasecamp-Desktop-v0.3.2-f2fae6-x86_64.AppImage` from the `0.3.2` release of logos-co/logos-app (no checksum is published; ours: sha256 `983de0c7e282155d93c7e338da78e5b1a7a32112390a89be48d44700c3ade012`), extracted with `--appimage-extract`, run on Linux x86-64 on a virtual X display (Xvfb, 1920x1080) and watched over VNC. Its Dashboard reads "Version 0.3.2, Portable build". Basecamp needs `libOpenGL.so.0` (libglvnd) on the host. |
| Core package | `logos-cliff_veil_module-module-lib.lgx`, `miniapp#lgx-portable`, sha256 `d8f94ceb27b942f3eb92eec10738316368af11a1d60f9af2487f026c12abda28` (the same file as the headless run in `../../live-local-dev/20261124T183901Z.log`) |
| View package | `logos-cliff_veil_ui-module.lgx`, `miniapp/ui#lgx-portable` built from the working tree with the fix below, sha256 `216bd37fb73614431b4bdf63e6411c6d1939f9fbc51536ac63cb35eb3458ff91` |
| Command line | `cliff-veil` (`cli/`), the build the headless run used, entered in the view's Settings tab |
| Wallet | A fresh wallet home. A creator and a beneficiary account, funded from LEZ's second published local test key; three schedules created and milestone 0 signalled with the command line before Basecamp was opened: linear with a cliff (`DLi2VtXvqSvt8XgUYdG2zV32L9Fmso15rYXaY7WzoMeZ`, 1,000,000,000), fully linear (`HyEG9G6by3RyKDXMio61yQHj9AUZQ9Rop1TqZ8Uz4Joy`, 600,000,000), milestones (`3JaZUVbrFfuY94DPThFTcgWkDVg9JVUg7GjNqDM63773`, 300,000,000 + 200,000,000). Beneficiary `BfCG3EUqqT9NzxghbtPCmr2ByyLmstpAN2ggUh6ctfdw`. |

## Found in Basecamp, fixed

The first time the view called the module, every answer was ignored: Basecamp 0.3.2's QML bridge
(`logos.callModule`) hands a module's string result to QML JSON-encoded once more (`"{\"ok\":true,...}"`), so
`JSON.parse` gave a string, not the answer object. The offscreen renders with a stub bridge could not show this. Fix
in `miniapp/ui/Main.qml` (`call`): parse a second time when the first parse gives a string. With that file copied
into the installed view, a first pass (09:59 to 10:05 UTC) made a public claim from `DLi2...` (transaction
`9e8d00e1f6041e5219ac3d5170827169b51a664c2178d1a667001106966bcde5`, block 5651) and a private claim from `HyEG...`
to a new private account (transaction `bdf0d0546f9771748e64fd4215be544d76a1620bec30195e8d9ba1214a47b3e6`, block
5660; the new account `Hx3QyGCxpbD66nyjVRGCdnwMk3Kk21ERkY5Y7uydxGc7` paid the fee out of the claim and then held
42,762,634, read with `wallet account get` after `account sync-private`). The view package was then rebuilt from the
repository with the fix (its `Main.qml` is byte-identical to the file used in that pass) and reinstalled through
the Package Manager ("Reinstall Package?"); the run below uses the rebuilt package only.

## The run (10:17 to 10:23 UTC), screenshot by screenshot

| File | What it shows |
|---|---|
| `01-package-manager-installed.png` | Package Manager, Installed: "cliff-veil vesting core" (core, 0.1.0) and "cliff-veil vesting" (ui_qml, 0.1.0) under "local" |
| `02-module-inspector.png` | Settings > Module Inspector: `cliff_veil_module` LOADED, with its CPU and memory |
| `03-apps-inspector.png` | Settings > Apps Inspector: `cliff_veil_ui` listed, before **Load** |
| `04-app-opened.png` | After **Load**: the view open in its own tab (Recipient, Creator, Settings) |
| `05-settings-applied.png` | Settings tab: command line path and environment (wallet home, `CV_IDS`, `RISC0_DEV_MODE=1`, `RISC0_SERVER_PATH`), **Apply**: "Applied: ..." |
| `06-settings-check.png` | **Check**: "The command line answers; its disclosure names every required item." |
| `07-recipient-view.png`, `08-recipient-view-more.png` | Recipient view for the beneficiary, schedule list empty, so the module lists the beneficiary's schedules by a block scan: 3 schedules, total locked and claimable now; per schedule total, vested (bar), claimed, total locked, claimable now, next unlock, terms, flags |
| `09-public-claim-panel.png` | **Claim...** on `DLi2...`: destination choice, public selected |
| `10-public-preclaim-summary.png` | **Review claim**: pre-claim summary (claimable "at least 46,710,570", claimed after, still to come, estimated network fee ~803,912 from a dry run, fee payer and its balance) and "Gas check passed" |
| `11-public-claim-sent.png` | **Confirm claim**: "Claimed: transaction `8f3dae7d2095683c36119b1709aadd80a24aeb750c40c58baa1a78409a2be6c0` in block 5724" |
| `12-private-claim-panel.png` | **Claim...** on the milestone schedule `3JaZ...`, "To a private account of this wallet" selected, destination and fee payer left empty |
| `13-private-preclaim-disclosure.png` | **Review claim**: summary (300,000,000 claimable; destination a new private account `9J8pGqqKi88EJK6jxh1TnLvF9LKRdEmExy87ypyfBW2v`, the transaction naming only a one-time address; fee payer `Hx3Q...` chosen by the SDK; gas check; the dev-mode notice), the privacy disclosure, the acknowledgement box unticked, "Not ready: ... sent only after its privacy disclosure is acknowledged", **Confirm claim** disabled |
| `14-private-confirm-disabled-click.png` | After clicking the disabled **Confirm claim**: nothing sent, same state |
| `15-private-acknowledged.png` | Box ticked ("I have read what this claim makes public and what it does not"): **Confirm claim** enabled |
| `16-private-claim-sent.png` | **Confirm claim**: "Claimed: transaction `f3bc705d672e0f772574007f4d2022579afd1f5606ee38b4e95d68417877ad92` in block 5733" |
| `basecamp-log-excerpt.txt` | Basecamp's own log lines for the installs (unsigned local packages), the reinstall, the module load and the view loads (times are local, UTC+05:30) |

Checked afterwards with the command line and the wallet (block 5737):

| Account | After the run |
|---|---|
| Schedule `DLi2...` | claimed 125,018,859 (76,992,500 in the first pass, the rest in this run) |
| Schedule `3JaZ...` | claimed 300,000,000 |
| Beneficiary (public) | balance 223,411,043 (100,000,000 funded, plus both public claims, less their fees) |
| `Private/9J8p...` (new destination) | balance 300,000,000 |
| `Private/Hx3Q...` (fee payer) | balance 26,174,602 (42,762,634 before: 16,588,032 taken) |

## Not covered, and one gap seen

- The creator tab was not driven in Basecamp (create, list, cancel and signal ran through the same core module
  headless: `../../live-local-dev/`).
- Dev mode only: the private claims carry no real proof.
- The private claim's summary shows "Estimated network fee ~5,529,344", but the fee payer was charged 16,588,032:
  the fee reserve fixed in the private transaction, priced for 12 blocks of steepest base-fee rise (the SDK's dry
  run reports it as `reserve` / `headroom_reserve`, 16,584,912 for the same claim a minute later). The view did not
  show that reserve then; it does since `../20261125T104537Z/`, where the charge equals the reserve shown.
