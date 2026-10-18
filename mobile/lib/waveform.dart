import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'l10n.dart';

import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as review;

/// What is visible: the first frame at the left edge and frames per pixel.
class WaveView {
  final double start;
  final double fpp;
  const WaveView(this.start, this.fpp);

  double frameAt(double x) => start + x * fpp;
  double xOf(num frame) => (frame - start) / fpp;
  double end(double width) => start + width * fpp;
}

enum Kind { marker, region }

class Selected {
  final Kind kind;
  final int id;
  const Selected(this.kind, this.id);

  @override
  bool operator ==(Object other) =>
      other is Selected && other.kind == kind && other.id == id;

  @override
  int get hashCode => Object.hash(kind, id);
}

String formatTime(double secs, {bool millis = true}) {
  if (secs.isNaN || secs < 0) secs = 0;
  final h = secs ~/ 3600;
  final m = (secs ~/ 60) % 60;
  final s = secs % 60;
  final ss = millis
      ? s.toStringAsFixed(3).padLeft(6, '0')
      : s.floor().toString().padLeft(2, '0');
  final mm = m.toString().padLeft(2, '0');
  return h > 0 ? '$h:$mm:$ss' : '$mm:$ss';
}

/// Per-channel lanes with regions, markers, the selection and the playhead.
class WavePainter extends CustomPainter {
  final engine.PeaksView peaks;
  final int channels;
  final WaveView view;
  final engine.MarkupView markup;
  final List<Color> palette;
  final Selected? selected;
  final (int, int)? selection;
  final int playhead;
  final int sampleRate;
  final Color accent;
  final ColorScheme scheme;

  WavePainter({
    required this.scheme,
    required this.peaks,
    required this.channels,
    required this.view,
    required this.markup,
    required this.palette,
    required this.selected,
    required this.selection,
    required this.playhead,
    required this.sampleRate,
    required this.accent,
    this.detailed = false,
    this.spectrum,
    this.look = WaveLook.plain,
    this.events = false,
  });

  /// Columns are one per pixel; drawn with straight edges.
  final bool detailed;

  /// Frequencies of the view, when the look or the events curve needs them.
  final engine.SpectrumData? spectrum;
  final WaveLook look;
  final bool events;

  static const labelHeight = 22.0;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width;
    final top = labelHeight;
    final laneH = (size.height - top) / channels;
    final columns = peaks.data.length ~/ (channels * 2);
    final sp = spectrum;
    // Until the analysis starts, the waveform stands in.
    final spectrogram = look == WaveLook.spectrogram && sp != null;
    if (spectrogram) {
      drawSpectrogram(canvas, sp, Rect.fromLTRB(0, top, w, size.height));
    }

    // Regions under the wave.
    for (final r in markup.regions) {
      final x0 = view.xOf(r.start), x1 = view.xOf(r.end);
      if (x1 < 0 || x0 > w) continue;
      final c = palette[r.color % palette.length];
      final isSel = selected == Selected(Kind.region, r.id);
      canvas.drawRect(
        Rect.fromLTRB(x0, top, x1, size.height),
        Paint()..color = c.withValues(alpha: isSel ? 0.30 : 0.16),
      );
      // Name plate along the top.
      canvas.drawRect(
        Rect.fromLTRB(x0, 0, x1, top - 2),
        Paint()..color = c.withValues(alpha: isSel ? 0.9 : 0.55),
      );
      _label(
        canvas,
        r.name.isEmpty ? '—' : r.name,
        x0 + 4,
        3,
        x1 - x0 - 8,
        Colors.black,
      );
      if (isSel) {
        final edge = Paint()
          ..color = c
          ..strokeWidth = 3;
        canvas.drawLine(Offset(x0, top), Offset(x0, size.height), edge);
        canvas.drawLine(Offset(x1, top), Offset(x1, size.height), edge);
        _handle(canvas, x0, top, c);
        _handle(canvas, x1, top, c);
      }
    }

