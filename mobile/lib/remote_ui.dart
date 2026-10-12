import 'dart:async';

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'browser.dart';
import 'l10n.dart';
import 'src/rust/api/remote.dart' as remote_api;
import 'src/rust/api/review.dart' as review_api;
import 'waveform.dart' show friendlyError;

/// A folder shared by Sample Creator on a computer.
class SharedFolder {
  final String ticket;
  final String name;

  /// Where its files appear once connected; null while offline.
  final String? root;
  const SharedFolder(this.ticket, this.name, this.root);
}

/// Folders shared with this phone, connected or not. Remembered (with their
/// codes) so they reconnect on the next start.
final sharedFolders = ValueNotifier<List<SharedFolder>>([]);

const _key = 'mobile.remotes';

void _save() {
  review_api.setSetting(
    key: _key,
    value: [
      for (final f in sharedFolders.value) '${f.name}\t${f.ticket}',
    ].join('\n'),
  );
}

/// Connects the remembered folders in the background.
Future<void> reconnectShared() async {
  final saved = (review_api.getSetting(key: _key) ?? '')
      .split('\n')
      .where((l) => l.contains('\t'))
      .map((l) => l.split('\t'))
      .map((p) => SharedFolder(p[1], p[0], null))
      .toList();
  sharedFolders.value = saved;
  for (final f in saved) {
    try {
      await connectShared(f.ticket);
    } catch (_) {
      // The computer is off or not sharing: stays listed as offline.
    }
  }
}

/// Connects with a code (from the QR) and remembers it.
Future<SharedFolder> connectShared(String ticket) async {
  final share = await remote_api.connectShare(ticket: ticket.trim());
  final folder = SharedFolder(ticket.trim(), share.name, share.root);
  sharedFolders.value = [
    for (final f in sharedFolders.value)
      if (f.ticket != folder.ticket) f,
    folder,
  ];
  _save();
  volumesChanged.value++;
  return folder;
}

void forgetShared(SharedFolder folder) {
  if (folder.root != null) remote_api.disconnectShare(root: folder.root!);
  sharedFolders.value = [
    for (final f in sharedFolders.value)
      if (f.ticket != folder.ticket) f,
  ];
  _save();
  volumesChanged.value++;
}

/// Scan the computer's QR code or paste its code.
class ConnectPage extends StatefulWidget {
  const ConnectPage({super.key});

  @override
  State<ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<ConnectPage> {
  final _code = TextEditingController();
  bool _busy = false;
  bool _scanning = false;
  String? _error;

  @override
  void dispose() {
    _code.dispose();
    super.dispose();
  }

  Future<void> _connect(String code) async {
    if (code.trim().isEmpty || _busy) return;
    setState(() {
      _busy = true;
      _scanning = false;
      _error = null;
    });
    try {
      final folder = await connectShared(code);
      if (mounted) Navigator.pop(context, folder);
    } catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = friendlyError(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Text(tr('Подключиться к компьютеру', 'Connect to a computer')),
      ),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text(
            tr(
              'На компьютере: правый клик по папке в дереве → «Поделиться папкой…». '
                  'Отсканируйте QR-код или вставьте код текстом.',
              'On the computer: right-click a folder in the tree → Share folder…. '
                  'Scan the QR code or paste the code.',
            ),
            style: theme.textTheme.bodyMedium,
          ),
          const SizedBox(height: 16),
          if (_scanning)
            AspectRatio(
              aspectRatio: 1,
              child: ClipRRect(
                borderRadius: BorderRadius.circular(12),
                child: MobileScanner(
                  onDetect: (capture) {
                    final value = capture.barcodes
                        .map((b) => b.rawValue)
                        .whereType<String>()
                        .firstWhere(
                          (v) => v.startsWith('sc1.'),
                          orElse: () => '',
                        );
                    if (value.isNotEmpty) {
                      _code.text = value;
                      _connect(value);
                    }
                  },
                ),
              ),
            )
          else
            FilledButton.tonalIcon(
              icon: const Icon(Icons.qr_code_scanner),
              label: Text(tr('Сканировать QR-код', 'Scan the QR code')),
              onPressed: _busy ? null : () => setState(() => _scanning = true),
            ),
          const SizedBox(height: 16),
          TextField(
            controller: _code,
            minLines: 1,
            maxLines: 4,
            decoration: InputDecoration(
              labelText: tr('Код', 'Code'),
              hintText: 'sc1.…',
              border: const OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          FilledButton.icon(
            icon: _busy
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.link),
            label: Text(tr('Подключиться', 'Connect')),
            onPressed: _busy ? null : () => _connect(_code.text),
          ),
          if (_busy)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                tr(
                  'Соединяюсь… вне домашней сети это может занять до 20 секунд.',
                  'Connecting… outside the home network this can take up to 20 seconds.',
                ),
                style: theme.textTheme.bodySmall,
              ),
            ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _error!,
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
        ],
      ),
    );
  }
}

