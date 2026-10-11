import 'dart:math' as math;

import 'package:flutter/material.dart';

import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as api;
import 'waveform.dart';

/// A part as laid out on the output timeline (all frames).
class Clip {
  final int start, end; // source
  final int fadeIn, fadeOut;
  final int overlap;
  const Clip(this.start, this.end, this.fadeIn, this.fadeOut, this.overlap);

  factory Clip.of(api.PartView p) =>
      Clip(p.start, p.end, p.fadeIn, p.fadeOut, p.overlap);

  int get len => end - start;

  Clip copyWith({
    int? start,
    int? end,
    int? fadeIn,
    int? fadeOut,
    int? overlap,
  }) => Clip(
    start ?? this.start,
    end ?? this.end,
    fadeIn ?? this.fadeIn,
    fadeOut ?? this.fadeOut,
    overlap ?? this.overlap,
  );

  /// Fade lengths as they sound: both fit inside the clip.
  (int, int) get fades {
    var fi = math.min(fadeIn, len), fo = math.min(fadeOut, len);
    if (fi + fo > len && fi + fo > 0) {
      fi = (fi * len / (fi + fo)).floor();
      fo = len - fi;
    }
    return (fi, fo);
  }
}

/// Output start of each clip, mirroring `core::parts::positions` in Rust
/// (overlaps limited by the neighbours, the first clip at zero).
List<int> layoutClips(List<Clip> clips) {
  final out = <int>[];
  var end = 0;
  for (var i = 0; i < clips.length; i++) {
    final c = clips[i];
    var start = 0;
    if (i > 0) {
      final limit = math.min(clips[i - 1].len, c.len);
      start = math.max(0, end - math.min(c.overlap, limit));
    }
    out.add(start);
    end = start + c.len;
  }
  return out;
}

int timelineLength(List<Clip> clips) {
  final pos = layoutClips(clips);
  var len = 0;
  for (var i = 0; i < clips.length; i++) {
    len = math.max(len, pos[i] + clips[i].len);
  }
  return len;
}

/// Parts on two alternating lanes so overlaps show: the waveform of each part,
/// its fade ramps, the cursor and the playhead. The selected part gets handles:
/// edges (trim), fade circles at its top, and its body moves it.
class TimelinePainter extends CustomPainter {
  final List<Clip> clips;
  final List<int> positions;

  /// Per clip: where the visible slice starts and how wide it is (px), and
  /// its peaks.
  final List<(double, double, engine.PeaksView)?> waves;
  final int channels;
  final double viewStart, viewEnd;
  final Color color;
  final ColorScheme scheme;
  final int? selected;

  /// The selected part is held by a long press and follows the finger.
  final bool lifted;
  final int? cursor;
  final int? playhead;

  static const fadeHandleY = 14.0;

  TimelinePainter({
    required this.clips,
    required this.positions,
    required this.waves,
    required this.channels,
    required this.viewStart,
    required this.viewEnd,
    required this.color,
    required this.scheme,
    required this.selected,
    this.lifted = false,
    required this.cursor,
    required this.playhead,
  });

  static Rect laneRect(int index, Size size) {
    final laneH = size.height / 2;
    final top = (index % 2) * laneH;
    return Rect.fromLTRB(0, top + 3, size.width, top + laneH - 3);
  }

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width;
    final span = math.max(1.0, viewEnd - viewStart);
    double xOf(num f) => (f - viewStart) / span * w;

    canvas.save();
    canvas.clipRRect(
      RRect.fromRectAndRadius(Offset.zero & size, const Radius.circular(8)),
    );
    canvas.drawRect(
      Offset.zero & size,
      Paint()..color = scheme.surfaceContainerHighest,
    );
    canvas.drawLine(
      Offset(0, size.height / 2),
      Offset(w, size.height / 2),
      Paint()..color = scheme.outlineVariant,
    );

