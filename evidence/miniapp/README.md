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
- `screenshots/`: `miniapp/ui/Main.qml` rendered offscreen (Qt 6.9.2, software renderer). The host bridge was
  replaced by a stub returning answers computed by the mini-app's Rust logic over SAMPLE command-line output (not
  chain data). The creator screenshot shows the review panel of a sample creation; its form fields are empty because
  the stub answered without the form being filled.
- Not verified: loading in Basecamp; a claim against a live sequencer.
