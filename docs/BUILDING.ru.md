# Сборка

<p><a href="BUILDING.md">English</a> | <b>Русский</b></p>

## Десктоп (Linux)

### С Nix

```sh
nix run github:Horhik/sample-creator          # собрать и запустить
nix build github:Horhik/sample-creator        # пакет с .desktop-файлом и значком
```

Для разработки:

```sh
nix develop
cargo run -- ~/Music          # папка с записями или сразу файл
cargo test --workspace
```

### Без Nix

Нужны Rust 1.85+, `pkg-config`, GTK 4.18+, libadwaita 1.7+ и заголовки ALSA.

| Дистрибутив | Пакеты |
|---|---|
| Debian / Ubuntu | `build-essential pkg-config libgtk-4-dev libadwaita-1-dev libasound2-dev` |
| Fedora | `gcc pkg-config gtk4-devel libadwaita-devel alsa-lib-devel` |
| Arch | `base-devel pkgconf gtk4 libadwaita alsa-lib` |

```sh
cargo build --release
./target/release/sample-creator
```

SQLite собирается вместе с приложением, отдельно не нужен. Значок и
`.desktop`-файл лежат в `data/`.

## Android

### С Nix

Окружение с Flutter, Android SDK/NDK и Rust для Android:

```sh
nix develop .#android
cd mobile
flutter build apk --release --split-per-abi --target-platform android-arm64
# build/app/outputs/flutter-apk/app-arm64-v8a-release.apk
```

Rust-часть Gradle собирает сам (`cargo ndk`). Для эмулятора добавьте
`android-x64`: `--target-platform android-arm64,android-x64`.

### Без Nix

- Flutter 3.41
- Android SDK: платформы 36, 35 и 31, build-tools 36.0.0, CMake 3.22.1,
  NDK 28.2.13676358
- Rust с таргетами `aarch64-linux-android` (и `x86_64-linux-android` для
  эмулятора) и [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk)

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
cd mobile
flutter build apk --release --split-per-abi --target-platform android-arm64
```

### После изменений в API моста

Файлы в `mobile/rust/src/api` описывают, что Dart может вызывать из Rust.
После правок там перегенерируйте привязки:

```sh
cd mobile
flutter_rust_bridge_codegen generate
```

Проверить мост без телефона:

```sh
cargo run -p sample_creator_bridge --example smoke -- запись.wav
```
