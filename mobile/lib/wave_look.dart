import 'package:flutter/material.dart';

import 'l10n.dart';
import 'waveform.dart';

/// Picks how the recording is shown (waveform, spectrogram, coloured
/// waveform) and the events curve; both are kept as settings.
class WaveLookButton extends StatelessWidget {
  const WaveLookButton({super.key});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([waveLook, waveEvents]),
      builder: (context, _) => PopupMenuButton<Object>(
        tooltip: tr('Вид волны', 'Waveform view'),
        icon: Icon(switch (waveLook.value) {
          WaveLook.plain => Icons.graphic_eq,
          WaveLook.spectrogram => Icons.gradient,
          WaveLook.colored => Icons.palette_outlined,
        }),
        onSelected: (v) {
          if (v is WaveLook) setWaveLook(v);
          if (v == #events) setWaveEvents(!waveEvents.value);
        },
        itemBuilder: (context) => [
          for (final l in WaveLook.values)
            CheckedPopupMenuItem<Object>(
              value: l,
              checked: waveLook.value == l,
              child: Text(waveLookLabel(l)),
            ),
          const PopupMenuDivider(),
          CheckedPopupMenuItem<Object>(
            value: #events,
            checked: waveEvents.value,
            child: Text(tr('Кривая событий', 'Events curve')),
          ),
        ],
      ),
    );
  }
}

/// What the looks show, for the settings.
String waveLookHint() => tr(
  'Спектрограмма — частоты во времени, низкие внизу. Цветная волна: красный — низкие, зелёный — средние, синий — высокие. Кривая событий поднимается там, где звук меняется.',
  'Spectrogram: frequencies over time, low at the bottom. Coloured waveform: red low, green middle, blue high. The events curve rises where the sound changes.',
);
