{
  description = "cliff-veil mini-app: QML view (cliff_veil_ui); it calls the core module cliff_veil_module by name";

  inputs = {
    # Same builder revision as the core module (../flake.nix), so both plugins link one Qt and logos-protocol closure.
    logos-module-builder.url = "github:logos-co/logos-module-builder/a57dc7fd1fe92d3ac7b545c5559b9c581576f2c9";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosQmlModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
    };
}
