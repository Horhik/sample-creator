import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as api;
import 'timeline.dart';
import 'waveform.dart';
import 'l10n.dart';

/// Full-screen cutting on the output timeline: parts laid end to end on two
/// lanes. Long-press a part to lift and slide it over the previous one (or
/// away, leaving a pause); drag its edges to trim, its top circles to fade. Pinch to
/// zoom; undo in the app bar.
class PartsEditorPage extends StatefulWidget {
  final int regionId;
  const PartsEditorPage({super.key, required this.regionId});

  @override
  State<PartsEditorPage> createState() => _PartsEditorPageState();
}

enum _Drag { none, pan, zoom, move, trimStart, trimEnd, fadeIn, fadeOut }

enum _Play { none, original, result, rendering }

class _PartsEditorPageState extends State<PartsEditorPage>
    with SingleTickerProviderStateMixin {
  late final List<Color> _palette = engine
      .palette()
      .map((c) => Color(0xff000000 | c.rgb))
      .toList();

  api.ReviewRegion? _r;
  Size _size = const Size(1, 1);

  /// Visible output-timeline frames.
  double _viewStart = 0, _viewEnd = 1;

  /// Output-timeline frame.
  int? _cursor;
  int? _selected;
  int? _playhead;
  _Play _play = _Play.none;
  late final Ticker _ticker = createTicker(_tick);

  // Gesture state.
  _Drag _drag = _Drag.none;
  double _dragStart = 0, _dragEnd = 1;
  Offset _dragFocal = Offset.zero;

  /// The selected clip as it was when the drag began, and as it is now.
  Clip? _dragFrom, _dragClip;

  @override
  void initState() {
    super.initState();
    _reload(fit: true);
    _ticker.start();
  }

  @override
  void dispose() {
    _ticker.dispose();
    api.stopPreview();
    super.dispose();
  }

  Future<void> _reload({bool fit = false}) async {
    final rows = await api.allRegions();
    final r = rows.where((x) => x.id == widget.regionId).firstOrNull;
    if (!mounted) return;
    setState(() {
      _r = r;
      if (r == null) return;
      final len = math.max(1, r.splicedFrames).toDouble();
      if (fit || _viewEnd > len * 1.5) {
        _viewStart = 0;
        _viewEnd = len;
      }
      if (_selected != null && _selected! >= r.parts.length) _selected = null;
    });
  }

  void _toast(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(content: Text(text), duration: const Duration(seconds: 2)),
      );
  }

  Future<void> _edit(Future<void> Function() action) async {
    _stop();
    try {
      await action();
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _reload();
  }

  /// Clips as shown, with the one being dragged in its dragged state.
  List<Clip> get _clips {
    final r = _r!;
    return [
      for (var i = 0; i < r.parts.length; i++)
        if (i == _selected && _dragClip != null)
          _dragClip!
        else
          Clip.of(r.parts[i]),
    ];
  }

  // ---- View ----

  double get _fpp => (_viewEnd - _viewStart) / _size.width;
  double _frameAt(double x) => _viewStart + x * _fpp;
  double _xOf(int f) => (f - _viewStart) / _fpp;

  /// Keeps the view on the timeline (a little past its end), no closer than
  /// the shared zoom limit.
  void _setView(double start, double end) {
    final total = math.max(1, timelineLength(_clips)).toDouble() * 1.1;
    final minSpan = math.min(minFramesPerPx * _size.width, total);
    final span = (end - start).clamp(minSpan, total).toDouble();
    start = start.clamp(0.0, math.max(0.0, total - span)).toDouble();
    _viewStart = start;
    _viewEnd = start + span;
  }

  void _zoom(double factor, {double? around}) {
    final centre = around ?? (_viewStart + _viewEnd) / 2;
    final half = (_viewEnd - _viewStart) / factor / 2;
    setState(() => _setView(centre - half, centre + half));
  }

  void _fit() {
    setState(() {
      _viewStart = 0;
      _viewEnd = math.max(1, timelineLength(_clips)).toDouble();
    });
  }

  // ---- Playback ----

  void _tick(Duration _) {
    if (!mounted || _r == null) return;
    final playing = _play == _Play.original || _play == _Play.result;
    if (playing && !engine.isPlaying()) {
      setState(() {
        _play = _Play.none;
        _playhead = null;
      });
      return;
    }
    if (_play == _Play.result) {
      // Rendered frames back to the timeline (the speed change stretched them).
      final pos = engine.position() * _r!.speed ~/ 100;
      setState(() {
        _playhead = pos;
        if (_drag == _Drag.none && pos > _viewEnd) {
          final span = _viewEnd - _viewStart;
          _setView(pos - span * 0.1, pos + span * 0.9);
        }
      });
    }
  }

  void _stop() {
    api.stopPreview();
    if (mounted) {
      setState(() {
        _play = _Play.none;
        _playhead = null;
      });
    }
  }

  void _playOriginal() {
    final r = _r!;
    if (_play == _Play.original) {
      _stop();
      return;
    }
    _stop();
    api.previewSource(path: r.path, start: r.start, end: r.end);
    setState(() => _play = _Play.original);
  }

  Future<void> _playResult() async {
    final r = _r!;
    if (_play == _Play.result) {
      _stop();
      return;
    }
    _stop();
    setState(() => _play = _Play.rendering);
    try {
      final from = (_cursor ?? 0)
          .clamp(0, math.max(0, r.splicedFrames - 1))
          .toInt();
      await api.previewProcessed(id: r.id, from: from);
      if (mounted) setState(() => _play = _Play.result);
    } catch (e) {
      _toast(friendlyError(e));
      if (mounted) setState(() => _play = _Play.none);
    }
  }

  // ---- Gestures ----

  static const _grab = 24.0;

  void _onTapUp(TapUpDetails d) {
    final clips = _clips;
    final pos = layoutClips(clips);
    final t = math.max(0, _frameAt(d.localPosition.dx).round());
    setState(() {
      _cursor = t;
      _selected = clipAt(clips, pos, t, d.localPosition.dy, _size);
    });
  }

  void _onScaleStart(ScaleStartDetails d) {
    _dragStart = _viewStart;
    _dragEnd = _viewEnd;
    _dragFocal = d.localFocalPoint;
    _drag = d.pointerCount > 1 ? _Drag.zoom : _Drag.pan;
    final sel = _selected;
    if (d.pointerCount != 1 || sel == null) return;
    final clips = _clips;
    final pos = layoutClips(clips);
    final c = clips[sel];
    final lane = TimelinePainter.laneRect(sel, _size);
    final x = d.localFocalPoint.dx, y = d.localFocalPoint.dy;
    if (y < lane.top - 8 || y > lane.bottom + 8) return;
    final (fi, fo) = c.fades;
    final x0 = _xOf(pos[sel]), x1 = _xOf(pos[sel] + c.len);
    final nearTop = y < lane.top + TimelinePainter.fadeHandleY + 18;
    if (nearTop && (x - _xOf(pos[sel] + fi)).abs() < _grab) {
      _drag = _Drag.fadeIn;
    } else if (nearTop && (x - _xOf(pos[sel] + c.len - fo)).abs() < _grab) {
      _drag = _Drag.fadeOut;
    } else if ((x - x0).abs() < _grab) {
      _drag = _Drag.trimStart;
    } else if ((x - x1).abs() < _grab) {
      _drag = _Drag.trimEnd;
    }
    if (_drag != _Drag.pan) {
      _dragFrom = c;
      _dragClip = c;
      HapticFeedback.selectionClick();
    }
  }

  void _onScaleUpdate(ScaleUpdateDetails d) {
    final r = _r!;
    final from = _dragFrom;
    final dx = d.localFocalPoint.dx - _dragFocal.dx;
    final dFrames = (dx * _fpp).round();
    switch (_drag) {
      case _Drag.pan:
        if (d.pointerCount > 1) {
          _drag = _Drag.zoom;
          _dragStart = _viewStart;
          _dragEnd = _viewEnd;
          _dragFocal = d.localFocalPoint;
          return;
        }
        final shift = dx * (_dragEnd - _dragStart) / _size.width;
        setState(() => _setView(_dragStart - shift, _dragEnd - shift));
      case _Drag.zoom:
        final span =
            (_dragEnd - _dragStart) / d.horizontalScale.clamp(0.01, 100);
        final anchor =
            _dragStart + _dragFocal.dx / _size.width * (_dragEnd - _dragStart);
        final start = anchor - d.localFocalPoint.dx / _size.width * span;
        setState(() => _setView(start, start + span));
      case _Drag.move:
        // Left slides over the previous part, right leaves a pause.
        setState(
          () => _dragClip = from!.copyWith(overlap: from.overlap - dFrames),
        );
      case _Drag.trimStart:
        // The sound stays put: a later start also starts later on the timeline.
        final start = (from!.start + dFrames).clamp(r.start, from.end - 1);
        setState(
          () => _dragClip = from.copyWith(
            start: start,
            overlap: from.overlap - (start - from.start),
          ),
        );
      case _Drag.trimEnd:
        final end = (from!.end + dFrames).clamp(from.start + 1, r.end);
        setState(() => _dragClip = from.copyWith(end: end));
      case _Drag.fadeIn:
        final (fi, _) = from!.fades;
        setState(
          () => _dragClip = from.copyWith(
            fadeIn: (fi + dFrames).clamp(0, from.len),
          ),
        );
      case _Drag.fadeOut:
        final (_, fo) = from!.fades;
        setState(
          () => _dragClip = from.copyWith(
            fadeOut: (fo - dFrames).clamp(0, from.len),
          ),
        );
      case _Drag.none:
        break;
    }
  }

  /// Long press lifts a part so it can slide: picks the part under the finger.
  void _onLongPressStart(LongPressStartDetails d) {
    final clips = _clips;
    final pos = layoutClips(clips);
    final t = math.max(0, _frameAt(d.localPosition.dx).round());
    final i = clipAt(clips, pos, t, d.localPosition.dy, _size);
    if (i == null) return;
    if (i == 0) {
      // The first part always starts the result.
      HapticFeedback.lightImpact();
      setState(() => _selected = 0);
      _toast(
        tr(
          'Первая часть всегда в начале — двигайте следующие',
          'The first part always starts the result — move the others',
        ),
      );
      return;
    }
    HapticFeedback.heavyImpact();
    setState(() {
      _selected = i;
      _drag = _Drag.move;
      _dragFocal = d.localPosition;
      _dragFrom = clips[i];
      _dragClip = clips[i];
    });
  }

  void _onLongPressMove(LongPressMoveUpdateDetails d) {
    if (_drag != _Drag.move) return;
    final from = _dragFrom!;
    final dFrames = ((d.localPosition.dx - _dragFocal.dx) * _fpp).round();
    // Left slides over the previous part, right leaves a pause.
    setState(() => _dragClip = from.copyWith(overlap: from.overlap - dFrames));
  }

  void _onLongPressEnd(LongPressEndDetails d) {
    if (_drag != _Drag.move) return;
    HapticFeedback.selectionClick();
    _onScaleEnd(ScaleEndDetails());
  }

  void _onScaleEnd(ScaleEndDetails d) {
    final drag = _drag;
    _drag = _Drag.none;
    final c = _dragClip;
    final sel = _selected;
    final r = _r!;
    if (c == null || sel == null) {
      _dragClip = _dragFrom = null;
      return;
    }
    int toMs(int frames) => (frames * 1000 / r.sampleRate).round();
    Future<void> commit() async {
      switch (drag) {
        case _Drag.move:
          await api.setPartOverlap(id: r.id, index: sel, overlap: c.overlap);
        case _Drag.trimStart || _Drag.trimEnd:
          await api.setPartBounds(
            id: r.id,
            index: sel,
            start: c.start,
            end: c.end,
          );
        case _Drag.fadeIn || _Drag.fadeOut:
          await api.setPartFades(
            id: r.id,
            index: sel,
            fadeInMs: toMs(c.fadeIn),
            fadeOutMs: toMs(c.fadeOut),
          );
        default:
          break;
      }
    }

    _edit(commit).then((_) {
      if (mounted) setState(() => _dragClip = _dragFrom = null);
    });
  }

  // ---- Actions ----

  Future<void> _cut() async {
    final at = _cursor;
    if (at == null) {
      _toast(
        tr('Коснитесь ленты там, где резать', 'Tap the timeline where to cut'),
      );
      return;
    }
    var done = false;
    await _edit(() async => done = await api.splitAtOutput(id: _r!.id, t: at));
    if (!done) {
      _toast(
        tr(
          'Под курсором нет части или это её край',
          'No part under the cursor, or it is an edge',
        ),
      );
    }
  }

  Future<void> _properties(int i) async {
    final r = _r!;
    final res = await editPartDialog(context, r, i);
    if (res == null) return;
    await _edit(() => applyPartDialog(r, i, res));
  }

  Future<void> _undo(bool redo) async {
    _stop();
    try {
      final id = redo ? await api.processingRedo() : await api.processingUndo();
      if (id != null && id != widget.regionId) {
        _toast(
          tr(
            'Отменено действие в другом регионе',
            'Undid a change in another region',
          ),
        );
      }
    } catch (e) {
      _toast(friendlyError(e));
    }
    await _reload();
  }

  // ---- Build ----

  @override
  Widget build(BuildContext context) {
    final r = _r;
    if (r == null) {
      return Scaffold(
        appBar: AppBar(),
        body: const Center(child: CircularProgressIndicator()),
      );
    }
    final theme = Theme.of(context);
    final color = _palette[r.color % _palette.length];
    final clips = _clips;
    final pos = layoutClips(clips);
    final total = timelineLength(clips);

    return Scaffold(
      appBar: AppBar(
        title: Text(r.name.isEmpty ? tr('Нарезка', 'Cutting') : r.name),
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
      body: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
              child: Row(
                children: [
                  Text(
                    formatTime(_viewStart / r.sampleRate),
                    style: theme.textTheme.bodySmall,
                  ),
                  const Spacer(),
                  Text(
                    '${tr('результат', 'result')} ${(total / r.sampleRate).toStringAsFixed(2)} $sec'
                    '${r.speed == 100 ? '' : tr(' (до смены скорости)', ' (before the speed change)')}',
                    style: theme.textTheme.bodySmall,
                  ),
                  const Spacer(),
                  Text(
                    formatTime(_viewEnd / r.sampleRate),
                    style: theme.textTheme.bodySmall,
                  ),
                ],
              ),
            ),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 8),
                child: LayoutBuilder(
                  builder: (context, c) {
                    _size = Size(math.max(c.maxWidth, 1), c.maxHeight);
                    return GestureDetector(
                      onTapUp: _onTapUp,
                      onDoubleTap: _fit,
                      onLongPressStart: _onLongPressStart,
                      onLongPressMoveUpdate: _onLongPressMove,
                      onLongPressEnd: _onLongPressEnd,
                      onScaleStart: _onScaleStart,
                      onScaleUpdate: _onScaleUpdate,
                      onScaleEnd: _onScaleEnd,
                      child: CustomPaint(
                        size: Size(c.maxWidth, c.maxHeight),
                        painter: TimelinePainter(
                          clips: clips,
                          positions: pos,
                          waves: clipWaves(
                            r,
                            clips,
                            pos,
                            _viewStart,
                            _viewEnd,
                            c.maxWidth,
                          ),
                          channels: r.fileChannels,
                          viewStart: _viewStart,
                          viewEnd: _viewEnd,
                          color: color,
                          scheme: theme.colorScheme,
                          selected: _selected,
                          lifted: _drag == _Drag.move,
                          cursor: _cursor,
                          playhead: _playhead,
                        ),
                      ),
                    );
                  },
                ),
              ),
            ),
            _controls(r),
          ],
        ),
      ),
    );
  }

  Widget _controls(api.ReviewRegion r) {
    final theme = Theme.of(context);
    final sel = _selected;
    final count = r.parts.length;
    final p = sel == null ? null : r.parts[sel];
    int toMs(int frames) => (frames * 1000 / r.sampleRate).round();
    return Material(
      elevation: 2,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    [
                      if (_cursor != null)
                        tr(
                          'курсор ${formatTime(_cursor! / r.sampleRate)}',
                          'cursor ${formatTime(_cursor! / r.sampleRate)}',
                        ),
                      if (p != null)
                        '${tr('часть', 'part')} ${sel! + 1}: ${((p.end - p.start) / r.sampleRate).toStringAsFixed(3)} $sec, '
                            '${tr('фейды', 'fades')} ${toMs(p.fadeIn)}/${toMs(p.fadeOut)} $ms'
                            '${overlapNote(r, sel)}',
                    ].join(' · '),
                    style: theme.textTheme.bodySmall,
                    overflow: TextOverflow.ellipsis,
                    maxLines: 2,
                  ),
                ),
                IconButton(
                  tooltip: tr('Отдалить', 'Zoom out'),
                  icon: const Icon(Icons.zoom_out),
                  onPressed: () => _zoom(0.5),
                ),
                IconButton(
                  tooltip: tr('Всё', 'All'),
                  icon: const Icon(Icons.fit_screen),
                  onPressed: _fit,
                ),
                IconButton(
                  tooltip: tr('Приблизить', 'Zoom in'),
                  icon: const Icon(Icons.zoom_in),
                  onPressed: () => _zoom(2, around: _cursor?.toDouble()),
                ),
              ],
            ),
            Row(
              children: [
                Expanded(
                  child: OutlinedButton.icon(
                    icon: Icon(
                      _play == _Play.original ? Icons.stop : Icons.play_arrow,
                    ),
                    label: Text(tr('Оригинал', 'Original')),
                    onPressed: _playOriginal,
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: FilledButton.tonalIcon(
                    icon: _play == _Play.rendering
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : Icon(
                            _play == _Play.result
                                ? Icons.stop
                                : Icons.play_arrow,
                          ),
                    label: Text(tr('Результат', 'Result')),
                    onPressed: _play == _Play.rendering ? null : _playResult,
                  ),
                ),
                const SizedBox(width: 8),
                IconButton.filled(
                  tooltip: tr('Разрезать в курсоре', 'Cut at cursor'),
                  icon: const Icon(Icons.content_cut),
                  onPressed: _cut,
                ),
              ],
            ),
            if (sel != null)
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                children: [
                  IconButton(
                    tooltip: tr('Раньше', 'Earlier'),
                    icon: const Icon(Icons.arrow_back),
                    onPressed: sel > 0
                        ? () async {
                            await _edit(
                              () =>
                                  api.movePart(id: r.id, index: sel, up: true),
                            );
                            setState(() => _selected = sel - 1);
                          }
                        : null,
                  ),
                  IconButton(
                    tooltip: tr('Позже', 'Later'),
                    icon: const Icon(Icons.arrow_forward),
                    onPressed: sel + 1 < count
                        ? () async {
                            await _edit(
                              () =>
                                  api.movePart(id: r.id, index: sel, up: false),
                            );
                            setState(() => _selected = sel + 1);
                          }
                        : null,
                  ),
                  TextButton.icon(
                    icon: const Icon(Icons.tune),
                    label: Text(tr('Точно…', 'Exact…')),
                    onPressed: () => _properties(sel),
                  ),
                  TextButton.icon(
                    style: TextButton.styleFrom(
                      foregroundColor: theme.colorScheme.error,
                    ),
                    icon: const Icon(Icons.delete_outline),
                    label: Text(tr('Убрать', 'Remove')),
                    onPressed: count > 1
                        ? () async {
                            await _edit(
                              () => api.removePart(id: r.id, index: sel),
                            );
                            setState(() => _selected = null);
                          }
                        : null,
                  ),
                ],
              )
            else
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(
                  tr(
                    'Касание — курсор и выбор части. Долгое нажатие на часть — '
                        'двигать её (наложение или пауза). У выбранной части края — '
                        'подрезка, кружки сверху — фейды. Щипок — масштаб.',
                    'Tap — cursor and pick a part. Long-press a part to move it (overlap or pause). On the picked part the edges trim and the top circles fade. Pinch to zoom.',
                  ),
                  style: theme.textTheme.bodySmall,
                  textAlign: TextAlign.center,
                ),
              ),
          ],
        ),
      ),
    );
  }
}
