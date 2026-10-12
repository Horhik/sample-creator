import 'package:flutter/material.dart';

import 'l10n.dart';
import 'src/rust/api/engine.dart' as engine;
import 'src/rust/api/review.dart' as review_api;

/// How loud everything plays, in dB on top of the recording (and of a
/// region's own gain). Only for listening: export is not affected.
final listeningDb = ValueNotifier<double>(0);

const _key = 'mobile.listening_db';
const _min = -24.0;
const _max = 24.0;

void loadListening() {
  final db = double.tryParse(review_api.getSetting(key: _key) ?? '') ?? 0;
  _apply(db.clamp(_min, _max));
}

void _apply(double db) {
  listeningDb.value = db;
  engine.setListeningDb(db: db);
}

void _save() {
  review_api.setSetting(key: _key, value: listeningDb.value.toStringAsFixed(0));
}

String _db(double db) => '${db > 0 ? '+' : ''}${db.round()} ${tr('дБ', 'dB')}';

/// A speaker button: shows the listening volume when it is not 0 dB and
/// opens a slider.
class ListeningButton extends StatelessWidget {
  const ListeningButton({super.key});

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<double>(
      valueListenable: listeningDb,
      builder: (context, db, _) {
        final icon = Icon(
          db < 0
              ? Icons.volume_down
              : db > 0
              ? Icons.volume_up
              : Icons.volume_up_outlined,
        );
        void open() => _sheet(context);
        final tooltip = tr('Громкость прослушивания', 'Listening volume');
        if (db == 0) {
          return IconButton(tooltip: tooltip, icon: icon, onPressed: open);
        }
        return TextButton.icon(
          onPressed: open,
          icon: icon,
          label: Text(_db(db)),
        );
      },
    );
  }

  void _sheet(BuildContext context) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 16),
          child: ValueListenableBuilder<double>(
            valueListenable: listeningDb,
            builder: (context, db, _) => Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        tr('Громкость прослушивания', 'Listening volume'),
                        style: Theme.of(context).textTheme.titleMedium,
                      ),
                    ),
                    Text(
                      _db(db),
                      style: Theme.of(context).textTheme.titleMedium,
                    ),
                  ],
                ),
                Slider(
                  min: _min,
                  max: _max,
                  divisions: (_max - _min).round(),
                  value: db,
                  label: _db(db),
                  onChanged: _apply,
                  onChangeEnd: (_) => _save(),
                ),
                Text(
                  tr(
                    'Для всего, что играет; в экспорт не попадает. Выше 0 дБ '
                        'громкие места перегружаются.',
                    'For everything that plays; export is not affected. Above '
                        '0 dB loud parts clip.',
                  ),
                  style: Theme.of(context).textTheme.bodySmall,
                ),
                Align(
                  alignment: Alignment.centerRight,
                  child: TextButton(
                    onPressed: db == 0
                        ? null
                        : () {
                            _apply(0);
                            _save();
                          },
                    child: Text(tr('Сбросить на 0 дБ', 'Reset to 0 dB')),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
