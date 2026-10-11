import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import 'parts_editor.dart';
import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as api;
import 'timeline.dart';
import 'waveform.dart';
import 'l10n.dart';

/// Per-region processing, as on the desktop: all regions in a list, the
/// selected one's settings on top: speed, and cutting into parts that are
/// spliced back with fades.
class ProcessingPage extends StatefulWidget {
  final bool active;
  const ProcessingPage({super.key, required this.active});

  @override
  State<ProcessingPage> createState() => _ProcessingPageState();
}

const _presets = [50, 75, 100, 125, 150, 200, 300];

enum _Preview { none, original, result, rendering }

class _ProcessingPageState extends State<ProcessingPage> {
  late final List<Color> _palette = engine
      .palette()
      .map((c) => Color(0xff000000 | c.rgb))
      .toList();

  List<api.ReviewRegion> _rows = [];
  int? _selected;

  /// The slider moves freely; the library is written when the finger lifts.
  double? _dragSpeed;
  _Preview _preview = _Preview.none;

  Timer? _timer;

  /// Bumped on every edit, so a slow render of old settings is not played.
  int _generation = 0;

  /// Where the next cut goes, on the output timeline.
  int? _cursor;
  int? _selectedPart;
  int? _playhead;

  @override
  void initState() {
    super.initState();
    if (widget.active) _enter();
  }

