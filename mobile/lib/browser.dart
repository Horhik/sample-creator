import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as review_api;
import 'src/rust/api/sync.dart' as sync;
import 'display.dart';
import 'remote_ui.dart';
import 'src/rust/api/remote.dart' as remote_api;
import 'sync_ui.dart';
import 'l10n.dart';
import 'waveform.dart' show friendlyError;

const _storage = MethodChannel('sample_creator/storage');

/// Bumped when Android mounts or unmounts a volume (MainActivity.kt).
final volumesChanged = ValueNotifier<int>(0);
var _listening = false;

void _listen() {
  if (_listening) return;
  _listening = true;
  _storage.setMethodCallHandler((call) async {
    if (call.method == 'volumesChanged') volumesChanged.value++;
  });
}

class Volume {
  final String label;
  final String path;
  final bool removable;
  const Volume(this.label, this.path, this.removable);
}

/// Mounted volumes as Android reports them: internal storage, SD card, and a
/// recorder or card reader plugged in over USB OTG.
Future<List<Volume>> storageVolumes() async {
  _listen();
  final volumes = <Volume>[];
  try {
    final list = await _storage.invokeListMethod<Map>('volumes') ?? [];
    for (final v in list) {
      volumes.add(
        Volume(
          '${v['label'] ?? v['path']}',
          '${v['path']}',
          v['removable'] == true,
        ),
      );
    }
  } on PlatformException {
    // Fall back to what /storage shows below.
  }
  // Anything else mounted under /storage that the system list missed.
  try {
    for (final e in Directory('/storage').listSync()) {
      final name = e.path.split('/').last;
      if (name == 'emulated' || name == 'self') continue;
      if (volumes.any((v) => v.path == e.path)) continue;
      volumes.add(Volume(tr('Носитель $name', 'Volume $name'), e.path, true));
    }
  } on FileSystemException {
    // Not listable on many devices.
  }
  if (!volumes.any((v) => v.path == '/storage/emulated/0')) {
    volumes.insert(
      0,
      Volume(tr('Телефон', 'Phone'), '/storage/emulated/0', false),
    );
  }
  return volumes;
}

/// Walks the file system one folder at a time: the folder's subfolders and
/// recordings, with an "up" button. `null` folder means the list of volumes.
class FileBrowser extends StatefulWidget {
  /// Remembers the folder between launches under this settings key.
  final String settingsKey;

  /// Tapping a recording. When `null`, only folders are shown (folder picker).
  final void Function(String path)? onFile;

  /// The recording currently open, highlighted.
  final String? current;

  /// Reports the folder being shown, e.g. for a "choose this folder" button.
  final void Function(String? dir)? onDir;

  /// App settings under the list of volumes (the side panel shows them).
  final bool showSettings;

  const FileBrowser({
    super.key,
    required this.settingsKey,
    this.onFile,
    this.current,
    this.onDir,
    this.showSettings = false,
  });

  @override
  State<FileBrowser> createState() => FileBrowserState();
}

class FileBrowserState extends State<FileBrowser> {
  List<Volume> _volumes = [
    Volume(tr('Телефон', 'Phone'), '/storage/emulated/0', false),
  ];
  String? _dir;
  List<engine.Entry> _entries = [];
  String? _error;

  String? get dir => _dir;

  @override
  void initState() {
    super.initState();
    _loadVolumes();
    volumesChanged.addListener(_onVolumesChanged);
    sharedFolders.addListener(_onShared);
    final saved = review_api.getSetting(key: widget.settingsKey);
    final start =
        saved ??
        (widget.current == null
            ? null
            : Directory(widget.current!).parent.path);
    if (start != null) {
      // May be a folder on a computer that is not connected yet.
      remote_api.pathExists(path: start).then((ok) {
        if (ok && mounted && _dir == null) open(start);
      });
    }
  }

  @override
  void dispose() {
    volumesChanged.removeListener(_onVolumesChanged);
    sharedFolders.removeListener(_onShared);
    super.dispose();
  }

  void _onShared() {
    if (mounted) setState(() {});
  }

  Future<void> _loadVolumes() async {
    final volumes = await storageVolumes();
    if (mounted) setState(() => _volumes = volumes);
  }

  /// A volume came or went: refresh the list, leave a folder that vanished,
  /// and pick up markup from a volume that just arrived.
  void _onVolumesChanged() {
    _loadVolumes();
    syncAll();
    final dir = _dir;
    if (dir != null) {
      remote_api.pathExists(path: dir).then((ok) {
        if (!ok && mounted && _dir == dir) open(null);
      });
    }
  }

  bool _isRoot(String dir) =>
      _volumes.any((v) => v.path == dir) ||
      sharedFolders.value.any((f) => f.root == dir);

  Future<void> _openShared(SharedFolder f) async {
    var root = f.root;
    if (root == null) {
      try {
        root = (await connectShared(f.ticket)).root;
      } catch (e) {
        if (mounted) {
          ScaffoldMessenger.of(
            context,
          ).showSnackBar(SnackBar(content: Text(friendlyError(e))));
        }
        return;
      }
    }
    if (root != null) open(root);
  }

