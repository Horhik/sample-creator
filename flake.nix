{
  description = "sample-creator: mark up audio recordings and cut them into samples";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # Only for the Android shell: nixpkgs' rustc has no Android std.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      nativeDeps = pkgs: with pkgs; [ pkg-config wrapGAppsHook4 ];
      libDeps = pkgs: with pkgs; [ gtk4 libadwaita alsa-lib ];

      # Versions Flutter 3.41 expects (FlutterExtension.kt), so Gradle never tries
      # to download into the read-only SDK.
      androidShell = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true;
            config.android_sdk.accept_license = true;
            overlays = [ rust-overlay.overlays.default ];
          };
          ndkVersion = "28.2.13676358";
          buildTools = "36.0.0";
          sdk = (pkgs.androidenv.composeAndroidPackages {
            platformVersions = [ "31" "35" "36" ];
            buildToolsVersions = [ "35.0.0" buildTools ];
            includeNDK = true;
            ndkVersions = [ ndkVersion ];
            # The Flutter Gradle plugin configures a stub CMake project with it.
            cmakeVersions = [ "3.22.1" ];
          }).androidsdk;
          sdkRoot = "${sdk}/libexec/android-sdk";
          rust = pkgs.rust-bin.stable.latest.default.override {
            targets = [ "aarch64-linux-android" "x86_64-linux-android" ];
          };
        in
        pkgs.mkShell {
          packages = with pkgs; [
            rust
            cargo-ndk
            flutter
            flutter_rust_bridge_codegen
            jdk17
            # The codegen expands the bridge crate for the host, which links ALSA.
            pkg-config
            alsa-lib
          ];
          ANDROID_HOME = sdkRoot;
          ANDROID_SDK_ROOT = sdkRoot;
          ANDROID_NDK_HOME = "${sdkRoot}/ndk/${ndkVersion}";
          JAVA_HOME = pkgs.jdk17.home;
          # The aapt2 Gradle fetches from Maven is a generic Linux binary that NixOS cannot run.
          GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdkRoot}/build-tools/${buildTools}/aapt2";
        };
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
          meta = {
            description = "Mark up audio recordings and cut them into samples";
            homepage = "https://github.com/Horhik/sample-creator";
            license = pkgs.lib.licenses.mit;
            mainProgram = "sample-creator";
          };
          postInstall = ''
            install -Dm644 data/io.github.horhik.SampleCreator.desktop -t $out/share/applications
            cp -r data/icons $out/share/
          '';
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
      } // nixpkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
        # The Android SDK is only published for x86_64 hosts.
        android = androidShell pkgs.stdenv.hostPlatform.system;
      });
    };
}