    for (var i = 0; i < clips.length; i++) {
      final c = clips[i];
      final lane = laneRect(i, size);
      final x0 = xOf(positions[i]), x1 = xOf(positions[i] + c.len);
      if (x1 < 0 || x0 > w) continue;
      final isSel = i == selected;
      final up = isSel && lifted;
      final block = RRect.fromRectAndRadius(
        Rect.fromLTRB(
          x0,
          lane.top,
          x1,
          lane.bottom,
        ).shift(Offset(0, up ? -3 : 0)),
        const Radius.circular(6),
      );
      if (up) {
        // Picked up: a shadow under the part.
        canvas.drawRRect(
          block.shift(const Offset(0, 5)),
          Paint()
            ..color = Colors.black.withValues(alpha: 0.35)
            ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 6),
        );
      }
      canvas.drawRRect(
        block,
        Paint()
          ..color = color.withValues(alpha: up ? 0.6 : (isSel ? 0.42 : 0.24)),
      );
      if (isSel) {
        canvas.drawRRect(
          block,
          Paint()
            ..color = color
            ..style = PaintingStyle.stroke
            ..strokeWidth = up ? 3 : 2,
        );
      }

      // Waveform of the visible slice.
      final wave = waves[i];
      if (wave != null) {
        final (left, width, peaks) = wave;
        final columns = peaks.data.length ~/ (math.max(channels, 1) * 2);
        final (lo, hi) = waveColumnsOf(peaks, channels);
        fillEnvelope(
          canvas,
          lo: lo,
          hi: hi,
          total: columns,
          left: left,
          width: width,
          mid: lane.center.dy,
          amp: lane.height / 2 - 3,
          paint: Paint()..color = scheme.onSurfaceVariant,
        );
      }

      // Fade ramps.
      final (fi, fo) = c.fades;
      final ramp = Paint()
        ..color = color
        ..strokeWidth = isSel ? 2.5 : 1.5;
      if (fi > 0) {
        canvas.drawLine(
          Offset(x0, lane.bottom),
          Offset(xOf(positions[i] + fi), lane.top),
          ramp,
        );
      }
      if (fo > 0) {
        canvas.drawLine(
          Offset(xOf(positions[i] + c.len - fo), lane.top),
          Offset(x1, lane.bottom),
          ramp,
        );
      }

      final tp = TextPainter(
        text: TextSpan(
          text: '${i + 1}',
          style: TextStyle(color: color, fontWeight: FontWeight.bold),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, Offset(math.max(x0, 0) + 6, lane.bottom - 20));

      if (isSel) {
        final handle = Paint()..color = color;
        for (final x in [x0, x1]) {
          canvas.drawRRect(
            RRect.fromRectAndRadius(
              Rect.fromCenter(
                center: Offset(x, lane.center.dy),
                width: 14,
                height: math.min(40, lane.height - 8),
              ),
              const Radius.circular(7),
            ),
            handle,
          );
        }
        for (final x in [
          xOf(positions[i] + fi),
          xOf(positions[i] + c.len - fo),
        ]) {
          final at = Offset(x, lane.top + fadeHandleY);
          canvas.drawCircle(at, 9, handle);
          canvas.drawCircle(
            at,
            9,
            Paint()
              ..color = scheme.surface
              ..style = PaintingStyle.stroke
              ..strokeWidth = 2,
          );
        }
      }
    }

    final c = cursor;
    if (c != null) {
      canvas.drawLine(
        Offset(xOf(c), 0),
        Offset(xOf(c), size.height),
        Paint()
          ..color = scheme.primary
          ..strokeWidth = 2.5,
      );
    }
    final ph = playhead;
    if (ph != null) {
      canvas.drawLine(
        Offset(xOf(ph), 0),
        Offset(xOf(ph), size.height),
        Paint()
          ..color = scheme.onSurface
          ..strokeWidth = 1.5,
      );
    }
    canvas.restore();
  }

  @override
  bool shouldRepaint(TimelinePainter old) => true;
}