  void _sharedMenu(SharedFolder f) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.computer),
              title: Text(f.name),
              subtitle: Text(
                f.root == null
                    ? tr('Не подключено', 'Not connected')
                    : tr('Подключено', 'Connected'),
              ),
            ),
            ListTile(
              leading: const Icon(Icons.link_off),
              title: Text(tr('Забыть этот компьютер', 'Forget this computer')),
              onTap: () {
                Navigator.pop(context);
                forgetShared(f);
              },
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _volumeMenu(Volume v) async {
    await showVolumeSheet(context, v);
    if (mounted) setState(() {});
  }

  Future<void> open(String? dir) async {
    if (dir == null) {
      _loadVolumes();
      setState(() {
        _dir = null;
        _error = null;
      });
      widget.onDir?.call(null);
      return;
    }
    try {
      final entries = await engine.listDir(path: dir);
      if (!mounted) return;
      setState(() {
        _dir = dir;
        _entries = widget.onFile == null
            ? entries.where((e) => e.isDir).toList()
            : entries;
        _error = null;
      });
      review_api.setSetting(key: widget.settingsKey, value: dir);
      widget.onDir?.call(dir);
    } catch (e) {
      setState(() => _error = friendlyError(e));
    }
  }

  void up() {
    final dir = _dir;
    if (dir == null) return;
    open(_isRoot(dir) ? null : Directory(dir).parent.path);
  }

  void refresh() => open(_dir);

  @override
  Widget build(BuildContext context) {
    final dir = _dir;
    return Column(
      children: [
        ListTile(
          leading: dir == null
              ? const Icon(Icons.storage)
              : IconButton(
                  tooltip: tr('Вверх', 'Up'),
                  icon: const Icon(Icons.arrow_upward),
                  onPressed: up,
                ),
          title: Text(
            dir == null ? tr('Носители', 'Volumes') : dir.split('/').last,
            overflow: TextOverflow.ellipsis,
            style: Theme.of(context).textTheme.titleMedium,
          ),
          subtitle: dir == null
              ? null
              : Text(dir, overflow: TextOverflow.ellipsis, maxLines: 1),
          trailing: IconButton(
            tooltip: tr('Обновить', 'Refresh'),
            icon: const Icon(Icons.refresh),
            onPressed: dir == null ? _loadVolumes : refresh,
          ),
        ),
        const Divider(height: 1),
        Expanded(child: dir == null ? _roots() : _listing()),
      ],
    );
  }

  Widget _roots() {
    return ListView(
      children: [
        for (final f in sharedFolders.value)
          ListTile(
            leading: Icon(
              Icons.computer,
              color: f.root == null
                  ? Theme.of(context).disabledColor
                  : Theme.of(context).colorScheme.primary,
            ),
            title: Text(f.name),
            subtitle: Text(
              f.root == null
                  ? tr('Компьютер · не подключено', 'Computer · not connected')
                  : tr('Компьютер', 'Computer'),
            ),
            onTap: () => _openShared(f),
            onLongPress: () => _sharedMenu(f),
            trailing: IconButton(
              icon: const Icon(Icons.more_vert),
              onPressed: () => _sharedMenu(f),
            ),
          ),
        for (final v in _volumes)
          ListTile(
            leading: Icon(v.removable ? Icons.usb : Icons.smartphone),
            title: Text(v.label),
            subtitle: Text(
              sync.syncInfo(root: v.path) == null
                  ? v.path
                  : tr(
                      '${v.path} · разметка на носителе',
                      '${v.path} · markup on the volume',
                    ),
            ),
            onTap: () => open(v.path),
            onLongPress: () => _volumeMenu(v),
            trailing: IconButton(
              tooltip: tr(
                'Файл разметки на носителе',
                'Markup file on the volume',
              ),
              icon: Icon(
                sync.syncInfo(root: v.path) == null
                    ? Icons.more_vert
                    : Icons.sync,
              ),
              onPressed: () => _volumeMenu(v),
            ),
          ),
        Padding(
          padding: EdgeInsets.all(16),
          child: Text(
            tr(
              'Рекордер по USB появится здесь, когда Android его подключит: '
                  'на рекордере выберите режим накопителя (STORAGE).',
              'A recorder on USB shows up here once Android mounts it: set the recorder to storage mode.',
            ),
            style: TextStyle(fontSize: 12),
          ),
        ),
        if (widget.onFile != null)
          ListTile(
            leading: const Icon(Icons.add_link),
            title: Text(
              tr('Подключиться к компьютеру', 'Connect to a computer'),
            ),
            subtitle: Text(
              tr(
                'Папка, открытая в Sample Creator на компьютере',
                'A folder shared by Sample Creator on a computer',
              ),
            ),
            onTap: () async {
              final folder = await Navigator.of(context).push<SharedFolder>(
                MaterialPageRoute(builder: (_) => const ConnectPage()),
              );
              if (folder?.root != null) open(folder!.root);
            },
          ),
        if (widget.showSettings) ...[
          const Divider(),
          ValueListenableBuilder(
            valueListenable: language,
            builder: (context, choice, _) => ListTile(
              leading: const Icon(Icons.translate),
              title: Text(tr('Язык', 'Language')),
              trailing: DropdownButton<String>(
                value: choice,
                onChanged: (v) => setLanguage(v ?? 'system'),
                items: [
                  DropdownMenuItem(
                    value: 'system',
                    child: Text(tr('Как в системе', 'System')),
                  ),
                  const DropdownMenuItem(value: 'en', child: Text('English')),
                  const DropdownMenuItem(value: 'ru', child: Text('Русский')),
                ],
              ),
            ),
          ),
          ValueListenableBuilder(
            valueListenable: fullscreen,
            builder: (context, on, _) => SwitchListTile(
              secondary: const Icon(Icons.fullscreen),
              title: Text(tr('Полноэкранный режим', 'Full screen')),
              subtitle: Text(
                tr(
                  'Скрыть строку состояния и системные кнопки',
                  'Hide the status bar and system buttons',
                ),
              ),
              value: on,
              onChanged: setFullscreen,
            ),
          ),
        ],
      ],
    );
  }

  Widget _listing() {
    if (_error != null) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Text(_error!, textAlign: TextAlign.center),
        ),
      );
    }
    if (_entries.isEmpty) {
      return Center(
        child: Text(
          widget.onFile == null
              ? tr('Здесь нет папок', 'No folders here')
              : tr(
                  'Здесь нет папок и записей',
                  'No folders or recordings here',
                ),
        ),
      );
    }
    final scheme = Theme.of(context).colorScheme;
    return ListView.builder(
      // A new folder starts at its top, not where the last one was scrolled.
      key: ValueKey(_dir),
      itemCount: _entries.length,
      itemBuilder: (context, i) {
        final e = _entries[i];
        if (e.isDir) {
          return ListTile(
            leading: const Icon(Icons.folder_outlined),
            title: Text(e.name),
            onTap: () => open(e.path),
          );
        }
        final badges = [
          if (e.regions > 0) tr('${e.regions} рег.', '${e.regions} reg.'),
          if (e.markers > 0) tr('${e.markers} марк.', '${e.markers} mark.'),
        ].join(' · ');
        return ListTile(
          selected: e.path == widget.current,
          leading: Icon(
            e.reviewed ? Icons.check_circle_outline : Icons.graphic_eq,
            color: e.regions > 0 ? scheme.primary : null,
          ),
          title: Text(e.alias ?? e.name),
          subtitle: Text(
            [
              if (e.alias != null) e.name,
              badges,
            ].where((s) => s.isNotEmpty).join(' · '),
          ),
          onTap: () => widget.onFile?.call(e.path),
        );
      },
    );
  }
}

