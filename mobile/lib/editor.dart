import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import 'item_sheet.dart';
import 'browser.dart';
import 'display.dart';
import 'listening.dart';
import 'markup_rail.dart';
import 'remote_ui.dart';
import 'src/rust/api/remote.dart' as remote_api;
import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as review;
import 'sync_ui.dart';
import 'waveform.dart';
import 'l10n.dart';

/// One recording: waveform, transport and the list of markers and regions.
/// Every change goes straight to the library, as on the desktop.
class EditorPage extends StatefulWidget {
  /// The recording to show; `null` until one is picked in the side panel.
  final String? path;

  /// This page is on screen and owns the player.
  final bool active;

  /// A region to select once [path] is open; applied when [focusToken] changes.
  final int? focusRegion;
  final int focusToken;

  final void Function(String path) onOpen;

  const EditorPage({
    super.key,
    required this.path,
    required this.active,
    required this.focusRegion,
    required this.focusToken,
    required this.onOpen,
  });

  @override
  State<EditorPage> createState() => _EditorPageState();
}

enum _Drag { none, pan, zoom, selStart, selEnd, regionStart, regionEnd, marker }

class _EditorPageState extends State<EditorPage>
    with SingleTickerProviderStateMixin {
  engine.FileInfo? _info;
  String? _error;

  /// The recording's file is not there (e.g. the recorder is unplugged).
  bool _missing = false;

  // Landscape: the list in a right-hand panel, the controls floating.
  bool _listOpen = true;
  bool _controlsOpen = true;
  late final List<Color> _palette = engine
      .palette()
      .map((c) => Color(0xff000000 | c.rgb))
      .toList();

  engine.MarkupView? _markup;
  WaveView _view = const WaveView(0, 1);
  double _width = 0;
  bool _fitted = false;

  Selected? _selected;
  (int, int)? _selection;
  int _color = 0;
  bool _loop = true;

  int _playhead = 0;
  bool _playing = false;
  double _peaksDone = 0;
  int _detailGeneration = 0;
  late final Ticker _ticker = createTicker(_tick);

  // Gesture state.
  _Drag _drag = _Drag.none;
  WaveView _dragView = const WaveView(0, 1);
  Offset _dragFocal = Offset.zero;
  int _dragFrame = 0;
  (int, int)? _dragRange;

  int get _total => _info?.totalFrames ?? 1;
  int get _rate => _info?.sampleRate ?? 48000;

  final _scaffold = GlobalKey<ScaffoldState>();

  @override
  void initState() {
    super.initState();
    markupChanged.addListener(_onSynced);
    volumesChanged.addListener(_onVolumes);
    waveDetailed.addListener(_onWaveStyle);
    fullscreen.addListener(_onWaveStyle);
    markupRail
      ..files = (() => _scaffold.currentState?.openDrawer())
      ..toggleList = (() => setState(() => _listOpen = !_listOpen))
      ..undo = (() => _run(() => engine.undo()))
      ..redo = (() => _run(() => engine.redo()));
    _load();
  }

  void _onWaveStyle() {
    if (mounted) setState(() => _view = _clampView(_view));
  }

  /// A sync brought markup from a volume.
  void _onSynced() {
    if (_info != null && mounted) {
      _refresh();
      _applyRange();
    }
  }

  @override
  void didUpdateWidget(EditorPage old) {
    super.didUpdateWidget(old);
    if (widget.path != old.path) {
      _load();
    } else if (widget.focusToken != old.focusToken) {
      _focus();
    }
    if (widget.active != old.active) {
      _ticker.muted = !widget.active;
      if (widget.active) {
        // Back from another page: the player may hold a preview.
        review.reattach();
        engine.seek(pos: _playhead);
        _applyRange();
        if (_info != null) _refresh();
      } else {
        engine.pause();
      }
    }
  }

  Future<void> _load() async {
    final path = widget.path;
    if (path == null) return;
    engine.pause();
    setState(() {
      _info = null;
      _markup = null;
      _error = null;
      _missing = false;
      _selected = null;
      _selection = null;
      _fitted = false;
      _playhead = 0;
    });
    if (!await remote_api.pathExists(path: path)) {
      // Opens by itself once the volume comes back (see _onVolumes).
      setState(() => _missing = true);
      return;
    }
    // Opened from the review or at start: a file on a computer downloads here.
    if (!mounted) return;
    if (!await ensureLocal(context, path)) {
      if (mounted && widget.path == path) {
        setState(
          () => _error = tr(
            'Запись не загружена с компьютера. Откройте её снова из боковой панели.',
            'The recording was not downloaded from the computer. Open it again from the side panel.',
          ),
        );
      }
      return;
    }
    if (!mounted || widget.path != path) return;
    try {
      // Edits of the previous recording go out, the volume's news come in.
      await syncAll();
      final info = await engine.openFile(path: path);
      if (!mounted || widget.path != path) return;
      setState(() {
        _info = info;
        _markup = engine.markup();
      });
      _applyRange();
      if (!_ticker.isActive) _ticker.start();
      _ticker.muted = !widget.active;
      _focus();
    } catch (e) {
      setState(() => _error = friendlyError(e));
    }
  }

  void _onVolumes() {
    final path = widget.path;
    if (_missing && path != null) {
      remote_api.pathExists(path: path).then((ok) {
        if (ok && mounted && _missing && widget.path == path) _load();
      });
    }
  }

  /// Selects the region the review asked for and brings it into view.
  void _focus() {
    final id = widget.focusRegion;
    final r = _markup?.regions.where((r) => r.id == id).firstOrNull;
    if (r == null) return;
    _select(Selected(Kind.region, r.id));
    _seek(r.start);
    if (_width > 0) {
      final len = (r.end - r.start).toDouble();
      final fpp = len * 1.6 / _width;
      setState(() {
        _fitted = true;
        _view = _clampView(WaveView(r.start - len * 0.3, fpp));
      });
    }
  }

  @override
  void dispose() {
    markupChanged.removeListener(_onSynced);
    volumesChanged.removeListener(_onVolumes);
    waveDetailed.removeListener(_onWaveStyle);
    fullscreen.removeListener(_onWaveStyle);
    markupRail.update(shown: false);
    _ticker.dispose();
    super.dispose();
  }

  /// The side panel plays a held recording: the player is not ours.
  bool _listPreview = false;

  void _tick(Duration _) {
    if (_listPreview) return;
    final pos = engine.position();
    final playing = engine.isPlaying();
    // Asked here, not while laying out the wave: the label above is built
    // before the wave, so it would always show the previous value.
    final done = engine.peaksDone();
    final detail = waveDetailed.value ? engine.detailGeneration() : 0;
    var view = _view;
    // Keep the playhead in sight while playing: page forward like a tape deck.
    if (playing && _width > 0 && _drag == _Drag.none) {
      final x = view.xOf(pos);
      if (x > _width || x < 0) {
        view = _clampView(WaveView(pos - _width * 0.1 * view.fpp, view.fpp));
      }
    }
    if (pos != _playhead ||
        playing != _playing ||
        view != _view ||
        done != _peaksDone ||
        detail != _detailGeneration) {
      setState(() {
        _playhead = pos;
        _playing = playing;
        _view = view;
        _peaksDone = done;
        _detailGeneration = detail;
      });
    }
  }

  void _refresh() => setState(() => _markup = engine.markup());

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      _toast(friendlyError(e));
    }
    _refresh();
    _applyRange();
  }

  void _toast(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(content: Text(text), duration: const Duration(seconds: 2)),
      );
  }

  // ---- View ----

  WaveView _clampView(WaveView v) {
    final maxFpp = _total / math.max(_width, 1);
    final fpp = v.fpp.clamp(_minFpp, math.max(maxFpp, _minFpp)).toDouble();
    final maxStart = math.max(0.0, _total - _width * fpp);
    return WaveView(v.start.clamp(0.0, maxStart).toDouble(), fpp);
  }

  /// Closest zoom: single samples in the detailed waveform.
  double get _minFpp =>
      waveDetailed.value ? detailMinFramesPerPx : minFramesPerPx;

  /// Widest view (the whole file) in frames per pixel.
  double get _maxFpp =>
      math.max(_total / math.max(_width, 1), _minFpp).toDouble();

  /// 0 = the whole file, 1 = the closest zoom; logarithmic, like the pinch.
  double get _zoomValue {
    final range = math.log(_maxFpp / _minFpp);
    if (range <= 0) return 0;
    return (math.log(_maxFpp / _view.fpp) / range).clamp(0.0, 1.0).toDouble();
  }

  /// Zooms around the playhead when it is in view, else around the middle.
  void _setZoom(double z) {
    final fpp = _maxFpp * math.pow(_minFpp / _maxFpp, z);
    final x = _view.xOf(_playhead);
    final anchor = x >= 0 && x <= _width ? x : _width / 2;
    final frame = _view.frameAt(anchor);
    setState(() => _view = _clampView(WaveView(frame - anchor * fpp, fpp)));
  }

  Widget _zoomSlider({double? width}) {
    final scheme = Theme.of(context).colorScheme;
    // The default inactive track almost matches the floating panel.
    final slider = SliderTheme(
      data: SliderTheme.of(context).copyWith(
        inactiveTrackColor: scheme.onSurfaceVariant.withValues(alpha: 0.35),
      ),
      child: Slider(value: _zoomValue, onChanged: _setZoom),
    );
    return Row(
      mainAxisSize: width == null ? MainAxisSize.max : MainAxisSize.min,
      children: [
        const Icon(Icons.zoom_out, size: 18),
        width == null
            ? Expanded(child: slider)
            : SizedBox(width: width, child: slider),
        const Icon(Icons.zoom_in, size: 18),
      ],
    );
  }

  void _fit() => setState(
    () => _view = _clampView(WaveView(0, _total / math.max(_width, 1))),
  );

  // ---- Playback ----

  /// The loop range: the selection, else the selected region.
  (int, int)? get _loopRange {
    if (!_loop) return null;
    if (_selection != null) return _selection;
    final r = _selectedRegion;
    return r == null ? null : (r.start, r.end);
  }

  void _applyRange() {
    // A selected region made louder is heard louder while marking up.
    final r = _selectedRegion;
    engine.setGain(
      gain: _selection == null && r != null && !r.normalize
          ? math.pow(10, r.gain / 200).toDouble()
          : 1.0,
    );
    final range = _loopRange;
    if (range == null) {
      engine.setRange(start: 0, end: null, looping: false);
    } else {
      engine.setRange(start: range.$1, end: range.$2, looping: true);
    }
  }

  void _togglePlay() {
    if (_playing) {
      engine.pause();
      return;
    }
    final range = _loopRange;
    _applyRange();
    if (range != null && (_playhead < range.$1 || _playhead >= range.$2)) {
      engine.seek(pos: range.$1);
    } else if (_playhead >= _total - 1) {
      engine.seek(pos: 0);
    }
    engine.play();
  }

  void _seek(int frame) {
    engine.seek(pos: frame.clamp(0, _total));
    setState(() => _playhead = frame.clamp(0, _total));
  }

  // ---- Items ----

  engine.RegionView? get _selectedRegion {
    final s = _selected;
    if (s == null || s.kind != Kind.region) return null;
    return _markup?.regions.where((r) => r.id == s.id).firstOrNull;
  }

  engine.MarkerView? get _selectedMarker {
    final s = _selected;
    if (s == null || s.kind != Kind.marker) return null;
    return _markup?.markers.where((m) => m.id == s.id).firstOrNull;
  }

  void _select(Selected? s) {
    setState(() {
      _selected = s;
      if (s != null) _selection = null;
    });
    _applyRange();
  }

  Future<void> _addMarker() async {
    await _run(() async {
      final id = await engine.addMarker(pos: _playhead, color: _color);
      _selected = Selected(Kind.marker, id);
    });
    HapticFeedback.lightImpact();
  }

  Future<void> _makeRegion() async {
    final sel = _selection;
    if (sel == null) {
      _toast(
        tr(
          'Сначала выделите участок: долгое нажатие на волне и ведите',
          'Select a stretch first: long-press the wave and drag',
        ),
      );
      return;
    }
    int? created;
    await _run(() async {
      created = await engine.addRegion(
        start: sel.$1,
        end: sel.$2,
        color: _color,
      );
    });
    if (created == null) return;
    setState(() {
      _selection = null;
      _selected = Selected(Kind.region, created!);
      // Next region gets the next colour, so neighbours differ.
      _color = (_color + 1) % _palette.length;
    });
    _applyRange();
    // Name it right away: that is what the region is for.
    if (mounted) _openSheet(Selected(Kind.region, created!), focusName: true);
  }

  Future<void> _openSheet(Selected s, {bool focusName = false}) async {
    _select(s);
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => ItemSheet(
        item: s,
        palette: _palette,
        focusName: focusName,
        onChanged: (color) {
          if (color != null) _color = color;
          _refresh();
        },
      ),
    );
    final m = engine.markup();
    final exists = s.kind == Kind.region
        ? m.regions.any((r) => r.id == s.id)
        : m.markers.any((x) => x.id == s.id);
    setState(() {
      _markup = m;
      if (!exists) _selected = null;
    });
    _applyRange();
  }

  Future<void> _renameFile() async {
    final controller = TextEditingController(text: _info?.title);
    final alias = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(tr('Имя записи', 'Recording name')),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: InputDecoration(
            hintText: tr('Пусто — имя файла', 'Empty — the file name'),
          ),
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
    if (alias == null) return;
    await engine.setAlias(alias: alias);
    final name = widget.path!.split('/').last;
    final stem = name.contains('.')
        ? name.substring(0, name.lastIndexOf('.'))
        : name;
    setState(() {
      final i = _info!;
      _info = engine.FileInfo(
        path: i.path,
        title: alias.trim().isEmpty ? stem : alias.trim(),
        channels: i.channels,
        sampleRate: i.sampleRate,
        totalFrames: i.totalFrames,
        encoding: i.encoding,
        recordedAt: i.recordedAt,
      );
    });
  }

  // ---- Gestures on the wave ----

  static const _grab = 28.0;

  void _onTapUp(TapUpDetails d) {
    final x = d.localPosition.dx;
    final frame = _view.frameAt(x).round();
    final m = _markup!;
    // Markers are thin: give them a finger-sized target.
    final marker = m.markers
        .where((mk) => (_view.xOf(mk.pos) - x).abs() < _grab / 2)
        .firstOrNull;
    if (marker != null) {
      _select(Selected(Kind.marker, marker.id));
    } else {
      // The shortest region under the finger, so nested ones stay reachable.
      final hits =
          m.regions.where((r) => r.start <= frame && frame < r.end).toList()
            ..sort((a, b) => (a.end - a.start).compareTo(b.end - b.start));
      if (hits.isNotEmpty) {
        _select(Selected(Kind.region, hits.first.id));
      } else {
        setState(() {
          _selected = null;
          _selection = null;
        });
        _applyRange();
      }
    }
    _seek(frame);
  }

  void _onLongPressStart(LongPressStartDetails d) {
    // A new selection begins: a clear buzz under the finger.
    HapticFeedback.mediumImpact();
    final f = _view.frameAt(d.localPosition.dx).round().clamp(0, _total);
    _dragFrame = f;
    setState(() {
      _selected = null;
      _selection = (f, f);
    });
  }

  void _onLongPressMove(LongPressMoveUpdateDetails d) {
    final f = _view.frameAt(d.localPosition.dx).round().clamp(0, _total);
    setState(
      () => _selection = (math.min(_dragFrame, f), math.max(_dragFrame, f)),
    );
  }

  void _onLongPressEnd(LongPressEndDetails d) {
    final sel = _selection;
    if (sel != null && sel.$2 - sel.$1 < _rate ~/ 100) {
      setState(() => _selection = null);
    } else if (sel != null) {
      _seek(sel.$1);
    }
    _applyRange();
  }

  void _onScaleStart(ScaleStartDetails d) {
    _dragView = _view;
    _dragFocal = d.localFocalPoint;
    _drag = d.pointerCount > 1 ? _Drag.zoom : _Drag.pan;
    if (d.pointerCount != 1) return;
    final x = d.localFocalPoint.dx;
    bool near(int frame) => (_view.xOf(frame) - x).abs() < _grab;

    final sel = _selection;
    final r = _selectedRegion;
    final mk = _selectedMarker;
    if (sel != null && near(sel.$1)) {
      _drag = _Drag.selStart;
    } else if (sel != null && near(sel.$2)) {
      _drag = _Drag.selEnd;
    } else if (r != null && near(r.start)) {
      _drag = _Drag.regionStart;
      _dragRange = (r.start, r.end);
    } else if (r != null && near(r.end)) {
      _drag = _Drag.regionEnd;
      _dragRange = (r.start, r.end);
    } else if (mk != null && near(mk.pos)) {
      _drag = _Drag.marker;
      _dragFrame = mk.pos;
    }
    // The marker or an edge is caught and now follows the finger.
    switch (_drag) {
      case _Drag.marker:
        HapticFeedback.mediumImpact();
      case _Drag.selStart ||
          _Drag.selEnd ||
          _Drag.regionStart ||
          _Drag.regionEnd:
        HapticFeedback.selectionClick();
      default:
        break;
    }
  }

  void _onScaleUpdate(ScaleUpdateDetails d) {
    final f = _view.frameAt(d.localFocalPoint.dx).round().clamp(0, _total);
    switch (_drag) {
      case _Drag.pan:
        if (d.pointerCount > 1) {
          // A second finger landed mid-pan: switch to zooming from here.
          _drag = _Drag.zoom;
          _dragView = _view;
          _dragFocal = d.localFocalPoint;
          return;
        }
        final dx = d.localFocalPoint.dx - _dragFocal.dx;
        setState(
          () => _view = _clampView(
            WaveView(_dragView.start - dx * _dragView.fpp, _dragView.fpp),
          ),
        );
      case _Drag.zoom:
        final anchor = _dragView.frameAt(_dragFocal.dx);
        final fpp = _dragView.fpp / d.horizontalScale.clamp(0.01, 100);
        final clamped = _clampView(WaveView(0, fpp)).fpp;
        setState(
          () => _view = _clampView(
            WaveView(anchor - d.localFocalPoint.dx * clamped, clamped),
          ),
        );
      case _Drag.selStart:
        final sel = _selection!;
        setState(() => _selection = (math.min(f, sel.$2 - 1), sel.$2));
      case _Drag.selEnd:
        final sel = _selection!;
        setState(() => _selection = (sel.$1, math.max(f, sel.$1 + 1)));
      case _Drag.regionStart:
        final r = _dragRange!;
        setState(() => _dragRange = (math.min(f, r.$2 - 1), r.$2));
      case _Drag.regionEnd:
        final r = _dragRange!;
        setState(() => _dragRange = (r.$1, math.max(f, r.$1 + 1)));
      case _Drag.marker:
        setState(() => _dragFrame = f);
      case _Drag.none:
        break;
    }
  }

  void _onScaleEnd(ScaleEndDetails d) {
    final drag = _drag;
    _drag = _Drag.none;
    final r = _selectedRegion;
    final mk = _selectedMarker;
    if (drag == _Drag.marker) HapticFeedback.lightImpact();
    switch (drag) {
      case _Drag.regionStart || _Drag.regionEnd:
        final range = _dragRange!;
        _dragRange = null;
        if (r != null) {
          _run(
            () => engine.setRegionBounds(
              id: r.id,
              start: range.$1,
              end: range.$2,
            ),
          );
        }
      case _Drag.marker:
        if (mk != null) {
          _run(() => engine.moveMarker(id: mk.id, pos: _dragFrame));
        }
      case _Drag.selStart || _Drag.selEnd:
        _applyRange();
      default:
        break;
    }
  }

  /// The markup as it should look right now, including an edge being dragged.
  engine.MarkupView _shownMarkup() {
    final m = _markup!;
    final range = _dragRange;
    final r = _selectedRegion;
    final mk = _selectedMarker;
    if (range != null && r != null) {
      return engine.MarkupView(
        markers: m.markers,
        regions: [
          for (final x in m.regions)
            x.id == r.id
                ? engine.RegionView(
                    id: x.id,
                    start: range.$1,
                    end: range.$2,
                    name: x.name,
                    color: x.color,
                    export_: x.export_,
                    speed: x.speed,
                    keepPitch: x.keepPitch,
                    gain: x.gain,
                    normalize: x.normalize,
                  )
                : x,
        ],
        canUndo: m.canUndo,
        canRedo: m.canRedo,
      );
    }
    if (_drag == _Drag.marker && mk != null) {
      return engine.MarkupView(
        markers: [
          for (final x in m.markers)
            x.id == mk.id
                ? engine.MarkerView(
                    id: x.id,
                    pos: _dragFrame,
                    name: x.name,
                    color: x.color,
                    fromRecorder: x.fromRecorder,
                  )
                : x,
        ],
        regions: m.regions,
        canUndo: m.canUndo,
        canRedo: m.canRedo,
      );
    }
    return m;
  }

  // ---- Build ----

  @override
  Widget build(BuildContext context) {
    final info = _info;
    final markup = _markup;
    if (widget.path == null) {
      return _bare(
        Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.library_music_outlined, size: 56),
            const SizedBox(height: 12),
            Text(
              tr(
                'Откройте запись в боковой панели',
                'Open a recording from the side panel',
              ),
            ),
            const SizedBox(height: 16),
            FilledButton.icon(
              icon: const Icon(Icons.folder_open),
              label: Text(tr('Файлы', 'Files')),
              onPressed: () => _scaffold.currentState?.openDrawer(),
            ),
          ],
        ),
      );
    }
    if (_missing) {
      final name = widget.path!.split('/').last;
      return _bare(
        Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(Icons.usb_off, size: 56),
              const SizedBox(height: 12),
              Text(
                tr(
                  'Запись $name сейчас недоступна',
                  '$name is not available now',
                ),
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.titleMedium,
              ),
              const SizedBox(height: 8),
              Text(
                tr(
                  'Подключите носитель — она откроется сама. Разметка '
                      'сохранена в приложении.',
                  'Connect the volume and it opens by itself. The markup is kept in the app.',
                ),
                textAlign: TextAlign.center,
              ),
              const SizedBox(height: 16),
              Wrap(
                spacing: 8,
                alignment: WrapAlignment.center,
                children: [
                  OutlinedButton.icon(
                    icon: const Icon(Icons.refresh),
                    label: Text(tr('Проверить', 'Check')),
                    onPressed: _load,
                  ),
                  FilledButton.icon(
                    icon: const Icon(Icons.folder_open),
                    label: Text(tr('Другая запись', 'Another recording')),
                    onPressed: () => _scaffold.currentState?.openDrawer(),
                  ),
                ],
              ),
            ],
          ),
        ),
      );
    }
    if (_error != null) {
      return _bare(
        Padding(padding: const EdgeInsets.all(24), child: Text(_error!)),
      );
    }
    if (info == null || markup == null) {
      return _bare(const CircularProgressIndicator());
    }
    final accent = Theme.of(context).colorScheme.primary;
    final shown = _shownMarkup();
    final landscape =
        MediaQuery.orientationOf(context) == Orientation.landscape;
    // Full screen, landscape: no title bar, its buttons are in the rail and
    // the title heads the list panel.
    final bare = landscape && fullscreen.value;
    markupRail.update(
      shown: bare,
      canUndo: markup.canUndo,
      canRedo: markup.canRedo,
      listOpen: _listOpen,
    );

    return Scaffold(
      key: _scaffold,
      drawer: _drawer(),
      appBar: bare
          ? null
          : AppBar(
              toolbarHeight: landscape ? 48 : null,
              title: _title(info),
              actions: [
                IconButton(
                  tooltip: tr('Отменить', 'Undo'),
                  icon: const Icon(Icons.undo),
                  onPressed: markup.canUndo
                      ? () => _run(() => engine.undo())
                      : null,
                ),
                IconButton(
                  tooltip: tr('Повторить', 'Redo'),
                  icon: const Icon(Icons.redo),
                  onPressed: markup.canRedo
                      ? () => _run(() => engine.redo())
                      : null,
                ),
                if (landscape)
                  IconButton(
                    tooltip: _listOpen
                        ? tr('Скрыть список', 'Hide the list')
                        : tr('Маркеры и регионы', 'Markers and regions'),
                    isSelected: _listOpen,
                    icon: const Icon(Icons.view_sidebar_outlined),
                    selectedIcon: const Icon(Icons.view_sidebar),
                    onPressed: () => setState(() => _listOpen = !_listOpen),
                  ),
              ],
            ),
      body: landscape
          ? _landscape(info, markup, shown, accent, bare: bare)
          : Column(
              children: [
                SizedBox(height: 44, child: _overview(info, shown)),
                Expanded(flex: 5, child: _wave(info, shown, accent)),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 12),
                  child: SizedBox(height: 36, child: _zoomSlider()),
                ),
                _transport(info),
                const Divider(height: 1),
                Expanded(flex: 4, child: _list(markup)),
              ],
            ),
    );
  }

  /// Landscape: the wave takes the screen, the controls float over it and
  /// can be tucked away, the list lives in a panel on the right.
  Widget _landscape(
    engine.FileInfo info,
    engine.MarkupView markup,
    engine.MarkupView shown,
    Color accent, {
    bool bare = false,
  }) {
    final width = MediaQuery.sizeOf(context).width;
    return Row(
      children: [
        Expanded(
          child: Stack(
            children: [
              Column(
                children: [
                  SizedBox(height: 32, child: _overview(info, shown)),
                  Expanded(child: _wave(info, shown, accent)),
                ],
              ),
              Positioned(
                left: 8,
                right: 8,
                bottom: 8,
                child: Center(
                  child: _controlsOpen
                      // Shrinks to the wave's width instead of running over
                      // the side panel.
                      ? FittedBox(
                          fit: BoxFit.scaleDown,
                          child: _floatingTransport(),
                        )
                      : FloatingActionButton.small(
                          tooltip: tr('Показать управление', 'Show controls'),
                          onPressed: () => setState(() => _controlsOpen = true),
                          child: const Icon(Icons.play_arrow),
                        ),
                ),
              ),
            ],
          ),
        ),
        if (_listOpen) ...[
          const VerticalDivider(width: 1),
          SizedBox(
            width: math.min(340, width * 0.34),
            child: bare
                ? SafeArea(
                    left: false,
                    child: Column(
                      children: [
                        Padding(
                          padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                          child: _title(info, panel: true),
                        ),
                        const Divider(height: 1),
                        Expanded(child: _list(markup)),
                      ],
                    ),
                  )
                : _list(markup),
          ),
        ],
      ],
    );
  }

  /// The recording's name (tap to rename) and its format.
  Widget _title(engine.FileInfo info, {bool panel = false}) {
    return GestureDetector(
      onTap: _renameFile,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            info.title,
            overflow: TextOverflow.ellipsis,
            style: panel ? Theme.of(context).textTheme.titleMedium : null,
          ),
          Text(
            [
              '${info.sampleRate ~/ 1000} kHz',
              info.encoding,
              info.channels == 1 ? tr('моно', 'mono') : tr('стерео', 'stereo'),
              formatTime(info.totalFrames / info.sampleRate, millis: false),
              if (info.recordedAt != null) info.recordedAt!,
            ].join(' · '),
            style: Theme.of(context).textTheme.bodySmall,
            overflow: TextOverflow.ellipsis,
          ),
        ],
      ),
    );
  }

  /// The transport as a compact floating bar (landscape).
  Widget _floatingTransport() {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      elevation: 4,
      color: scheme.surfaceContainerHigh.withValues(alpha: 0.94),
      borderRadius: BorderRadius.circular(28),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: Text(
                formatTime(_playhead / _rate),
                style: const TextStyle(
                  fontFeatures: [FontFeature.tabularFigures()],
                ),
              ),
            ),
            IconButton(
              tooltip: tr('В начало', 'To start'),
              icon: const Icon(Icons.skip_previous),
              onPressed: () => _seek(_loopRange?.$1 ?? 0),
            ),
            IconButton(
              tooltip: tr(
                'Цикл выделения или региона',
                'Loop the selection or region',
              ),
              isSelected: _loop,
              icon: const Icon(Icons.repeat),
              selectedIcon: const Icon(Icons.repeat_on),
              onPressed: () {
                setState(() => _loop = !_loop);
                _applyRange();
              },
            ),
            IconButton.filled(
              icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
              onPressed: _togglePlay,
            ),
            IconButton(
              tooltip: tr('Маркер', 'Marker'),
              icon: const Icon(Icons.flag_outlined),
              onPressed: _addMarker,
            ),
            IconButton(
              tooltip: tr('Регион из выделения', 'Region from selection'),
              icon: const Icon(Icons.crop_free),
              onPressed: _makeRegion,
            ),
            const ListeningButton(),
            _zoomSlider(width: 140),
            IconButton(
              tooltip: tr('Спрятать управление', 'Hide controls'),
              icon: const Icon(Icons.expand_more),
              onPressed: () => setState(() => _controlsOpen = false),
            ),
          ],
        ),
      ),
    );
  }

  /// The side panel: the file system, one folder at a time. Opening a
  /// recording keeps this page, it just switches the file.
  Widget _drawer() {
    return Drawer(
      width: math.min(MediaQuery.sizeOf(context).width * 0.88, 420),
      child: SafeArea(
        child: FileBrowser(
          settingsKey: 'mobile.browser',
          showSettings: true,
          current: widget.path,
          onPreview: (on) {
            if (on) {
              engine.pause();
              _listPreview = true;
            } else {
              // The open recording is loaded back: same place, same range.
              _listPreview = false;
              engine.seek(pos: _playhead);
              _applyRange();
            }
          },
          onFile: (path) async {
            // A file on a computer downloads first, with the panel still open.
            final same = path == widget.path && _info != null;
            if (!same && !await ensureLocal(context, path)) return;
            if (!mounted) return;
            Navigator.of(context).pop();
            if (path != widget.path) {
              widget.onOpen(path);
            } else if (_info == null) {
              _load();
            }
          },
        ),
      ),
    );
  }

  Widget _bare(Widget body) {
    markupRail.update(shown: false);
    return Scaffold(
      key: _scaffold,
      drawer: _drawer(),
      appBar: AppBar(title: Text(tr('Разметка', 'Markup'))),
      body: Center(child: body),
    );
  }

  Widget _overview(engine.FileInfo info, engine.MarkupView shown) {
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        void jump(double x) {
          final centre = x / w * _total;
          setState(
            () => _view = _clampView(
              WaveView(centre - _width * _view.fpp / 2, _view.fpp),
            ),
          );
        }

        final peaks = engine.peaks(
          start: 0,
          end: _total.toDouble(),
          columns: waveColumns(_total.toDouble(), w),
        );
        return GestureDetector(
          onTapDown: (d) => jump(d.localPosition.dx),
          onHorizontalDragUpdate: (d) => jump(d.localPosition.dx),
          child: CustomPaint(
            size: Size(w, c.maxHeight),
            painter: OverviewPainter(
              scheme: Theme.of(context).colorScheme,
              peaks: peaks,
              channels: info.channels,
              total: _total,
              view: _view,
              viewWidth: _width,
              markup: shown,
              palette: _palette,
              playhead: _playhead,
            ),
          ),
        );
      },
    );
  }

  Widget _wave(engine.FileInfo info, engine.MarkupView shown, Color accent) {
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        if (w != _width) {
          // The zoom slider and the overview were built with the old width.
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) setState(() {});
          });
          _width = w;
          if (!_fitted) {
            _fitted = true;
            _view = _clampView(WaveView(0, _total / w));
          } else {
            _view = _clampView(_view);
          }
        }
        final detailed = waveDetailed.value;
        final peaks = detailed
            ? engine.waveDetail(
                start: _view.start,
                end: _view.end(w),
                columns: math.max(1, w.floor()),
              )
            : engine.peaks(
                start: _view.start,
                end: _view.end(w),
                columns: waveColumns(_view.end(w) - _view.start, w),
              );
        return GestureDetector(
          onTapUp: _onTapUp,
          onDoubleTap: _fit,
          onLongPressStart: _onLongPressStart,
          onLongPressMoveUpdate: _onLongPressMove,
          onLongPressEnd: _onLongPressEnd,
          onScaleStart: _onScaleStart,
          onScaleUpdate: _onScaleUpdate,
          onScaleEnd: _onScaleEnd,
          child: ClipRect(
            child: CustomPaint(
              size: Size(w, c.maxHeight),
              painter: WavePainter(
                scheme: Theme.of(context).colorScheme,
                peaks: peaks,
                channels: info.channels,
                view: _view,
                markup: shown,
                palette: _palette,
                selected: _selected,
                selection: _selection,
                playhead: _playhead,
                sampleRate: info.sampleRate,
                accent: accent,
                detailed: detailed,
              ),
            ),
          ),
        );
      },
    );
  }

  Widget _transport(engine.FileInfo info) {
    final sel = _selection;
    final r = _selectedRegion;
    String detail;
    if (sel != null) {
      detail = tr(
        'выделено ${formatTime((sel.$2 - sel.$1) / _rate)}',
        'selected ${formatTime((sel.$2 - sel.$1) / _rate)}',
      );
    } else if (r != null) {
      detail = r.name.isEmpty ? tr('регион', 'region') : r.name;
    } else {
      detail = _peaksDone < 1
          ? tr(
              'волна: ${(_peaksDone * 100).floor()} %',
              'waveform: ${(_peaksDone * 100).floor()} %',
            )
          : '';
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
      child: Column(
        children: [
          Row(
            children: [
              Text(
                formatTime(_playhead / _rate),
                style: const TextStyle(
                  fontFeatures: [FontFeature.tabularFigures()],
                  fontSize: 18,
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  detail,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
              if (!engine.hasAudioOutput())
                Tooltip(
                  message: tr('Нет вывода звука', 'No audio output'),
                  child: Icon(Icons.volume_off),
                )
              else
                const ListeningButton(),
            ],
          ),
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              IconButton(
                tooltip: tr('В начало', 'To start'),
                icon: const Icon(Icons.skip_previous),
                onPressed: () => _seek(_loopRange?.$1 ?? 0),
              ),
              IconButton(
                tooltip: tr(
                  'Цикл выделения или региона',
                  'Loop the selection or region',
                ),
                isSelected: _loop,
                icon: const Icon(Icons.repeat),
                selectedIcon: const Icon(Icons.repeat_on),
                onPressed: () {
                  setState(() => _loop = !_loop);
                  _applyRange();
                },
              ),
              IconButton.filled(
                iconSize: 36,
                icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
                onPressed: _togglePlay,
              ),
              TextButton.icon(
                onPressed: _addMarker,
                icon: const Icon(Icons.flag_outlined),
                label: Text(tr('Маркер', 'Marker')),
              ),
              TextButton.icon(
                onPressed: _makeRegion,
                icon: const Icon(Icons.crop_free),
                label: Text(tr('Регион', 'Region')),
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _list(engine.MarkupView m) {
    final items = <(int, Widget)>[];
    for (final r in m.regions) {
      final s = Selected(Kind.region, r.id);
      final len = (r.end - r.start) / _rate;
      final speed = r.speed == 100
          ? ''
          : ' → ${(len * 100 / r.speed).toStringAsFixed(2)} $sec (×${_speed(r.speed)}${r.keepPitch ? tr(', тон', ', pitch kept') : ''})';
      items.add((
        r.start,
        ListTile(
          dense: true,
          selected: _selected == s,
          leading: Icon(
            Icons.square,
            color: _palette[r.color % _palette.length],
          ),
          title: Text(r.name.isEmpty ? tr('Без имени', 'Untitled') : r.name),
          subtitle: Text(
            '${formatTime(r.start / _rate)} · ${len.toStringAsFixed(2)} $sec$speed'
            '${r.export_ ? '' : tr(' · не в экспорт', ' · not exported')}',
          ),
          trailing: IconButton(
            icon: const Icon(Icons.tune),
            onPressed: () => _openSheet(s),
          ),
          onTap: () {
            _select(s);
            _seek(r.start);
          },
          onLongPress: () => _openSheet(s),
        ),
      ));
    }
    for (final mk in m.markers) {
      final s = Selected(Kind.marker, mk.id);
      items.add((
        mk.pos,
        ListTile(
          dense: true,
          selected: _selected == s,
          leading: Icon(
            Icons.flag,
            color: _palette[mk.color % _palette.length],
          ),
          title: Text(mk.name.isEmpty ? tr('Маркер', 'Marker') : mk.name),
          subtitle: Text(formatTime(mk.pos / _rate)),
          trailing: IconButton(
            icon: const Icon(Icons.tune),
            onPressed: () => _openSheet(s),
          ),
          onTap: () {
            _select(s);
            _seek(mk.pos);
          },
          onLongPress: () => _openSheet(s),
        ),
      ));
    }
    if (items.isEmpty) {
      return Center(
        child: Padding(
          padding: EdgeInsets.all(24),
          child: Text(
            tr(
              'Долгое нажатие на волне и ведите — выделение. Потом «Регион».\n'
                  'Щипок — масштаб, двойное касание — весь файл.',
              'Long-press the wave and drag to select, then tap Region.\nPinch to zoom, double tap for the whole file.',
            ),
            textAlign: TextAlign.center,
          ),
        ),
      );
    }
    items.sort((a, b) => a.$1.compareTo(b.$1));
    return ListView(children: [for (final (_, w) in items) w]);
  }
}

String _speed(int percent) {
  final v = percent / 100;
  return v == v.roundToDouble()
      ? v.toStringAsFixed(0)
      : v.toStringAsFixed(2).replaceAll(RegExp(r'0$'), '');
}
