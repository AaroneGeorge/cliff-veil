# Creator view in Logos Basecamp 0.3.2 (local-dev, dev mode)

Same Basecamp, wallet and network as `../20261125T101712Z/` (read that README first). Here the packages carry the
fee-reserve change to the pre-claim summary (`miniapp/rust-lib/logic`, `u4_*` tests) and were reinstalled through
the Package Manager ("Reinstall Package?") over the earlier ones:

| Package | sha256 |
|---|---|
| core `logos-cliff_veil_module-module-lib.lgx` | `0b56e75c40513261c4f078e12b1bcc3e12f6130c84ccba2ecc5373431f94cc81` |
| view `logos-cliff_veil_ui-module.lgx` | `b6aa17eedee1d521325a9b9154b7b6e7b38caaebf51c48625b330c86987b92ff` |

Run 10:30 to 10:41 UTC. Creator `CMwTgenX6yX44ti1NVhtf49GjhJC4P8KtwtSMahZehkF`, beneficiary
`BfCG3EUqqT9NzxghbtPCmr2ByyLmstpAN2ggUh6ctfdw`.

| File | What it shows |
|---|---|
| `01-package-manager-installed.png` | Package Manager, Installed: both packages, 0.1.0, under "local" |
| `02-module-inspector-loaded.png` | `cliff_veil_module` loaded again after the reinstall |
| `03-apps-inspector.png`, `04-app-opened.png` | `cliff_veil_ui` loaded from the Apps Inspector and opened |
| `05-settings-applied.png`, `06-settings-check.png` | Settings applied; the check reads the disclosure through the command line |
| `07-create-form.png` | Creator tab, every parameter filled: creator, beneficiary, asset native, kind linear, total 200,000,000, start 10:27Z, cliff 10:36Z, end 12:32Z, cancelable, transferable, cancel authority, milestone authority and refund account (the creator, typed in), label `basecamp-gui-linear` |
| `08-create-review.png` | **Review creation**: every term, the schedule id (SHA-256 of the label), the predicted outcome of the dry run (schedule account `3gfDeTFbPnm3hDD9f319CtP8PnC8MU8FZ3tVzE7hKgp8`, escrowed 200,000,000, creation fee 0) and the estimated network fee (~781,792) |
| `09-create-sent.png` | **Confirm and send**: "Sent: transaction `f951f9ab1207632cfdf048d4a008c956a8be3d8b2d0254a4db06a85d32eff16e` in block 5780"; the active-schedules list reloaded (block scan) with the new schedule on top |
| `10-creator-list.png` | Active schedules of the creator, each with Cancel and Signal (Signal disabled with "linear schedule: no milestones") |
| `11-cancel-review.png` | **Cancel schedule...** on `HyEG9G6by3RyKDXMio61yQHj9AUZQ9Rop1TqZ8Uz4Joy`: predicted outcome (vested part frozen, refund to the refund account, what stays claimable by the beneficiary) and estimated fee |
| `12-cancel-sent.png` | "Sent: transaction `9c18509e06476bf9dbdba321520d85219a4825f7d019b39bb4c9dd5a8d251a46` in block 5791"; the schedule now "cancelled", its Cancel button disabled with "already cancelled" |
| `13-creator-list-after-cancel.png` | The list after the cancel: the milestone schedule `3JaZ...` with milestone index 1 selectable and **Signal milestone...** enabled |
| `14-signal-click-no-review.png`, `15-signal-call-refused.png` | **Signal milestone...** produced no review. With the view's error display widened to show the bridge's own answer (`15`, same view plus that change), it reads "Call refused": Basecamp's view bridge refused `prepare_signal(schedule, authority, index)`, the only three-argument call of the module, while the one- and two-argument calls (create, cancel, claims) went through |

Checked afterwards with the command line (`cliff-veil show`): `3gfDe...` active, linear start 10:27:00Z, cliff
10:36:00Z, end 12:32:00Z, total 200,000,000, cancelable yes, transferable yes, cancel authority, milestone authority
and refund account the creator; `HyEG...` cancelled, claimed 48,291,578, escrow 52,105,264 left for the
beneficiary.

## The signal fix

The module now also offers `prepare_signal_request(request_json)` taking `{"schedule", "authority", "index"}`, one
string like `prepare_create` and `prepare_claim`, and the view calls it (`app::prepare_signal_request`, tested in
`app::tests::u2_create_cancel_and_signal_are_prepared_then_confirmed`). `prepare_signal` stays for `logosctl`. Run in
Basecamp in `../20261125T104537Z/` (`17-signal-review.png`, `18-signal-sent.png`).