    // Selection.
    final sel = selection;
    if (sel != null) {
      final x0 = view.xOf(sel.$1), x1 = view.xOf(sel.$2);
      canvas.drawRect(
        Rect.fromLTRB(x0, top, x1, size.height),
        Paint()..color = accent.withValues(alpha: 0.25),
      );
      final edge = Paint()
        ..color = accent
        ..strokeWidth = 2;
      canvas.drawLine(Offset(x0, top), Offset(x0, size.height), edge);
      canvas.drawLine(Offset(x1, top), Offset(x1, size.height), edge);
      _handle(canvas, x0, top, accent);
      _handle(canvas, x1, top, accent);
    }

    // Wave.
    final wave = Paint()
      ..color = scheme.onSurfaceVariant
      ..strokeWidth = 1;
    if (look == WaveLook.colored && sp != null) {
      wave.shader = colorShader(sp, w, scheme.onSurfaceVariant);
    }
    final mid = Paint()..color = scheme.outlineVariant.withValues(alpha: 0.5);
    for (var ch = 0; ch < channels; ch++) {
      final y0 = top + laneH * ch;
      final cy = y0 + laneH / 2;
      final amp = laneH / 2 - 2;
      canvas.drawLine(Offset(0, cy), Offset(w, cy), mid);
      if (ch > 0) {
        canvas.drawLine(
          Offset(0, y0),
          Offset(w, y0),
          Paint()..color = scheme.outlineVariant,
        );
      }
      if (spectrogram) continue;
      final (lo, hi) = shownColumnsOf(peaks, channels, channel: ch);
      fillEnvelope(
        canvas,
        lo: lo,
        hi: hi,
        total: columns,
        left: 0,
        width: w,
        mid: cy,
        amp: amp,
        paint: wave,
        smooth: !detailed,
      );
    }

    if (events && sp != null) {
      drawEvents(canvas, sp, Rect.fromLTRB(0, top + 4, w, size.height - 4));
    }

    // Markers.
    for (final m in markup.markers) {
      final x = view.xOf(m.pos);
      if (x < -50 || x > w + 50) continue;
      final c = palette[m.color % palette.length];
      final isSel = selected == Selected(Kind.marker, m.id);
      canvas.drawLine(
        Offset(x, top),
        Offset(x, size.height),
        Paint()
          ..color = c
          ..strokeWidth = isSel ? 3 : 1.5,
      );
      final flag = Path()
        ..moveTo(x, top)
        ..lineTo(x + 10, top)
        ..lineTo(x, top + 12)
        ..close();
      canvas.drawPath(flag, Paint()..color = c);
      if (m.name.isNotEmpty) {
        _label(canvas, m.name, x + 12, top + 1, 120, c);
      }
    }

    // Playhead.
    final px = view.xOf(playhead);
    if (px >= 0 && px <= w) {
      canvas.drawLine(
        Offset(px, 0),
        Offset(px, size.height),
        Paint()
          ..color = scheme.onSurface
          ..strokeWidth = 1.5,
      );
    }
  }

  /// Grip at the top of an edge, under the name plate: the floating
  /// controls cover the bottom of the wave.
  void _handle(Canvas canvas, double x, double top, Color c) {
    canvas.drawRRect(
      RRect.fromRectAndRadius(
        Rect.fromCenter(center: Offset(x, top + 18), width: 14, height: 30),
        const Radius.circular(7),
      ),
      Paint()..color = c,
    );
  }

  void _label(
    Canvas canvas,
    String text,
    double x,
    double y,
    double maxW,
    Color color,
  ) {
    if (maxW < 12) return;
    final tp = TextPainter(
      text: TextSpan(
        text: text,
        style: TextStyle(
          color: color,
          fontSize: 12,
          fontWeight: FontWeight.w600,
        ),
      ),
      maxLines: 1,
      ellipsis: '…',
      textDirection: TextDirection.ltr,
    )..layout(maxWidth: maxW);
    tp.paint(canvas, Offset(math.max(x, 2), y));
  }

  @override
  bool shouldRepaint(WavePainter old) => true;
}

/// The whole file in one strip, with the visible window framed.
class OverviewPainter extends CustomPainter {
  final engine.PeaksView peaks;
  final int channels;
  final int total;
  final WaveView view;
  final double viewWidth;
  final engine.MarkupView markup;
  final List<Color> palette;
  final int playhead;
  final ColorScheme scheme;

