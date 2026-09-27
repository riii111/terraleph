{
  description = "Terraleph development and build environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
          config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "terraform";
        };
      toolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          toolchain = toolchainFor pkgs;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in
        {
          default = rustPlatform.buildRustPackage {
            pname = manifest.package.name;
            version = manifest.package.version;
            src = pkgs.lib.cleanSource self;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "terraleph";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              (toolchainFor pkgs)
              pkgs.cargo-nextest
              pkgs.cargo-audit
              pkgs.cargo-machete
              pkgs.ast-grep
              pkgs.lefthook
              pkgs.actionlint
              pkgs.nixfmt
              pkgs.git
              pkgs.terraform
            ];
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
