import 'dart:math' as math;

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
  });

  /// Columns are one per pixel; drawn with straight edges.
  final bool detailed;

  static const labelHeight = 22.0;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width;
    final top = labelHeight;
    final laneH = (size.height - top) / channels;
    final columns = peaks.data.length ~/ (channels * 2);

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
        _handle(canvas, x0, size.height, c);
        _handle(canvas, x1, size.height, c);
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
      _handle(canvas, x0, size.height, accent);
      _handle(canvas, x1, size.height, accent);
    }

    // Wave.
    final wave = Paint()
      ..color = scheme.onSurfaceVariant
      ..strokeWidth = 1;
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
      final (lo, hi) = waveColumnsOf(peaks, channels, channel: ch);
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

  void _handle(Canvas canvas, double x, double bottom, Color c) {
    canvas.drawRRect(
      RRect.fromRectAndRadius(
        Rect.fromCenter(center: Offset(x, bottom - 18), width: 14, height: 30),
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
  });

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
    final (lo, hi) = waveColumnsOf(peaks, channels);
    fillEnvelope(
      canvas,
      lo: lo,
      hi: hi,
      total: columns,
      left: 0,
      width: w,
      mid: h / 2,
      amp: (h - 6) / 2,
      paint: Paint()..color = scheme.onSurfaceVariant.withValues(alpha: 0.6),
    );

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

void loadWaveStyle() {
  waveDetailed.value = review.getSetting(key: 'mobile.waveform') == 'detailed';
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