  OverviewPainter({
    required this.scheme,
    required this.peaks,
    required this.channels,
    required this.total,
    required this.view,
    required this.viewWidth,
    required this.markup,
    required this.palette,
    required this.playhead,
    this.spectrum,
  });

  /// Colours and events of the whole file, when they are shown.
  final engine.SpectrumData? spectrum;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width, h = size.height;
    double xOf(num f) => f / total * w;
    canvas.drawRect(
      Offset.zero & size,
      Paint()..color = scheme.surfaceContainerHighest,
    );

    for (final r in markup.regions) {
      canvas.drawRect(
        Rect.fromLTRB(
          xOf(r.start),
          h - 4,
          math.max(xOf(r.end), xOf(r.start) + 1),
          h,
        ),
        Paint()..color = palette[r.color % palette.length],
      );
    }

    final columns = peaks.data.length ~/ (channels * 2);
    final (lo, hi) = shownColumnsOf(peaks, channels);
    fillEnvelope(
      canvas,
      lo: lo,
      hi: hi,
      total: columns,
      left: 0,
      width: w,
      mid: h / 2,
      amp: (h - 6) / 2,
      paint: Paint()
        ..color = scheme.onSurfaceVariant.withValues(alpha: 0.6)
        ..shader = spectrum != null && waveLook.value == WaveLook.colored
            ? colorShader(spectrum!, w, scheme.onSurfaceVariant)
            : null,
    );
    if (spectrum != null && waveEvents.value) {
      drawEvents(canvas, spectrum!, Rect.fromLTRB(0, 2, w, h - 2));
    }

    final frame = Rect.fromLTRB(
      xOf(view.start).clamp(0, w),
      0,
      xOf(view.end(viewWidth)).clamp(0, w),
      h,
    );
    canvas.drawRect(
      frame,
      Paint()..color = scheme.primary.withValues(alpha: 0.14),
    );
    canvas.drawRect(
      frame,
      Paint()
        ..color = scheme.primary
        ..style = PaintingStyle.stroke,
    );
    final px = xOf(playhead);
    canvas.drawLine(
      Offset(px, 0),
      Offset(px, h),
      Paint()..color = scheme.onSurface,
    );
  }

  @override
  bool shouldRepaint(OverviewPainter old) => true;
}

/// A region's waveform in a list row: channels overlaid, in the region colour.
class MiniWavePainter extends CustomPainter {
  final engine.PeaksView peaks;
  final int channels;
  final Color color;

  /// Where a preview is (0..1): a line across the wave. Repaints on its own.
  final ValueListenable<double?>? progress;

  MiniWavePainter({
    required this.peaks,
    required this.channels,
    required this.color,
    this.progress,
  }) : super(repaint: progress);

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRRect(
      RRect.fromRectAndRadius(Offset.zero & size, const Radius.circular(6)),
      Paint()..color = color.withValues(alpha: 0.12),
    );
    final columns = peaks.data.length ~/ (math.max(channels, 1) * 2);
    final (lo, hi) = waveColumnsOf(peaks, channels);
    fillEnvelope(
      canvas,
      lo: lo,
      hi: hi,
      total: columns,
      left: 0,
      width: size.width,
      mid: size.height / 2,
      amp: size.height / 2 - 2,
      paint: Paint()..color = color,
    );
    final p = progress?.value;
    if (p != null) {
      final x = p * size.width;
      canvas.drawLine(
        Offset(x, 0),
        Offset(x, size.height),
        Paint()
          ..color = Colors.white
          ..strokeWidth = 2,
      );
    }
  }

  @override
  bool shouldRepaint(MiniWavePainter old) => true;
}

/// " → 2.10 с (×2, тон сохранён, склейка)" for a processed region, else "".
/// "+3.5 dB", "−2 dB", "0 dB" for a gain in tenths of a dB.
String gainLabel(int tenths) {
  final db = tenths / 10;
  final unit = tr('дБ', 'dB');
  if (tenths == 0) return '0 $unit';
  final text = db == db.roundToDouble()
      ? db.toStringAsFixed(0)
      : db.toStringAsFixed(1);
  return '${db > 0 ? '+' : ''}$text $unit';
}