/// A file on a computer is downloaded whole before it opens: the waveform and
/// playback then read from the phone. True when it can be opened.
Future<bool> ensureLocal(BuildContext context, String path) async {
  if (!remote_api.isRemotePath(path: path)) return true;
  if (await remote_api.hasLocalCopy(path: path)) return true;
  if (!context.mounted) return false;
  final ok = await showDialog<bool>(
    context: context,
    barrierDismissible: false,
    builder: (_) => _DownloadDialog(path),
  );
  return ok ?? false;
}

class _DownloadDialog extends StatefulWidget {
  final String path;
  const _DownloadDialog(this.path);

  @override
  State<_DownloadDialog> createState() => _DownloadDialogState();
}

class _DownloadDialogState extends State<_DownloadDialog> {
  late final StreamSubscription<remote_api.DownloadProgress> _sub;
  final _started = DateTime.now();
  int _done = 0;
  int _total = 0;
  String? _error;

  @override
  void initState() {
    super.initState();
    _sub = remote_api.downloadRemote(path: widget.path).listen((p) {
      if (!mounted) return;
      if (p.ready) {
        Navigator.pop(context, true);
      } else if (p.error != null) {
        setState(() => _error = p.error);
      } else {
        setState(() {
          _done = p.done;
          _total = p.total;
        });
      }
    });
  }

  @override
  void dispose() {
    _sub.cancel();
    super.dispose();
  }

  void _cancel() {
    if (_error == null) remote_api.cancelDownload();
    Navigator.pop(context, false);
  }

  static String _mb(int bytes) => (bytes / 1048576).toStringAsFixed(1);

  static String _left(int s) {
    final time = s < 60
        ? '$s $sec'
        : '${s ~/ 60} ${tr('мин', 'min')} ${s % 60} $sec';
    return tr('ещё $time', '$time left');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final seconds = DateTime.now().difference(_started).inMilliseconds / 1000;
    final speed = seconds > 1 ? _done / seconds : 0.0;
    final left = speed > 0 ? ((_total - _done) / speed).round() : null;
    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _cancel();
      },
      child: AlertDialog(
        title: Text(
          tr('Загрузка с компьютера', 'Downloading from the computer'),
        ),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              widget.path.split('/').last,
              style: theme.textTheme.titleSmall,
              overflow: TextOverflow.ellipsis,
            ),
            const SizedBox(height: 16),
            if (_error == null) ...[
              LinearProgressIndicator(
                value: _total > 0 ? _done / _total : null,
              ),
              const SizedBox(height: 8),
              Text(
                _total > 0
                    ? '${_mb(_done)} / ${_mb(_total)} ${tr('МБ', 'MB')}'
                          '${speed > 0 ? ' · ${_mb(speed.round())} ${tr('МБ/с', 'MB/s')}' : ''}'
                          '${left != null && left > 0 ? ' · ${_left(left)}' : ''}'
                    : tr('Соединяюсь…', 'Connecting…'),
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 8),
              Text(
                tr(
                  'Файл откроется, когда загрузится целиком. Копия хранится '
                      'в кэше приложения и открывается даже без компьютера.',
                  'The file opens once it is downloaded whole. The copy stays '
                      'in the app cache and opens even without the computer.',
                ),
                style: theme.textTheme.bodySmall,
              ),
            ] else
              Text(_error!, style: TextStyle(color: theme.colorScheme.error)),
          ],
        ),
        actions: [
          TextButton(
            onPressed: _cancel,
            child: Text(
              _error == null ? tr('Отмена', 'Cancel') : tr('Закрыть', 'Close'),
            ),
          ),
        ],
      ),
    );
  }
}
