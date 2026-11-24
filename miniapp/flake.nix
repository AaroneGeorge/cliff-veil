{
  description = "cliff-veil mini-app: Logos core module (cliff_veil_module); the QML view is the flake in ui/";

  inputs = {
    # Pinned builder revision (the one our earlier Logos modules were built and loaded with); its flake.lock pins Qt,
    # logos-protocol and the logos-rust-sdk source the crate links.
    logos-module-builder.url = "github:logos-co/logos-module-builder/a57dc7fd1fe92d3ac7b545c5559b9c581576f2c9";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
    };
}