String regionNote(review.ReviewRegion r) {
  if (!r.processed) return '';
  final notes = <String>[];
  if (r.speed != 100) {
    var factor = (r.speed / 100).toStringAsFixed(2);
    factor = factor.replaceFirst(RegExp(r'\.?0+$'), '');
    notes.add('×$factor');
    if (r.keepPitch) notes.add(tr('тон сохранён', 'pitch kept'));
  }
  if (r.edited) notes.add(tr('склейка', 'spliced'));
  notes.addAll(r.fxNotes);
  final out = r.outputFrames / r.sampleRate;
  return ' → ${out.toStringAsFixed(2)} $sec (${notes.join(', ')})';
}

/// Detailed waveform (a setting): a column per pixel with straight edges,
/// down to single samples, instead of smooth curves through the summary.
final waveDetailed = ValueNotifier<bool>(false);

/// Waveform height on the decibel scale (a setting): quiet sounds show.
final waveDecibels = ValueNotifier<bool>(false);

void loadWaveStyle() {
  waveDetailed.value = review.getSetting(key: 'mobile.waveform') == 'detailed';
  waveDecibels.value = review.getSetting(key: 'mobile.wave_scale') == 'db';
  final look = review.getSetting(key: 'mobile.wave_view');
  waveLook.value = WaveLook.values.firstWhere(
    (l) => l.name == look,
    orElse: () => WaveLook.plain,
  );
  waveEvents.value = review.getSetting(key: 'mobile.wave_events') == 'on';
}

void setWaveDecibels(bool on) {
  waveDecibels.value = on;
  review.setSetting(key: 'mobile.wave_scale', value: on ? 'db' : 'linear');
}

/// How the recording is shown (a setting).
enum WaveLook {
  /// Loudness only.
  plain,

  /// Frequencies over time instead of the waveform.
  spectrogram,

  /// The waveform coloured by where its energy sits: red low, green middle,
  /// blue high.
  colored,
}

String waveLookLabel(WaveLook l) => switch (l) {
  WaveLook.plain => tr('Волна', 'Waveform'),
  WaveLook.spectrogram => tr('Спектрограмма', 'Spectrogram'),
  WaveLook.colored => tr('Цветная волна', 'Coloured waveform'),
};

final waveLook = ValueNotifier<WaveLook>(WaveLook.plain);

/// A curve rising where the sound changes (a setting).
final waveEvents = ValueNotifier<bool>(false);

/// Whether the look or the events curve needs `engine.spectrumView`.
bool get waveNeedsSpectrum =>
    waveLook.value != WaveLook.plain || waveEvents.value;

void setWaveLook(WaveLook l) {
  waveLook.value = l;
  review.setSetting(key: 'mobile.wave_view', value: l.name);
}

void setWaveEvents(bool on) {
  waveEvents.value = on;
  review.setSetting(key: 'mobile.wave_events', value: on ? 'on' : 'off');
}

Int32List? _colormap;

/// Spectrogram colours by brightness (from the engine, like the desktop).
Int32List get spectrumColormap => _colormap ??= Int32List.fromList(
  engine.spectrumColormap().map((c) => c.toSigned(32)).toList(),
);

/// Most columns a spectrogram is drawn with (vertex indices are 16-bit).
const maxSpectrumColumns = 1000;

/// The spectrogram of `d` over `r`, low frequencies at the bottom: a grid of
/// vertices at the cell centres, colours blended between them.
void drawSpectrogram(Canvas canvas, engine.SpectrumData d, Rect r) {
  final bands = d.bands;
  final cols = d.levels.length ~/ math.max(bands, 1);
  if (cols < 2 || bands < 2) return;
  final map = spectrumColormap;
  final positions = Float32List(cols * bands * 2);
  final colors = Int32List(cols * bands);
  final dx = r.width / cols, dy = r.height / bands;
  for (var x = 0; x < cols; x++) {
    final px = x == 0
        ? r.left
        : x == cols - 1
        ? r.right
        : r.left + (x + 0.5) * dx;
    for (var b = 0; b < bands; b++) {
      final py = b == 0
          ? r.bottom
          : b == bands - 1
          ? r.top
          : r.bottom - (b + 0.5) * dy;
      final i = x * bands + b;
      positions[i * 2] = px;
      positions[i * 2 + 1] = py;
      colors[i] = map[d.levels[i]];
    }
  }
  final vertices = ui.Vertices.raw(
    VertexMode.triangles,
    positions,
    colors: colors,
    indices: _gridIndices(cols, bands),
  );
  // dst: the vertex colours alone, the paint adds nothing.
  canvas.drawVertices(vertices, BlendMode.dst, Paint());
}