/// Full-screen folder chooser, e.g. for the export destination.
Future<String?> pickFolder(
  BuildContext context, {
  required String title,
  String settingsKey = 'mobile.export_browser',
}) {
  return Navigator.of(context).push<String>(
    MaterialPageRoute(
      builder: (_) => _FolderPicker(title: title, settingsKey: settingsKey),
    ),
  );
}

class _FolderPicker extends StatefulWidget {
  final String title;
  final String settingsKey;
  const _FolderPicker({required this.title, required this.settingsKey});

  @override
  State<_FolderPicker> createState() => _FolderPickerState();
}

class _FolderPickerState extends State<_FolderPicker> {
  final _browser = GlobalKey<FileBrowserState>();
  String? _dir;

  Future<void> _newFolder() async {
    final dir = _dir;
    if (dir == null) return;
    final controller = TextEditingController();
    final name = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(tr('Новая папка', 'New folder')),
        content: TextField(
          controller: controller,
          autofocus: true,
          onSubmitted: (v) => Navigator.pop(context, v),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: Text(tr('Отмена', 'Cancel')),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: Text(tr('Создать', 'Create')),
          ),
        ],
      ),
    );
    if (name == null || name.trim().isEmpty) return;
    final created = Directory('$dir/${name.trim()}');
    try {
      await created.create();
      _browser.currentState?.open(created.path);
    } on FileSystemException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text(
              tr(
                'Не создать папку: ${e.message}',
                'Cannot create the folder: ${e.message}',
              ),
            ),
          ),
        );
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.title),
        actions: [
          if (_dir != null)
            IconButton(
              tooltip: tr('Новая папка', 'New folder'),
              icon: const Icon(Icons.create_new_folder_outlined),
              onPressed: _newFolder,
            ),
        ],
      ),
      body: FileBrowser(
        key: _browser,
        settingsKey: widget.settingsKey,
        onDir: (d) => setState(() => _dir = d),
      ),
      bottomNavigationBar: SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: FilledButton.icon(
            icon: const Icon(Icons.check),
            label: Text(
              _dir == null
                  ? tr('Выберите папку', 'Pick a folder')
                  : tr('Сохранить сюда', 'Save here'),
            ),
            onPressed: _dir == null ? null : () => Navigator.pop(context, _dir),
          ),
        ),
      ),
    );
  }
}
