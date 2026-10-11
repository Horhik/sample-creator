import 'package:flutter/foundation.dart';

import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as review_api;

/// The user's choice: 'system', 'en' or 'ru'. Remembered between launches.
final language = ValueNotifier<String>('system');

/// Reads the saved choice once the engine is up, and tells the engine.
void loadLanguage() {
  try {
    language.value = review_api.getSetting(key: 'mobile.language') ?? 'system';
  } catch (_) {
    // The engine is not running (start-up failure): follow the system.
  }
  _tellEngine();
}

Future<void> setLanguage(String choice) async {
  language.value = choice;
  _tellEngine();
  await review_api.setSetting(key: 'mobile.language', value: choice);
}

void _tellEngine() {
  try {
    engine.setLanguage(russian: isRussian);
  } catch (_) {}
}

bool get isRussian => switch (language.value) {
  'ru' => true,
  'en' => false,
  _ => PlatformDispatcher.instance.locale.languageCode == 'ru',
};

/// The text in the interface language.
String tr(String ru, String en) => isRussian ? ru : en;

/// Units after numbers.
String get sec => tr('с', 's');
String get ms => tr('мс', 'ms');