/// Peaks of each clip's visible slice, for [TimelinePainter.waves].
List<(double, double, engine.PeaksView)?> clipWaves(
  api.ReviewRegion r,
  List<Clip> clips,
  List<int> positions,
  double viewStart,
  double viewEnd,
  double width,
) {
  final span = math.max(1.0, viewEnd - viewStart);
  return [
    for (var i = 0; i < clips.length; i++)
      () {
        if (!r.available) return null;
        final a = positions[i].toDouble(), b = a + clips[i].len;
        final v0 = math.max(a, viewStart), v1 = math.min(b, viewEnd);
        if (v1 <= v0) return null;
        final left = (v0 - viewStart) / span * width;
        final px = (v1 - v0) / span * width;
        if (px < 1) return null;
        final cols = waveColumns(v1 - v0, px);
        final src0 = clips[i].start + (v0 - a);
        final src1 = clips[i].start + (v1 - a);
        return (
          left,
          px,
          api.miniPeaks(
            path: r.path,
            channels: r.fileChannels,
            totalFrames: r.totalFrames,
            start: src0.floor(),
            end: src1.ceil(),
            columns: cols,
          ),
        );
      }(),
  ];
}

/// The clip under a touch: lanes alternate, so `y` picks even or odd clips.
int? clipAt(List<Clip> clips, List<int> positions, int t, double y, Size size) {
  final lane = y < size.height / 2 ? 0 : 1;
  int? any;
  for (var i = clips.length - 1; i >= 0; i--) {
    if (positions[i] <= t && t < positions[i] + clips[i].len) {
      if (i % 2 == lane) return i;
      any ??= i;
    }
  }
  return any;
}

/// Fades and the overlap with the previous part, in milliseconds.
Future<(int, int, int)?> editPartDialog(
  BuildContext context,
  api.ReviewRegion r,
  int index,
) {
  final p = r.parts[index];
  int ms(int frames) => (frames * 1000 / r.sampleRate).round();
  var fadeIn = ms(p.fadeIn).clamp(0, 3000).toDouble();
  var fadeOut = ms(p.fadeOut).clamp(0, 3000).toDouble();
  var overlap = ms(p.overlap).clamp(-3000, 3000).toDouble();
  return showDialog<(int, int, int)>(
    context: context,
    builder: (context) => StatefulBuilder(
      builder: (context, set) => AlertDialog(
        title: Text('Часть ${index + 1}'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Fade in: ${fadeIn.round()} мс'),
            Slider(
              max: 3000,
              divisions: 300,
              value: fadeIn,
              onChanged: (v) => set(() => fadeIn = v),
            ),
            Text('Fade out: ${fadeOut.round()} мс'),
            Slider(
              max: 3000,
              divisions: 300,
              value: fadeOut,
              onChanged: (v) => set(() => fadeOut = v),
            ),
            if (index > 0) ...[
              Text(
                overlap >= 0
                    ? 'Наложение на предыдущую: ${overlap.round()} мс'
                    : 'Пауза перед частью: ${-overlap.round()} мс',
              ),
              Slider(
                min: -3000,
                max: 3000,
                divisions: 600,
                value: overlap,
                onChanged: (v) => set(() => overlap = v),
              ),
            ],
            const Text(
              'Плавный переход: наложение и равные fade out / fade in у соседей.',
              style: TextStyle(fontSize: 12),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Отмена'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, (
              fadeIn.round(),
              fadeOut.round(),
              overlap.round(),
            )),
            child: const Text('Готово'),
          ),
        ],
      ),
    ),
  );
}

/// Applies [editPartDialog]'s result.
Future<void> applyPartDialog(
  api.ReviewRegion r,
  int index,
  (int, int, int) res,
) async {
  await api.setPartFades(
    id: r.id,
    index: index,
    fadeInMs: res.$1,
    fadeOutMs: res.$2,
  );
  if (index > 0) {
    await api.setPartOverlap(
      id: r.id,
      index: index,
      overlap: (res.$3 * r.sampleRate / 1000).round(),
    );
  }
}

/// "наложение 120 мс" / "пауза 50 мс" / "" for a part.
String overlapNote(api.ReviewRegion r, int index) {
  if (index == 0) return '';
  final ms = (r.parts[index].overlap * 1000 / r.sampleRate).round();
  if (ms > 0) return ' · наложение $ms мс';
  if (ms < 0) return ' · пауза ${-ms} мс';
  return '';
}

/// Helper so callers can show the timeline position as text.
String timelineTime(int frames, int rate) => formatTime(frames / rate);