  @override
  void didUpdateWidget(ProcessingPage old) {
    super.didUpdateWidget(old);
    if (widget.active && !old.active) _enter();
    if (!widget.active && old.active) {
      _timer?.cancel();
      _stop();
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  void _enter() {
    _refresh();
    // A preview that ran to its end resets the buttons.
    _timer = Timer.periodic(const Duration(milliseconds: 50), (_) {
      if (!mounted) return;
      final playing =
          _preview == _Preview.original || _preview == _Preview.result;
      if (playing && !engine.isPlaying()) {
        setState(() {
          _preview = _Preview.none;
          _playhead = null;
        });
      } else if (_preview == _Preview.result) {
        // Follow the result on the timeline (undo the speed stretch).
        final speed = _region?.speed ?? 100;
        setState(() => _playhead = engine.position() * speed ~/ 100);
      }
    });
  }

  Future<void> _refresh() async {
    final rows = await api.allRegions();
    if (!mounted) return;
    setState(() {
      _rows = rows;
      // Nothing is picked for the user: the page opens on the plain list.
      if (!rows.any((r) => r.id == _selected)) _selected = null;
    });
  }

  api.ReviewRegion? get _region =>
      _rows.where((r) => r.id == _selected).firstOrNull;

  void _stop() {
    api.stopPreview();
    _generation++;
    if (mounted) {
      setState(() {
        _preview = _Preview.none;
        _playhead = null;
      });
    }
  }

  /// Runs a parts edit, then reloads; the old preview no longer matches.
  Future<void> _edit(Future<void> Function() action) async {
    _stop();
    try {
      await action();
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _refresh();
  }

  Future<void> _cut(api.ReviewRegion r) async {
    final at = _cursor;
    if (at == null) {
      _toast(
        tr(
          'Сначала коснитесь ленты там, где резать',
          'Tap the timeline where to cut first',
        ),
      );
      return;
    }
    var done = false;
    await _edit(() async => done = await api.splitAtOutput(id: r.id, t: at));
    if (!done) {
      _toast(
        tr(
          'Под курсором нет части или это её край',
          'No part under the cursor, or it is an edge',
        ),
      );
    }
  }

  Future<void> _editFades(api.ReviewRegion r, int index) async {
    final res = await editPartDialog(context, r, index);
    if (res == null) return;
    await _edit(() => applyPartDialog(r, index, res));
  }

  Future<void> _setSpeed(api.ReviewRegion r, int speed, bool keepPitch) async {
    _stop();
    try {
      await api.reviewSetSpeed(id: r.id, speed: speed, keepPitch: keepPitch);
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _refresh();
  }

  void _toast(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _play(api.ReviewRegion r, {required bool result}) async {
    final wanted = result ? _Preview.result : _Preview.original;
    if (_preview == wanted) {
      _stop();
      return;
    }
    _stop();
    if (!r.available) {
      _toast(
        tr(
          'Запись недоступна. Подключите рекордер',
          'The recording is not available. Connect the recorder',
        ),
      );
      return;
    }
    if (!result) {
      api.previewSource(path: r.path, start: r.start, end: r.end);
      setState(() => _preview = _Preview.original);
      return;
    }
    final generation = _generation;
    setState(() => _preview = _Preview.rendering);
    try {
      await api.previewProcessed(id: r.id, from: 0);
      if (generation != _generation) {
        api.stopPreview();
        return;
      }
      setState(() => _preview = _Preview.result);
    } catch (e) {
      _toast(friendlyError(e));
      setState(() => _preview = _Preview.none);
    }
  }

  Future<void> _openEditor(api.ReviewRegion r) async {
    _stop();
    await Navigator.of(
      context,
    ).push(MaterialPageRoute(builder: (_) => PartsEditorPage(regionId: r.id)));
    await _refresh();
  }

  Future<void> _undo(bool redo) async {
    _stop();
    try {
      final id = redo ? await api.processingRedo() : await api.processingUndo();
      if (id != null) _selected = id;
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _refresh();
  }

  @override
  Widget build(BuildContext context) {
    final r = _region;
    return Scaffold(
      appBar: AppBar(
        title: Text(tr('Обработка', 'Processing')),
        actions: [
          IconButton(
            tooltip: tr('Отменить', 'Undo'),
            icon: const Icon(Icons.undo),
            onPressed: api.processingCanUndo() ? () => _undo(false) : null,
          ),
          IconButton(
            tooltip: tr('Повторить', 'Redo'),
            icon: const Icon(Icons.redo),
            onPressed: api.processingCanRedo() ? () => _undo(true) : null,
          ),
        ],
      ),
      body: _rows.isEmpty
          ? Center(
              child: Padding(
                padding: EdgeInsets.all(24),
                child: Text(
                  tr(
                    'Регионов пока нет. Создайте их во вкладке «Разметка».',
                    'No regions yet. Make them on the Markup page.',
                  ),
                  textAlign: TextAlign.center,
                ),
              ),
            )
          : ListView(
              children: [
                if (r != null) ...[
                  _settings(r),
                  _parts(r),
                  const Divider(height: 1),
                ] else
                  Padding(
                    padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                    child: Text(
                      tr(
                        'Выберите регион: скорость, нарезка и склейка.',
                        'Pick a region: speed, cutting and splicing.',
                      ),
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
                ..._list(),
              ],
            ),
    );
  }

  Widget _settings(api.ReviewRegion r) {
    final speed = _dragSpeed?.round() ?? r.speed;
    final len = (r.end - r.start) / r.sampleRate;
    // While dragging, estimate from the spliced length at the stored speed.
    final out = r.outputFrames * r.speed / speed / r.sampleRate;
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      r.name.isEmpty ? tr('Без имени', 'Untitled') : r.name,
                      style: theme.textTheme.titleLarge,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    Text(
                      r.fileTitle,
                      style: theme.textTheme.bodySmall,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ),
              ),
              IconButton(
                tooltip: tr('Закрыть', 'Close'),
                icon: const Icon(Icons.close),
                onPressed: () {
                  _stop();
                  setState(() {
                    _selected = null;
                    _cursor = null;
                    _selectedPart = null;
                  });
                },
              ),
            ],
          ),
          const SizedBox(height: 8),
          Row(
            children: [
              Text(
                tr(
                  'Скорость ×${(speed / 100).toStringAsFixed(2)}',
                  'Speed ×${(speed / 100).toStringAsFixed(2)}',
                ),
                style: theme.textTheme.titleMedium,
              ),
              const Spacer(),
              Text(
                tr(
                  '${len.toStringAsFixed(2)} с → ${out.toStringAsFixed(2)} с',
                  '${len.toStringAsFixed(2)} s → ${out.toStringAsFixed(2)} s',
                ),
                style: theme.textTheme.titleMedium,
              ),
            ],
          ),
          Slider(
            min: 25,
            max: 400,
            value: speed.toDouble().clamp(25, 400),
            onChanged: (v) => setState(() => _dragSpeed = v),
            onChangeEnd: (v) {
              _dragSpeed = null;
              _setSpeed(r, v.round(), r.keepPitch);
            },
          ),
          Wrap(
            spacing: 6,
            runSpacing: 4,
            children: [
              for (final p in _presets)
                ChoiceChip(
                  label: Text('×${p / 100}'.replaceAll('.0', '')),
                  selected: speed == p,
                  onSelected: (_) => _setSpeed(r, p, r.keepPitch),
                ),
            ],
          ),
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            title: Text(tr('Сохранять тон', 'Keep pitch')),
            subtitle: Text(
              r.keepPitch
                  ? tr('Меняется только темп', 'Only the tempo changes')
                  : tr(
                      'Как пластинка: быстрее — значит выше',
                      'Like a record: faster is higher',
                    ),
            ),
            value: r.keepPitch,
            onChanged: (v) => _setSpeed(r, speed, v),
          ),
          Row(
            children: [
              Expanded(
                child: OutlinedButton.icon(
                  icon: Icon(
                    _preview == _Preview.original
                        ? Icons.stop
                        : Icons.play_arrow,
                  ),
                  label: Text(tr('Оригинал', 'Original')),
                  onPressed: () => _play(r, result: false),
                ),
              ),
              const SizedBox(width: 12),
              Expanded(
                child: FilledButton.icon(
                  icon: _preview == _Preview.rendering
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : Icon(
                          _preview == _Preview.result
                              ? Icons.stop
                              : Icons.play_arrow,
                        ),
                  label: Text(tr('Результат', 'Result')),
                  onPressed: _preview == _Preview.rendering
                      ? null
                      : () => _play(r, result: true),
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _parts(api.ReviewRegion r) {
    final theme = Theme.of(context);
    final color = _palette[r.color % _palette.length];
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Divider(),
          Row(
            children: [
              Expanded(
                child: Text(
                  tr('Нарезка и склейка', 'Cut and splice'),
                  style: theme.textTheme.titleMedium,
                ),
              ),
              IconButton.filledTonal(
                tooltip: tr('Подробный режим', 'Full screen'),
                icon: const Icon(Icons.open_in_full),
                onPressed: () => _openEditor(r),
              ),
            ],
          ),
          Text(
            tr(
              'Лента результата: части подряд, по очереди сверху и снизу. '
                  'Коснитесь, где резать. Сдвиги, подрезка и фейды пальцем — в '
                  'подробном режиме.',
              'The result timeline: parts in a row, alternating top and bottom. Tap where to cut. Moving, trimming and fades by finger are in full screen.',
            ),
            style: theme.textTheme.bodySmall,
          ),
          const SizedBox(height: 8),
          SizedBox(
            height: 110,
            child: LayoutBuilder(
              builder: (context, c) {
                final clips = [for (final p in r.parts) Clip.of(p)];
                final pos = layoutClips(clips);
                final total = math.max(1, timelineLength(clips)).toDouble();
                final size = Size(c.maxWidth, 110);
                void place(Offset at) {
                  final t = (at.dx / c.maxWidth * total).round().clamp(
                    0,
                    total.toInt(),
                  );
                  setState(() {
                    _cursor = t;
                    _selectedPart = clipAt(clips, pos, t, at.dy, size);
                  });
                }

                return GestureDetector(
                  onTapDown: (d) => place(d.localPosition),
                  onHorizontalDragUpdate: (d) => place(d.localPosition),
                  child: CustomPaint(
                    size: size,
                    painter: TimelinePainter(
                      clips: clips,
                      positions: pos,
                      waves: clipWaves(r, clips, pos, 0, total, c.maxWidth),
                      channels: r.fileChannels,
                      viewStart: 0,
                      viewEnd: total,
                      color: color,
                      scheme: theme.colorScheme,
                      selected: _selectedPart,
                      cursor: _cursor,
                      playhead: _preview == _Preview.result ? _playhead : null,
                    ),
                  ),
                );
              },
            ),
          ),
          const SizedBox(height: 8),
          Row(
            children: [
              FilledButton.tonalIcon(
                icon: const Icon(Icons.content_cut),
                label: Text(tr('Разрезать', 'Cut')),
                onPressed: () => _cut(r),
              ),
              const SizedBox(width: 8),
              TextButton(
                onPressed: r.edited
                    ? () => _edit(() => api.resetParts(id: r.id))
                    : null,
                child: Text(tr('Сбросить', 'Reset')),
              ),
            ],
          ),
          if (r.parts.length > 1 || r.edited)
            for (var i = 0; i < r.parts.length; i++) _partTile(r, i),
        ],
      ),
    );
  }

  Widget _partTile(api.ReviewRegion r, int i) {
    final p = r.parts[i];
    String t(int f) => formatTime((f - r.start) / r.sampleRate);
    int toMs(int frames) => (frames * 1000 / r.sampleRate).round();
    final count = r.parts.length;
    return ListTile(
      contentPadding: EdgeInsets.zero,
      dense: true,
      selected: i == _selectedPart,
      leading: CircleAvatar(radius: 14, child: Text('${i + 1}')),
      title: Text(
        '${t(p.start)}–${t(p.end)} · '
        '${((p.end - p.start) / r.sampleRate).toStringAsFixed(2)} $sec',
      ),
      subtitle: Text(
        'fade in ${toMs(p.fadeIn)} $ms · out ${toMs(p.fadeOut)} $ms'
        '${overlapNote(r, i)}',
      ),
      onTap: () => _editFades(r, i),
      trailing: PopupMenuButton<String>(
        onSelected: (v) {
          switch (v) {
            case 'fades':
              _editFades(r, i);
            case 'up':
              _edit(() => api.movePart(id: r.id, index: i, up: true));
            case 'down':
              _edit(() => api.movePart(id: r.id, index: i, up: false));
            case 'remove':
              _edit(() => api.removePart(id: r.id, index: i));
          }
        },
        itemBuilder: (_) => [
          PopupMenuItem(
            value: 'fades',
            child: Text(tr('Фейды и наложение…', 'Fades and overlap…')),
          ),
          PopupMenuItem(
            value: 'up',
            enabled: i > 0,
            child: Text(tr('Раньше', 'Earlier')),
          ),
          PopupMenuItem(
            value: 'down',
            enabled: i + 1 < count,
            child: Text(tr('Позже', 'Later')),
          ),
          PopupMenuItem(
            value: 'remove',
            enabled: count > 1,
            child: Text(tr('Убрать часть', 'Remove part')),
          ),
        ],
      ),
    );
  }

  List<Widget> _list() {
    return [
      for (final r in _rows)
        ListTile(
          selected: r.id == _selected,
          leading: Icon(
            Icons.square,
            color: _palette[r.color % _palette.length],
          ),
          title: Text(r.name.isEmpty ? tr('Без имени', 'Untitled') : r.name),
          subtitle: Text(
            '${r.fileTitle} · ${((r.end - r.start) / r.sampleRate).toStringAsFixed(2)} $sec'
            '${regionNote(r)}',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
          trailing: r.edited ? const Icon(Icons.content_cut, size: 18) : null,
          onTap: () {
            _stop();
            setState(() {
              _selected = r.id;
              _cursor = null;
              _selectedPart = null;
            });
          },
        ),
    ];
  }
}
