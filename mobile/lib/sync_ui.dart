import 'package:flutter/material.dart';

import 'browser.dart';
import 'l10n.dart';
import 'src/rust/api/sync.dart' as sync;
import 'waveform.dart' show friendlyError;

/// Bumped when a sync brought markup from a volume, so open pages reload.
final markupChanged = ValueNotifier<int>(0);

/// Syncs every plugged-in volume with a state file. Errors are only reported
/// when [context] is given (silent on mount and on leaving the app).
Future<void> syncAll([BuildContext? context]) async {
  try {
    final r = await sync.syncAllVolumes();
    if (r.imported > 0 || r.relinked > 0) markupChanged.value++;
    if (context != null && context.mounted && r.errors.isNotEmpty) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(r.errors.join('\n'))));
    }
  } catch (_) {
    // The library is always the source of truth; the file catches up later.
  }
}

String _when(int? ms) {
  if (ms == null) return tr('ещё не было', 'not yet');
  final t = DateTime.fromMillisecondsSinceEpoch(ms);
  String two(int v) => v.toString().padLeft(2, '0');
  return '${two(t.day)}.${two(t.month)} ${two(t.hour)}:${two(t.minute)}';
}

/// The volume menu: keep this volume's markup in a file on it.
Future<void> showVolumeSheet(BuildContext context, Volume v) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (_) => _VolumeSheet(volume: v),
  );
}

class _VolumeSheet extends StatefulWidget {
  final Volume volume;
  const _VolumeSheet({required this.volume});

  @override
  State<_VolumeSheet> createState() => _VolumeSheetState();
}

class _VolumeSheetState extends State<_VolumeSheet> {
  bool _busy = false;
  String? _message;

  String get _root => widget.volume.path;

  Future<void> _run(Future<sync.SyncReport> Function() action) async {
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      final r = await action();
      if (r.imported > 0 || r.relinked > 0) markupChanged.value++;
      _message = r.errors.isNotEmpty
          ? r.errors.join('\n')
          : tr(
              'Готово: с носителя записей — ${r.imported}, на носитель — ${r.exported}',
              'Done: recordings from the volume — ${r.imported}, to the volume — ${r.exported}',
            );
    } catch (e) {
      _message = friendlyError(e);
    }
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _create({required bool here}) async {
    var location = _root;
    if (!here) {
      final picked = await pickFolder(
        context,
        title: tr('Где хранить файл разметки', 'Where to keep the markup file'),
        settingsKey: 'mobile.sync_browser',
      );
      if (picked == null) return;
      location = picked;
    }
    await _run(
      () => sync.enableSync(root: _root, location: location, mustExist: false),
    );
  }

  Future<void> _find() async {
    final picked = await pickFolder(
      context,
      title: tr(
        'Папка с ${sync.stateFileName()}',
        'Folder with ${sync.stateFileName()}',
      ),
      settingsKey: 'mobile.sync_browser',
    );
    if (picked == null) return;
    await _run(
      () => sync.enableSync(root: _root, location: picked, mustExist: true),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final info = sync.syncInfo(root: _root);
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(widget.volume.label, style: theme.textTheme.titleLarge),
            Text(_root, style: theme.textTheme.bodySmall),
            const SizedBox(height: 16),
            Text(
              tr('Файл разметки', 'Markup file'),
              style: theme.textTheme.titleMedium,
            ),
            const SizedBox(height: 4),
            if (info == null) ...[
              Text(
                tr(
                  'Разметка (регионы, маркеры, имена, нарезка) сейчас хранится '
                      'только в приложении. Можно держать её копию в файле '
                      '${sync.stateFileName()} на этом носителе — тогда она поедет '
                      'вместе с записями на другое устройство. Синхронизация сама '
                      'по себе: при подключении носителя, смене записи и выходе из '
                      'приложения. Побеждает более поздняя правка каждой записи.',
                  'The markup (regions, markers, names, cuts) lives only in the app now. A copy can be kept in ${sync.stateFileName()} on this volume, so it travels with the recordings to another device. It syncs by itself: when the volume is connected, when you switch recordings and when you leave the app. The later edit of each recording wins.',
                ),
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              FilledButton.icon(
                icon: const Icon(Icons.note_add_outlined),
                label: Text(
                  tr('Создать в корне носителя', 'Create in the volume root'),
                ),
                onPressed: _busy ? null : () => _create(here: true),
              ),
              OutlinedButton.icon(
                icon: const Icon(Icons.create_new_folder_outlined),
                label: Text(tr('Создать в папке…', 'Create in a folder…')),
                onPressed: _busy ? null : () => _create(here: false),
              ),
              OutlinedButton.icon(
                icon: const Icon(Icons.search),
                label: Text(
                  tr('Указать существующий…', 'Use an existing one…'),
                ),
                onPressed: _busy ? null : _find,
              ),
            ] else ...[
              Text(info.file, style: theme.textTheme.bodyMedium),
              Text(
                info.available
                    ? tr(
                        'Последняя синхронизация: ${_when(info.lastSync)}',
                        'Last sync: ${_when(info.lastSync)}',
                      )
                    : tr(
                        'Носитель не подключён — синхронизируется при подключении',
                        'Volume not connected — it syncs when connected',
                      ),
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              FilledButton.icon(
                icon: const Icon(Icons.sync),
                label: Text(tr('Синхронизировать сейчас', 'Sync now')),
                onPressed: _busy || !info.available
                    ? null
                    : () => _run(() => sync.syncVolume(root: _root)),
              ),
              TextButton.icon(
                icon: const Icon(Icons.sync_disabled),
                label: Text(
                  tr(
                    'Отключить (файл останется на носителе)',
                    'Turn off (the file stays on the volume)',
                  ),
                ),
                onPressed: _busy
                    ? null
                    : () async {
                        await sync.disableSync(root: _root);
                        if (mounted) {
                          setState(
                            () => _message = tr('Отключено', 'Turned off'),
                          );
                        }
                      },
              ),
            ],
            if (_busy) const LinearProgressIndicator(),
            if (_message != null)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(_message!, style: theme.textTheme.bodySmall),
              ),
          ],
        ),
      ),
    );
  }
}