final _indices = <(int, int), Uint16List>{};

Uint16List _gridIndices(int cols, int bands) =>
    _indices.putIfAbsent((cols, bands), () {
      final out = Uint16List((cols - 1) * (bands - 1) * 6);
      var k = 0;
      for (var x = 0; x < cols - 1; x++) {
        for (var b = 0; b < bands - 1; b++) {
          final a = x * bands + b, c = a + bands;
          out.setAll(k, [a, c, a + 1, a + 1, c, c + 1]);
          k += 6;
        }
      }
      return out;
    });

/// A horizontal gradient through the colours of `d` across `width`;
/// `fallback` where they are not analysed yet.
Shader colorShader(engine.SpectrumData d, double width, Color fallback) {
  final n = d.colors.length;
  final colors = <Color>[];
  final stops = <double>[];
  for (var i = 0; i < n; i++) {
    final c = d.colors[i];
    colors.add(c == 0 ? fallback.withValues(alpha: 0.6) : Color(c));
    stops.add((i + 0.5) / n);
  }
  if (n < 2) {
    return ui.Gradient.linear(Offset.zero, Offset(width, 0), [
      fallback,
      fallback,
    ]);
  }
  return ui.Gradient.linear(Offset.zero, Offset(width, 0), colors, stops);
}

/// Colour of the events curve: reads over the waveform and the spectrogram.
const eventsColor = Color(0xFF40E6FF);

/// The events curve of `d` over `r`, rising from the bottom, lightly filled.
void drawEvents(Canvas canvas, engine.SpectrumData d, Rect r) {
  final n = d.events.length;
  if (n == 0) return;
  final step = r.width / n;
  final fill = Paint()..color = eventsColor.withValues(alpha: 0.15);
  final line = Paint()
    ..color = eventsColor
    ..style = PaintingStyle.stroke
    ..strokeWidth = 1.5;
  final run = <Offset>[];
  void flush() {
    if (run.length >= 2) {
      canvas.drawPath(
        Path()..addPolygon([
          Offset(run.first.dx, r.bottom),
          ...run,
          Offset(run.last.dx, r.bottom),
        ], true),
        fill,
      );
      canvas.drawPath(Path()..addPolygon(run, false), line);
    }
    run.clear();
  }

  for (var i = 0; i < n; i++) {
    final e = d.events[i];
    if (e.isNaN) {
      flush();
    } else {
      run.add(Offset(r.left + (i + 0.5) * step, r.bottom - e * r.height));
    }
  }
  flush();
}

/// Bottom of the decibel scale (mirrors `peaks::DB_FLOOR`).
const dbFloor = -60.0;

/// A sample value on the decibel scale, sign kept (mirrors `peaks::db_scale`):
/// 0 dB is the full height, [dbFloor] the middle line.
double dbScale(double x) {
  final a = x.abs();
  if (a <= 0) return 0;
  final db = 20 * math.log(a) / math.ln10;
  final y = (1 - db / dbFloor).clamp(0.0, 1.0).toDouble();
  return x < 0 ? -y : y;
}

/// [waveColumnsOf] on the scale the setting asks for.
(List<double>, List<double>) shownColumnsOf(
  engine.PeaksView peaks,
  int channels, {
  int? channel,
}) {
  final (lo, hi) = waveColumnsOf(peaks, channels, channel: channel);
  if (!waveDecibels.value) return (lo, hi);
  return (lo.map(dbScale).toList(), hi.map(dbScale).toList());
}

