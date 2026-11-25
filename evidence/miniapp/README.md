# Mini-app evidence

- `build.log`: Nix builds of the core module (`miniapp#lgx-portable`) and the QML view (`miniapp/ui#lgx-portable`),
  including the two failed first attempts and their fixes, and the rebuilds after each change. Local paths are
  replaced by `<repo>`, `<work>`, `<build>`.
- `load-logosctl.log`: the core module installed, loaded and called under `logosctl` 0.3.2 (headless Logos Core).
  Part A runs it over the real `cliff-veil` command line: the privacy disclosure read through the module, and a refusal
  (no program ids: the vesting program was not deployed on the dev sequencer at the time) returned as an error object.
  Part B runs it over a stand-in command line printing sample answers in the command line's JSON shapes (not the SDK,
  no chain): a private claim refused before the disclosure acknowledgement, refused with a wrong digest, sent with
  `--yes` after it (to the private account the review named), and refused a second time. The stand-in's record of the
  argument lists the module sent is at the end.
- `live-local-dev/20261124T183901Z.log` and `.json`: the core module under `logosctl` 0.3.2 with the real `cliff-veil`
  command line against the live local-dev sequencer (LEZ v0.4.0-rc3, RISC0_DEV_MODE=1: private claims carry a dev-mode
  receipt, not a real proof). A fresh wallet, funded from LEZ's second published local test key. Every module call
  with its raw answer, the answer pretty-printed, and the checks the run made (31 of 31 passed); the `.json` lists
  the accounts, schedules, transaction hashes and blocks, and every step. Creator view: create (linear with cliff,
  linear, milestones), `list --creator`, cancel, signal. Recipient view: position, claimable, next unlock, public
  claim, the insufficient-gas refusal (`private_fee_shortfall`), the private claim gate (refused before the
  acknowledgement and with a wrong digest, sent after it) and the destination's private balance before and after,
  read from the wallet. The core module was built from the repository with the `read_output` fix below (its sha256 is
  in the log).
- `live-local-dev/20261124T183204Z.log` and `.json`: the run before that fix, 29 of 31. Both private claims were
  included and the private balance grew, but the module answered `backend_bad_output`: the command line prints the
  wallet's sync lines before its JSON on a private claim, and the module did not skip them.
- `screenshots/live-*.png`: `miniapp/ui/Main.qml` rendered offscreen (Qt 6.9.2, software renderer) with the host
  bridge replaced by a stub that replays answers the core module gave in the live run (`recipient_view` from the
  block scan before the public claim, `prepare_claim` and `acknowledge_disclosure` of the private claim after it,
  `creator_view` after the three creations, `prepare_create` of the first one). Each panel is one recorded answer; they are from
  different moments of the run, so the recipient list shows nothing claimed yet while the claim review shows the
  amount claimed after the public claim. The creator form fields are empty because the stub does not fill them.
- `screenshots/miniapp-*.png`: `miniapp/ui/Main.qml` rendered offscreen (Qt 6.9.2, software renderer). The host bridge was
  replaced by a stub returning answers computed by the mini-app's Rust logic over SAMPLE command-line output (not
  chain data). The creator screenshot shows the review panel of a sample creation; its form fields are empty because
  the stub answered without the form being filled.
- `basecamp/20261125T101712Z/`: the mini-app in the Basecamp 0.3.2 GUI (released AppImage, linux x86-64) against
  local-dev (LEZ v0.4.0-rc3, RISC0_DEV_MODE=1: dev mode, no real proofs). Both packages installed through Basecamp's
  Package Manager, the core module loaded in the Module Inspector, the view loaded from the Apps Inspector, Settings
  applied and checked; the recipient view from a block scan; a public claim (summary, estimated fee, gas check, sent)
  and a private claim to a new private account (summary, dev-mode notice, the disclosure, **Confirm claim** disabled
  until the acknowledgement, sent), with transaction hashes and the balances read back from the wallet. Its README
  also records the bridge fix: Basecamp's view bridge hands a module's string answer to QML JSON-encoded once more
  (`"{\"ok\":true,...}"`), so the view parses a second time when the first parse gives a string; without it the view
  ignored every answer (the offscreen renders above use a stub bridge and could not show this).
- `basecamp/20261125T103042Z/`: the creator view in the same Basecamp, after reinstalling packages with the fee
  reserve in the pre-claim summary: a linear schedule with a cliff created with every parameter (reviewed, then
  sent), the creator's active schedules listed, a schedule cancelled (reviewed, then sent); the milestone signal
  refused by the bridge ("Call refused") and the change made for it.
- `basecamp/20261125T104537Z/`: the full run in Basecamp with the final packages, one screenshot per step: recipient
  list; a public claim (summary with estimated fee, fee reserve and gas check; sent); a private claim (summary with
  "Fee reserve (max charged)", disclosure, **Confirm claim** greyed out, acknowledged, sent); the creator list; a
  creation with every parameter (review, sent); a cancel (review, sent); a milestone signal (review, sent) and the
  public claim of that milestone. Transaction hashes, blocks and the balances read back: the private claim's fee
  payer was charged exactly the reserve shown, the public claims the estimated fee.
