import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data' show Float32List;

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show Int64List;

import 'browser.dart';
import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as api;
import 'waveform.dart';
import 'l10n.dart';

/// All regions from all recordings, grouped by recording, with previews,
/// export flags and the export itself. Mirrors the desktop review page.
class ReviewPage extends StatefulWidget {
  final bool active;
  final void Function(String path, {int? region}) onOpen;

  const ReviewPage({super.key, required this.active, required this.onOpen});

  @override
  State<ReviewPage> createState() => _ReviewPageState();
}

class _ReviewPageState extends State<ReviewPage> {
  late final List<Color> _palette = engine
      .palette()
      .map((c) => Color(0xff000000 | c.rgb))
      .toList();
  // Getters: names follow the interface language.
  List<String> get _colorNames => engine.palette().map((c) => c.name).toList();
  List<api.FormatOption> get _formats => api.outputFormats();

  List<api.ReviewRegion> _rows = [];
  bool _loaded = false;
  int? _colorFilter;
  bool _onlyExport = false;

  /// Landscape: filters and export in a panel on the right.
  bool _panelOpen = true;

  /// Region whose preview is playing.
  int? _playing;
  Timer? _timer;

  late final _template = TextEditingController(
    text: api.getSetting(key: 'export.template') ?? api.defaultTemplate(),
  );
  late String _format = api.getSetting(key: 'export.format') ?? 'source';

  @override
  void initState() {
    super.initState();
    if (widget.active) _enter();
  }

  @override
  void didUpdateWidget(ReviewPage old) {
    super.didUpdateWidget(old);
    if (widget.active && !old.active) _enter();
    if (!widget.active && old.active) _leave();
  }

  @override
  void dispose() {
    _timer?.cancel();
    _template.dispose();
    super.dispose();
  }

  void _enter() {
    _refresh();
    // Redraw mini waveforms while summaries are computed, and notice when a
    // preview reaches its end.
    _timer = Timer.periodic(const Duration(milliseconds: 400), (_) {
      if (!mounted) return;
      setState(() {
        if (_playing != null && !engine.isPlaying()) _playing = null;
      });
    });
  }

  void _leave() {
    _timer?.cancel();
    _timer = null;
    api.stopPreview();
    api.reviewClose();
    _playing = null;
  }

  Future<void> _refresh() async {
    final rows = await api.allRegions();
    if (mounted) {
      setState(() {
        _rows = rows;
        _loaded = true;
      });
    }
  }

  Iterable<api.ReviewRegion> get _visible => _rows.where(
    (r) =>
        (_colorFilter == null || r.color == _colorFilter) &&
        (!_onlyExport || r.export_),
  );

  Iterable<api.ReviewRegion> get _toExport => _rows.where((r) => r.export_);

