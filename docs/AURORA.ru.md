# Порт на ОС Аврора

Заметки по переносу телефонной версии (`mobile/`) на [ОС Аврора](https://auroraos.ru/).
Проверено по документации [developer.auroraos.ru](https://developer.auroraos.ru/)
(MCP-сервер портала подключён в `.mcp.json`) на октябрь 2026.

## Итог

Перенос реален и недорог: ОМП поддерживает Flutter 3.41.4, а мобильная версия
написана на Flutter 3.41. Движок (`engine/`, ~8,6 тыс. строк) и мост
(`mobile/rust`, ~6,5 тыс.) — чистый Rust без привязки к Android, кроме JNI-контекста
для cpal в `mobile/rust/src/lib.rs`. Интерфейс на Dart (~7,4 тыс.) переносится как есть.
Переписывается платформенный слой: звук, носители, QR, упаковка.

## Что известно о платформе

### Flutter

- Flutter для Авроры 3.41.4, CLI `flutter-aurora`, цели `aurora-arm` (по умолчанию),
  `aurora-arm64`, `aurora-x64`.
- В `dart:io` есть `Platform.isAurora`.
- Типы плагинов: platform channels, channels + Qt, D-Bus, FFI
  (`ffiPlugin: true` в `pubspec.yaml`, нативная часть собирается CMake из `aurora/`).
  Пример: `flutter-aurora create --template=plugin_ffi --platforms aurora …`,
  [examples/ffi](https://developer.auroraos.ru/git/flutter/flutter/-/tree/stable/examples/ffi).
- Портированные плагины ОМП: `path_provider`, `camera`, `qr_code_scanner`,
  `file_picker`, `permission_handler`, `wakelock_plus`, `audioplayers`, `record`.
  Нет: `mobile_scanner`, `dynamic_color`, `flutter_rust_bridge` (последний не нужен
  как плагин — он работает поверх FFI).
- Обновление Flutter у ОМП — примерно раз в 4 месяца.

### Сборка

- **Build Tools (BT)** — Docker-образ с sysroot и кросс-компиляторами для
  aarch64, armv7hl, x86_64: `docker pull hub.omp.ru/public/sdk-build-tools:5.1.0`.
  Образ привязан к релизу ОС, пакеты в него не доустанавливаются (только своим слоем).
  Flutter использует его через `aurora-sdk-dir`.
- На NixOS это удобнее, чем Platform SDK (chroot): Docker уже есть.
- Rust собираем сами против sysroot из образа
  (`armv7-unknown-linux-gnueabihf`, `aarch64-unknown-linux-gnu`), библиотеку
  кладём в пакет. Официальных материалов про Rust-приложения нет (есть только
  статья про D-Bus-сервис на Rust).

### Пакет и валидатор

- Имя пакета `<домен>.<Приложение>`, например `io.github.horhik.SampleCreator`,
  архитектуры armv7hl / aarch64 / x86_64.
- Свои `.so` — в `/usr/share/<пакет>/lib`, у бинарника правильный `rpath`.
  Библиотеки не из списка разрешённых подключаются через
  `%define __requires_exclude ^libname.*$` и
  `%define __provides_exclude_from ^%{_datadir}/%{name}/lib/.*$`.
- В `.spec` запрещены `Vendor`, `Obsoletes`, `%pre/%post/%preun/%postun`.
- Значок — PNG 86, 108, 128, 172 px в `/usr/share/icons/hicolor/<размер>/apps/`.
- Подпись: сертификат профиля **Regular** скачивается свободно
  (`/files/regular_cert.pem`), для других профилей — письмо в dev-support@omp.ru.
  Публикация — RuStore для Авроры (нужен аккаунт разработчика).

### Песочница и разрешения

`[X-Application]` в `.desktop` обязателен: `Permissions=`, `OrganizationName=`,
`ApplicationName=`. Нужные нам и доступные в профиле Regular:

| Разрешение | Зачем |
|---|---|
| `Audio` | воспроизведение |
| `RemovableMedia` | карта памяти и USB-накопители (рекордер) |
| `Music` или `UserDirs` | записи во внутренней памяти |
| `Internet` | общая папка (iroh) |
| `Camera` | сканирование QR-кода |

`UsbDevices` в Regular недоступен, но и не нужен: рекордер подключается как накопитель.

Пути приложения: данные `~/.local/share/<орг>/<прил>`, кэш `~/.cache/<орг>/<прил>`
(их отдаёт `path_provider`). Рядом с записями ничего не пишем — это совпадает с
принципами проекта.

### Звук

- Звуковой сервер — PulseAudio. В Regular разрешены `libpulse.so.0` и
  `libpulse-simple.so.0`; **`libasound` в списке нет**, поэтому cpal на ALSA
  валидатор не пропустит.
- В cpal 0.18.2 есть бэкенд `pulseaudio` (feature `pulseaudio`, крейт `pulseaudio` 0.3).
  Но зависимость `alsa` в cpal на Linux безусловная — бинарник всё равно будет
  ссылаться на `libasound.so.2`.
- Варианты: `[patch]` cpal с необязательной ALSA (предпочтительно) или положить
  `libasound` в пакет и исключить через `__requires_exclude`.
- Проверить: работает ли крейт `pulseaudio` без `libpulse` (клиент протокола
  на Rust) и пускает ли песочница к сокету PulseAudio с `Audio`.

### Носители

- Готового API со списком носителей не нашлось: есть путь `RemovableMedia`
  в libauroraapp, монтирование SD через udev + sd-utils, события в журнале
  безопасности. Канал `sample_creator/storage` (`volumes`, `volumesChanged`)
  пишем сами: точки монтирования или UDisks2 по D-Bus.
- Монтирует ли Аврора рекордер по OTG без MDM-политик — проверить на устройстве.

## Что сделать

| Задача | Оценка (человек) |
|---|---|
| Окружение: Build Tools в Docker, flutter-aurora, `devShells.aurora` | 2–3 дн. |
| Rust под armv7/aarch64, CMake-шаг вместо `buildRust`, загрузка `.so` по `Platform.isAurora` | 1–2 дн. |
| Звук: cpal без ALSA, бэкенд `pulseaudio`; убрать JNI-инициализацию под Авроой | 1–2 дн. |
| Канал носителей | 2–3 дн. |
| QR на `qr_code_scanner`, убрать `dynamic_color`, `display.dart` (системные панели) | 1–2 дн. |
| `.desktop`, `.spec`, значки, валидатор, подпись | 1–2 дн. |
| Жесты: конфликт системных свайпов с ручками и ведением по волне | 1–3 дн. |
| Проверка на устройстве | 3–5 дн. |
| **Итого** | **~2,5–4 недели** |

С ИИ-агентом и устройством под рукой — 2–4 дня по календарю: упирается в сборки
и ручную проверку, а не в код.

## Риски

1. Звук без `libasound` (см. выше).
2. Доступ к рекордеру по OTG из песочницы.
3. Скорость расчёта пиков, спектра и WSOLA на 32-битных корпоративных устройствах.
4. Ошибки линковки C-зависимостей (`ring`, bundled sqlite) против sysroot BT.

## Прототип (первый шаг)

1. Поднять Build Tools, собрать `engine` + мост под `aurora-arm`/`aurora-arm64`,
   загрузить `.so` из Flutter на устройстве.
2. Звук через cpal `pulseaudio`, пройти валидатор.

Устройство: режим разработчика, USB в режиме разработчика, SSH
`defaultuser@192.168.2.15`. Нужны модель и версия ОС (выбор архитектуры).

## Ссылки

- [Flutter для Авроры](https://developer.auroraos.ru/doc/extended/flutter),
  [пакеты и плагины](https://developer.auroraos.ru/doc/extended/flutter/support/packages_and_plugins),
  [плагин FFI](https://developer.auroraos.ru/doc/extended/flutter/plugins_development/ffi)
- [Требования к RPM](https://developer.auroraos.ru/doc/software_development/guidelines/rpm_requirements),
  [файл .desktop](https://developer.auroraos.ru/doc/software_development/guidelines/rpm_requirements/desktop_requirements),
  [профили безопасности](https://developer.auroraos.ru/doc/software_development/guidelines/security_profiles)
- [Данные пользователя](https://developer.auroraos.ru/doc/software_development/reference/user_data)
- [Build Tools](https://developer.auroraos.ru/doc/sdk/app_development/build/build_tools)
- [Аудио](https://developer.auroraos.ru/doc/platform/architecture/multimedia/audio)
- [MCP-сервер портала](https://developer.auroraos.ru/news/17-09-2026-mcp-server)
