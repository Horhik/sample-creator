{
  description = "sample-creator: mark up recorder takes and cut them into samples";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      nativeDeps = pkgs: with pkgs; [ pkg-config wrapGAppsHook4 ];
      libDeps = pkgs: with pkgs; [ gtk4 libadwaita alsa-lib ];
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "sample-creator";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = nativeDeps pkgs;
          buildInputs = libDeps pkgs;
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = nativeDeps pkgs ++ (with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ]);
          buildInputs = libDeps pkgs;
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          # wrapGAppsHook4 only wraps installed binaries; for `cargo run` expose the schemas by hand.
          shellHook = ''
            export XDG_DATA_DIRS=${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}:$XDG_DATA_DIRS
          '';
        };
      });
    };
}