  void _toast(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _edit(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _refresh();
  }

  void _togglePreview(api.ReviewRegion r) async {
    if (_playing == r.id) {
      api.stopPreview();
      setState(() => _playing = null);
      return;
    }
    if (!r.available) {
      _toast(
        tr(
          'Запись недоступна. Подключите рекордер',
          'The recording is not available. Connect the recorder',
        ),
      );
      return;
    }
    setState(() => _playing = r.id);
    // A changed speed is heard as it will be exported.
    if (r.speed != 100) {
      try {
        await api.previewProcessed(id: r.id, from: 0);
      } catch (e) {
        _toast(friendlyError(e));
        setState(() => _playing = null);
      }
    } else {
      api.previewSource(path: r.path, start: r.start, end: r.end);
    }
  }

  Future<void> _rename(api.ReviewRegion r) async {
    final controller = TextEditingController(text: r.name);
    final name = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(tr('Имя региона', 'Region name')),
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
            child: Text(tr('Сохранить', 'Save')),
          ),
        ],
      ),
    );
    if (name != null) _edit(() => api.reviewRename(id: r.id, name: name));
  }

  // ---- Export ----

  Future<void> _export() async {
    final ids = _toExport.map((r) => r.id).toList();
    if (ids.isEmpty) {
      _toast(
        tr(
          'Нет регионов, отмеченных к экспорту',
          'No regions are marked for export',
        ),
      );
      return;
    }
    final idList = Int64List.fromList(ids);
    final missing = await api.exportMissing(ids: idList);
    if (missing.isNotEmpty) {
      _toast(
        tr(
          'Нет доступа к записям (${missing.length}). Подключите рекордер и попробуйте снова',
          'Recordings not available (${missing.length}). Connect the recorder and try again',
        ),
      );
      return;
    }
    if (!mounted) return;
    final dest = await pickFolder(
      context,
      title: tr('Куда сохранить', 'Where to save'),
    );
    if (dest == null || !mounted) return;
    api.setSetting(key: 'export.template', value: _template.text);
    api.setSetting(key: 'export.format', value: _format);
    api.stopPreview();
    setState(() => _playing = null);

    final names = await api.exportPreview(
      ids: idList,
      dest: dest,
      template: _template.text,
    );
    if (!mounted) return;
    final go = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(
          tr(
            'Сохранить ${names.length} файлов?',
            'Save ${names.length} files?',
          ),
        ),
        content: SizedBox(
          width: double.maxFinite,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(dest, style: Theme.of(context).textTheme.bodySmall),
              const SizedBox(height: 8),
              Flexible(
                child: ListView(
                  shrinkWrap: true,
                  children: [for (final n in names) Text(n)],
                ),
              ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(tr('Отмена', 'Cancel')),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(tr('Экспортировать', 'Export')),
          ),
        ],
      ),
    );
    if (go != true || !mounted) return;

    final progress = api.exportRegions(
      ids: idList,
      dest: dest,
      template: _template.text,
      format: _format,
    );
    final result = await showDialog<api.ExportProgress>(
      context: context,
      barrierDismissible: false,
      builder: (_) => _ExportDialog(progress: progress),
    );
    if (result == null || !mounted) return;
    if (result.cancelled) {
      _toast(
        tr(
          'Экспорт отменён. Сохранено файлов: ${result.written}',
          'Export cancelled. Files saved: ${result.written}',
        ),
      );
    } else if (result.errors.isNotEmpty) {
      _toast(
        tr(
          'Сохранено файлов: ${result.written}, ошибок: ${result.errors.length}. '
              'Первая: ${result.errors.first}',
          'Files saved: ${result.written}, errors: ${result.errors.length}. First: ${result.errors.first}',
        ),
      );
    } else {
      _toast(
        tr(
          'Сохранено файлов: ${result.written} в $dest',
          'Files saved: ${result.written} to $dest',
        ),
      );
    }
  }

  // ---- Build ----

  @override
  Widget build(BuildContext context) {
    final visible = _visible.toList();
    final files = _rows.map((r) => r.fileId).toSet().length;
    final landscape =
        MediaQuery.orientationOf(context) == Orientation.landscape;
    final list = !_loaded
        ? const Center(child: CircularProgressIndicator())
        : _rows.isEmpty
        ? Center(
            child: Padding(
              padding: EdgeInsets.all(24),
              child: Text(
                tr(
                  'Регионов пока нет. Выделите участок во вкладке «Разметка» '
                      'и нажмите «Регион».',
                  'No regions yet. Select a stretch on the Markup page and tap Region.',
                ),
                textAlign: TextAlign.center,
              ),
            ),
          )
        : ListView(
            padding: const EdgeInsets.only(bottom: 16),
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                child: Text(
                  tr(
                    'Регионов: ${_rows.length} в $files записях · к экспорту ${_toExport.length}',
                    '${_rows.length} regions in $files recordings · ${_toExport.length} to export',
                  ),
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
              ..._grouped(visible),
            ],
          );
    return Scaffold(
      appBar: AppBar(
        toolbarHeight: landscape ? 48 : null,
        title: Text(tr('Обзор регионов', 'Region review')),
        actions: [
          IconButton(
            tooltip: tr('Обновить', 'Refresh'),
            icon: const Icon(Icons.refresh),
            onPressed: _refresh,
          ),
          if (landscape)
            IconButton(
              tooltip: _panelOpen
                  ? tr('Скрыть фильтр и экспорт', 'Hide filter and export')
                  : tr('Фильтр и экспорт', 'Filter and export'),
              isSelected: _panelOpen,
              icon: const Icon(Icons.view_sidebar_outlined),
              selectedIcon: const Icon(Icons.view_sidebar),
              onPressed: () => setState(() => _panelOpen = !_panelOpen),
            ),
        ],
        bottom: landscape
            ? null
            : PreferredSize(
                preferredSize: const Size.fromHeight(52),
                child: _filters(),
              ),
      ),
      body: landscape
          ? Row(
              children: [
                Expanded(child: list),
                if (_panelOpen) ...[
                  const VerticalDivider(width: 1),
                  SizedBox(
                    width: math.min(
                      320,
                      MediaQuery.sizeOf(context).width * 0.34,
                    ),
                    child: _sidePanel(),
                  ),
                ],
              ],
            )
          : list,
      bottomNavigationBar: landscape ? null : _exportBar(),
    );
  }

  List<Widget> _filterChips() => [
    FilterChip(
      label: Text(tr('Только к экспорту', 'Export only')),
      selected: _onlyExport,
      onSelected: (v) => setState(() => _onlyExport = v),
    ),
    for (var i = 0; i < _palette.length; i++)
      Tooltip(
        message: _colorNames[i],
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: () =>
              setState(() => _colorFilter = _colorFilter == i ? null : i),
          child: Padding(
            padding: const EdgeInsets.all(4),
            child: CircleAvatar(
              radius: 13,
              backgroundColor: _palette[i],
              child: _colorFilter == i
                  ? const Icon(Icons.check, size: 16, color: Colors.black)
                  : null,
            ),
          ),
        ),
      ),
  ];

  /// Portrait: one scrolling row under the title.
  Widget _filters() {
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      child: Row(
        children: [
          for (final chip in _filterChips())
            Padding(padding: const EdgeInsets.only(right: 4), child: chip),
        ],
      ),
    );
  }

  List<Widget> _grouped(List<api.ReviewRegion> rows) {
    final out = <Widget>[];
    int? file;
    for (final r in rows) {
      if (r.fileId != file) {
        file = r.fileId;
        out.add(_fileHeader(r));
      }
      out.add(_row(r));
    }
    return out;
  }

  /// One short line per recording; the full name and path on long press.
  Widget _fileHeader(api.ReviewRegion r) {
    final theme = Theme.of(context);
    return InkWell(
      onLongPress: () => _showFile(r),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
        child: Row(
          children: [
            Icon(
              r.available ? Icons.graphic_eq : Icons.usb_off,
              size: 16,
              color: theme.colorScheme.onSurfaceVariant,
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                r.fileTitle,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelLarge?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  void _showFile(api.ReviewRegion r) {
    final name = r.path.split('/').last;
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SelectableText(
                r.fileTitle,
                style: Theme.of(context).textTheme.titleMedium,
              ),
              if (name != r.fileTitle) SelectableText(name),
              const SizedBox(height: 8),
              SelectableText(
                r.path,
                style: Theme.of(context).textTheme.bodySmall,
              ),
              if (!r.available)
                Padding(
                  padding: EdgeInsets.only(top: 8),
                  child: Text(
                    tr(
                      'Сейчас недоступна — подключите носитель',
                      'Not available now — connect the volume',
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  /// A region in its own frame: the waveform first, a short caption under it,
  /// the controls at the bottom.
  Widget _row(api.ReviewRegion r) {
    final theme = Theme.of(context);
    final color = _palette[r.color % _palette.length];
    final len = (r.end - r.start) / r.sampleRate;
    final note = regionNote(r);
    final channels = r.fileChannels == 1
        ? ''
        : switch (r.channels) {
            api.Channels.asSource => '',
            api.Channels.left => tr('только L', 'L only'),
            api.Channels.right => tr('только R', 'R only'),
            api.Channels.splitMono => tr('L и R отдельно', 'L and R apart'),
          };
    final details = [
      if (note.isNotEmpty) note.replaceFirst(' → ', '→ '),
      if (channels.isNotEmpty) channels,
    ].join(' · ');
    return Card.outlined(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onLongPress: () => _showFile(r),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(12, 12, 4, 0),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.only(right: 8),
                child: SizedBox(
                  height: 44,
                  child: LayoutBuilder(
                    builder: (context, c) => CustomPaint(
                      size: Size(c.maxWidth, 44),
                      painter: MiniWavePainter(
                        peaks: r.available
                            ? api.miniPeaks(
                                path: r.path,
                                channels: r.fileChannels,
                                totalFrames: r.totalFrames,
                                start: r.start,
                                end: r.end,
                                columns: waveColumns(
                                  (r.end - r.start).toDouble(),
                                  c.maxWidth,
                                ),
                              )
                            : engine.PeaksView(done: 0, data: Float32List(0)),
                        channels: r.fileChannels,
                        color: color,
                      ),
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              Padding(
                padding: const EdgeInsets.only(right: 8),
                child: Row(
                  children: [
                    Expanded(
                      child: GestureDetector(
                        onTap: () => _rename(r),
                        child: Text(
                          r.name.isEmpty ? tr('Без имени', 'Untitled') : r.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.titleSmall,
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Text(
                      tr(
                        '${len.toStringAsFixed(2)} с',
                        '${len.toStringAsFixed(2)} s',
                      ),
                      style: theme.textTheme.bodySmall,
                    ),
                  ],
                ),
              ),
              if (details.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(right: 8, top: 2),
                  child: Text(
                    details,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                ),
              Row(
                children: [
                  IconButton(
                    tooltip: tr('Прослушать', 'Listen'),
                    icon: Icon(
                      _playing == r.id ? Icons.stop : Icons.play_arrow,
                    ),
                    onPressed: () => _togglePreview(r),
                  ),
                  if (r.fileChannels > 1) _channelMenu(r),
                  const Spacer(),
                  IconButton(
                    tooltip: tr('В разметку', 'Open in markup'),
                    icon: const Icon(Icons.open_in_new),
                    onPressed: () => widget.onOpen(r.path, region: r.id),
                  ),
                  Checkbox(
                    value: r.export_,
                    onChanged: (v) => _edit(
                      () => api.reviewSetExport(id: r.id, export_: v ?? false),
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _channelMenu(api.ReviewRegion r) {
    final labels = {
      api.Channels.asSource: tr('Стерео', 'Stereo'),
      api.Channels.left: tr('Только L', 'L only'),
      api.Channels.right: tr('Только R', 'R only'),
      api.Channels.splitMono: tr('L и R отдельно', 'L and R apart'),
    };
    return PopupMenuButton<api.Channels>(
      tooltip: tr('Каналы при экспорте', 'Channels on export'),
      initialValue: r.channels,
      onSelected: (c) =>
          _edit(() => api.reviewSetChannels(id: r.id, channels: c)),
      itemBuilder: (_) => [
        for (final e in labels.entries)
          PopupMenuItem(value: e.key, child: Text(e.value)),
      ],
      icon: Icon(
        r.channels == api.Channels.asSource
            ? Icons.call_split
            : Icons.call_split_outlined,
        color: r.channels == api.Channels.asSource
            ? null
            : Theme.of(context).colorScheme.primary,
      ),
    );
  }

  Widget _templateField() => TextField(
    controller: _template,
    decoration: InputDecoration(
      isDense: true,
      labelText: tr('Имя файла', 'File name'),
      border: const OutlineInputBorder(),
      suffixIcon: Tooltip(
        message: api.templateHelp(),
        triggerMode: TooltipTriggerMode.tap,
        showDuration: const Duration(seconds: 8),
        child: const Icon(Icons.help_outline),
      ),
    ),
  );

  Widget _formatMenu({bool expanded = false}) => DropdownButton<String>(
    isExpanded: expanded,
    value: _formats.any((f) => f.key == _format) ? _format : _formats.first.key,
    onChanged: (v) => setState(() => _format = v ?? 'source'),
    items: [
      for (final f in _formats)
        DropdownMenuItem(value: f.key, child: Text(f.label)),
    ],
  );

  Widget _exportButton() => SizedBox(
    width: double.infinity,
    child: FilledButton.icon(
      icon: const Icon(Icons.save_alt),
      label: Text(
        tr(
          'Экспортировать (${_toExport.length})…',
          'Export (${_toExport.length})…',
        ),
      ),
      onPressed: _toExport.isEmpty ? null : _export,
    ),
  );

  /// Portrait: template, format and the export button under the list.
  Widget _exportBar() {
    return Material(
      elevation: 2,
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Row(
                children: [
                  Expanded(child: _templateField()),
                  const SizedBox(width: 8),
                  _formatMenu(),
                ],
              ),
              const SizedBox(height: 8),
              _exportButton(),
            ],
          ),
        ),
      ),
    );
  }

  /// Landscape: filters and export in a panel on the right, so the list
  /// keeps the whole height.
  Widget _sidePanel() {
    final theme = Theme.of(context);
    return SafeArea(
      left: false,
      child: ListView(
        padding: const EdgeInsets.all(12),
        children: [
          Text(tr('Фильтр', 'Filter'), style: theme.textTheme.titleSmall),
          const SizedBox(height: 8),
          Wrap(spacing: 4, runSpacing: 4, children: _filterChips()),
          const Divider(height: 32),
          Text(tr('Экспорт', 'Export'), style: theme.textTheme.titleSmall),
          const SizedBox(height: 12),
          _templateField(),
          const SizedBox(height: 8),
          _formatMenu(expanded: true),
          const SizedBox(height: 12),
          _exportButton(),
        ],
      ),
    );
  }
}

class _ExportDialog extends StatefulWidget {
  final Stream<api.ExportProgress> progress;
  const _ExportDialog({required this.progress});

  @override
  State<_ExportDialog> createState() => _ExportDialogState();
}

class _ExportDialogState extends State<_ExportDialog> {
  late final StreamSubscription<api.ExportProgress> _sub;
  double _fraction = 0;
  bool _cancelling = false;

  @override
  void initState() {
    super.initState();
    _sub = widget.progress.listen(
      (p) {
        if (!mounted) return;
        if (p.finished) {
          Navigator.of(context).pop(p);
          return;
        }
        setState(() => _fraction = p.total == 0 ? 0 : p.done / p.total);
      },
      onError: (Object e) {
        if (!mounted) return;
        Navigator.of(context).pop(
          api.ExportProgress(
            done: 0,
            total: 0,
            finished: true,
            written: 0,
            errors: [friendlyError(e)],
            cancelled: false,
          ),
        );
      },
    );
  }

  @override
  void dispose() {
    _sub.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(tr('Экспорт', 'Export')),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          LinearProgressIndicator(value: _fraction),
          const SizedBox(height: 8),
          Text('${(_fraction * 100).floor()} %'),
        ],
      ),
      actions: [
        TextButton(
          onPressed: _cancelling
              ? null
              : () {
                  setState(() => _cancelling = true);
                  api.cancelExport();
                },
          child: Text(tr('Отменить', 'Undo')),
        ),
      ],
    );
  }
}