void setWaveDetailed(bool on) {
  waveDetailed.value = on;
  review.setSetting(key: 'mobile.waveform', value: on ? 'detailed' : 'smooth');
}

/// Closest zoom of the detailed waveform: one sample per pixel.
const detailMinFramesPerPx = 1.0;

/// Closest zoom, frames per pixel (mirrors `peaks::MIN_FRAMES_PER_PX`): a
/// summary bin spans at most 4 px, so the waveform stays smooth and fills the
/// view.
const minFramesPerPx = 64.0;

/// Columns to ask the engine for: one per pixel, or one per summary bin (256
/// frames) once zoomed in further, so the curve between bins is smoothed
/// instead of drawn as steps.
int waveColumns(double frames, double width) =>
    math.max(1, math.min(width.floor(), (frames / 256).ceil()));

/// Per column min and max of `channel` (or all channels merged when null);
/// stops at the first column not computed yet.
(List<double>, List<double>) waveColumnsOf(
  engine.PeaksView peaks,
  int channels, {
  int? channel,
}) {
  final columns = peaks.data.length ~/ (math.max(channels, 1) * 2);
  final lo = <double>[], hi = <double>[];
  for (var x = 0; x < columns; x++) {
    var l = 0.0, h = 0.0;
    var any = false;
    for (var ch = 0; ch < channels; ch++) {
      if (channel != null && ch != channel) continue;
      final a = peaks.data[ch * columns * 2 + x * 2];
      final b = peaks.data[ch * columns * 2 + x * 2 + 1];
      if (a.isNaN) continue;
      any = true;
      l = math.min(l, a);
      h = math.max(h, b);
    }
    if (!any) break;
    lo.add(l);
    hi.add(h);
  }
  return (lo, hi);
}

/// Fills between the max and min curves around `mid` with smooth
/// (Catmull-Rom) edges; column `i` of `total` sits at the middle of its slice
/// of `[left, left + width)`.
void fillEnvelope(
  Canvas canvas, {
  required List<double> lo,
  required List<double> hi,
  required int total,
  required double left,
  required double width,
  required double mid,
  required double amp,
  required Paint paint,
  bool smooth = true,
}) {
  final n = lo.length;
  if (n == 0 || total == 0) return;
  final step = width / total;
  double xAt(int i) => left + (i + 0.5) * step;
  final top = [
    for (var i = 0; i < n; i++) Offset(xAt(i), mid - hi[i] * amp - 0.5),
  ];
  final bottom = [
    for (var i = n - 1; i >= 0; i--) Offset(xAt(i), mid - lo[i] * amp + 0.5),
  ];
  final path = Path()..moveTo(top[0].dx, top[0].dy);
  if (smooth) {
    _smoothThrough(path, top);
    path.lineTo(bottom[0].dx, bottom[0].dy);
    _smoothThrough(path, bottom);
  } else {
    for (final p in [...top.skip(1), ...bottom]) {
      path.lineTo(p.dx, p.dy);
    }
  }
  path.close();
  canvas.drawPath(path, paint);
}

void _smoothThrough(Path path, List<Offset> p) {
  Offset at(int i) => p[i.clamp(0, p.length - 1)];
  for (var i = 0; i < p.length - 1; i++) {
    final p0 = at(i - 1), p1 = at(i), p2 = at(i + 1), p3 = at(i + 2);
    path.cubicTo(
      p1.dx + (p2.dx - p0.dx) / 6,
      p1.dy + (p2.dy - p0.dy) / 6,
      p2.dx - (p3.dx - p1.dx) / 6,
      p2.dy - (p3.dy - p1.dy) / 6,
      p2.dx,
      p2.dy,
    );
  }
}

/// An error as a person reads it: the message without the Rust backtrace and
/// the bridge's exception wrapper.
String friendlyError(Object e) {
  var text = '$e';
  final wrapped = RegExp(
    r'^\w+Exception\((.*)\)$',
    dotAll: true,
  ).firstMatch(text);
  if (wrapped != null) text = wrapped.group(1)!;
  final cut = text.indexOf('\n\nStack backtrace');
  if (cut >= 0) text = text.substring(0, cut);
  return text.trim();
}
