# Building

<p><b>English</b> | <a href="BUILDING.ru.md">Русский</a></p>

## Desktop (Linux)

### With Nix

```sh
nix run github:Horhik/sample-creator          # build and run
nix build github:Horhik/sample-creator        # package with the .desktop file and icon
```

For development:

```sh
nix develop
cargo run -- ~/Music          # a folder with recordings, or a file
cargo test --workspace
```

### Without Nix

You need Rust 1.85+, `pkg-config`, GTK 4.18+, libadwaita 1.7+ and the ALSA
headers.

| Distribution | Packages |
|---|---|
| Debian / Ubuntu | `build-essential pkg-config libgtk-4-dev libadwaita-1-dev libasound2-dev` |
| Fedora | `gcc pkg-config gtk4-devel libadwaita-devel alsa-lib-devel` |
| Arch | `base-devel pkgconf gtk4 libadwaita alsa-lib` |

```sh
cargo build --release
./target/release/sample-creator
```

SQLite is built in, nothing to install for it. The icon and the `.desktop`
file are in `data/`.

## Android

### With Nix

A shell with Flutter, the Android SDK/NDK and Rust for Android:

```sh
nix develop .#android
cd mobile
flutter build apk --release --split-per-abi --target-platform android-arm64
# build/app/outputs/flutter-apk/app-arm64-v8a-release.apk
```

Gradle builds the Rust part itself (`cargo ndk`). For the emulator add
`android-x64`: `--target-platform android-arm64,android-x64`.

### Without Nix

- Flutter 3.41
- Android SDK: platforms 36, 35 and 31, build-tools 36.0.0, CMake 3.22.1,
  NDK 28.2.13676358
- Rust with the `aarch64-linux-android` target (and `x86_64-linux-android`
  for the emulator) and [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk)

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
cd mobile
flutter build apk --release --split-per-abi --target-platform android-arm64
```

### After changing the bridge API

The files in `mobile/rust/src/api` define what Dart can call in Rust. After
editing them, regenerate the bindings:

```sh
cd mobile
flutter_rust_bridge_codegen generate
```

To try the bridge without a phone:

```sh
cargo run -p sample_creator_bridge --example smoke -- recording.wav
```
