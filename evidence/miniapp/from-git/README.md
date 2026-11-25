# Mini-app packages built straight from the GitHub URL

On 2026-11-25 both packages were built from the public repository, not from a local checkout:

```bash
nix build 'github:AaroneGeorge/cliff-veil/7c56204?dir=miniapp#lgx-portable' -o core
nix build 'github:AaroneGeorge/cliff-veil/7c56204?dir=miniapp/ui#lgx-portable' -o ui
```

Both builds succeeded (log: `build-7c56204.log`). Their sha256 are identical to the packages installed and used in the
Basecamp 0.3.2 GUI run (`../basecamp/20261125T104537Z/README.md`):

| Package | sha256 |
|---|---|
| core `logos-cliff_veil_module-module-lib.lgx` | `c96108b187f582c7fbdaa2a0df8f7d1875045a5155ac61b2f1790d988437c02d` |
| view `logos-cliff_veil_ui-module.lgx` | `1404d60368f9cb948ffa752a83bbe178b4430119afd8a48cb76968fffbf7ea20` |
