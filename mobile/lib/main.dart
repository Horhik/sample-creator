import 'package:dynamic_color/dynamic_color.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:flutter/services.dart';

import 'home.dart';
import 'src/rust/api/engine.dart' as engine;
import 'src/rust/frb_generated.dart';
import 'l10n.dart';

void main() => runApp(const SampleCreatorApp());

/// Loads the engine. Runs inside the app, so a failure shows up on screen
/// instead of leaving the splash forever.
Future<void> _startEngine() async {
  await RustLib.init();
  // Markup and caches live in the app's private storage, never next to the
  // recordings: the recorder card must not fill up.
  final data = await getApplicationSupportDirectory();
  final cache = await getApplicationCacheDirectory();
  await engine.init(dataDir: data.path, cacheDir: cache.path);
  loadLanguage();
}

final _engineReady = _startEngine();

class SampleCreatorApp extends StatelessWidget {
  const SampleCreatorApp({super.key});

  static const _seed = Color(0xff62a0ea);

  /// Follows the system: light or dark, and Material You colours taken from
  /// the wallpaper where Android offers them (12+).
  @override
  Widget build(BuildContext context) {
    // Rebuilt when the language changes, so every label follows.
    return ValueListenableBuilder(
      valueListenable: language,
      builder: (context, _, _) => DynamicColorBuilder(
        builder: (light, dark) => MaterialApp(
          key: ValueKey(isRussian),
          title: 'Sample Creator',
          debugShowCheckedModeBanner: false,
          themeMode: ThemeMode.system,
          theme: ThemeData(
            colorScheme:
                light?.harmonized() ?? ColorScheme.fromSeed(seedColor: _seed),
            useMaterial3: true,
          ),
          darkTheme: ThemeData(
            colorScheme:
                dark?.harmonized() ??
                ColorScheme.fromSeed(
                  seedColor: _seed,
                  brightness: Brightness.dark,
                ),
            useMaterial3: true,
          ),
          home: FutureBuilder(
            future: _engineReady,
            builder: (context, snap) {
              if (snap.hasError) {
                return _Failure("${snap.error}\n\n${snap.stackTrace}");
              }
              if (snap.connectionState != ConnectionState.done) {
                return const Scaffold(
                  body: Center(child: CircularProgressIndicator()),
                );
              }
              return const StorageGate();
            },
          ),
        ),
      ),
    );
  }
}

/// Recordings sit on shared storage or a USB card reader, so the app needs
/// "all files" access before it can list anything.
class StorageGate extends StatefulWidget {
  const StorageGate({super.key});

  @override
  State<StorageGate> createState() => _StorageGateState();
}

class _StorageGateState extends State<StorageGate> with WidgetsBindingObserver {
  bool? _granted;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _check();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  // Coming back from the settings screen.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _check();
  }

  Future<void> _check() async {
    final granted = await _storage.invokeMethod<bool>('granted') ?? false;
    if (mounted) setState(() => _granted = granted);
  }

  Future<void> _request() async {
    await _storage.invokeMethod<void>('request');
    await _check();
  }

  @override
  Widget build(BuildContext context) {
    switch (_granted) {
      case null:
        return const Scaffold(body: Center(child: CircularProgressIndicator()));
      case true:
        return const HomeShell();
      case false:
        return Scaffold(
          body: Center(
            child: Padding(
              padding: const EdgeInsets.all(32),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  const Icon(Icons.folder_off_outlined, size: 64),
                  const SizedBox(height: 16),
                  Text(
                    tr(
                      'Нужен доступ к файлам, чтобы открывать записи с телефона '
                          'и с рекордера. Приложение только читает их.',
                      'File access is needed to open recordings on the phone and on a recorder. The app only reads them.',
                    ),
                    textAlign: TextAlign.center,
                  ),
                  const SizedBox(height: 24),
                  FilledButton(
                    onPressed: _request,
                    child: Text(tr('Разрешить', 'Allow')),
                  ),
                ],
              ),
            ),
          ),
        );
    }
  }
}

/// Implemented in MainActivity.kt.
const _storage = MethodChannel('sample_creator/storage');

class _Failure extends StatelessWidget {
  final String text;
  const _Failure(this.text);

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(tr('Не удалось запустить', 'Could not start')),
      ),
      body: SingleChildScrollView(
        padding: const EdgeInsets.all(16),
        child: SelectableText(
          text,
          style: const TextStyle(fontFamily: 'monospace'),
        ),
      ),
    );
  }
}
