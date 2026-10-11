import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'src/rust/api/review.dart' as review_api;

/// Full screen: hides the status bar and the system navigation (they come
/// back with a swipe from the edge). Remembered between launches.
final fullscreen = ValueNotifier<bool>(
  review_api.getSetting(key: 'mobile.fullscreen') == '1',
);

Future<void> setFullscreen(bool on) async {
  fullscreen.value = on;
  await review_api.setSetting(key: 'mobile.fullscreen', value: on ? '1' : '0');
  await applyFullscreen();
}

Future<void> applyFullscreen() => SystemChrome.setEnabledSystemUIMode(
  fullscreen.value ? SystemUiMode.immersiveSticky : SystemUiMode.edgeToEdge,
);
