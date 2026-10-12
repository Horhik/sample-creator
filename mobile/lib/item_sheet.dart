import 'package:flutter/material.dart';

import 'src/rust/api/engine.dart' as engine;
import 'waveform.dart';
import 'l10n.dart';

/// Name, colour and, for a region, the export flag. Each change is saved at once.
/// Speed lives on the processing page, as on the desktop.
class ItemSheet extends StatefulWidget {
  final Selected item;
  final List<Color> palette;
  final bool focusName;

  /// Called after every change; carries the colour if that was what changed.
  final void Function(int? color) onChanged;

  const ItemSheet({
    super.key,
    required this.item,
    required this.palette,
    required this.focusName,
    required this.onChanged,
  });

  @override
  State<ItemSheet> createState() => _ItemSheetState();
}

class _ItemSheetState extends State<ItemSheet> {
  late final TextEditingController _name;
  engine.RegionView? _region;
  engine.MarkerView? _marker;

  /// The volume slider moves freely; the library is written on release.
  double? _dragGain;

  bool get _isRegion => widget.item.kind == Kind.region;
  engine.ItemKind get _kind =>
      _isRegion ? engine.ItemKind.region : engine.ItemKind.marker;

  @override
  void initState() {
    super.initState();
    _reload();
    _name = TextEditingController(text: _region?.name ?? _marker?.name ?? '');
  }

  @override
  void dispose() {
    _saveName();
    _name.dispose();
    super.dispose();
  }

  void _reload() {
    final m = engine.markup();
    _region = m.regions.where((r) => r.id == widget.item.id).firstOrNull;
    _marker = m.markers.where((x) => x.id == widget.item.id).firstOrNull;
  }

  Future<void> _do(Future<void> Function() action, {int? color}) async {
    await action();
    if (!mounted) return;
    setState(_reload);
    widget.onChanged(color);
  }

  void _saveName() {
    final current = _region?.name ?? _marker?.name;
    if (current == null || _name.text.trim() == current) return;
    engine
        .rename(kind: _kind, id: widget.item.id, name: _name.text)
        .then((_) => widget.onChanged(null));
  }

  @override
  Widget build(BuildContext context) {
    final r = _region;
    final m = _marker;
    if (r == null && m == null) return const SizedBox(height: 80);
    final color = r?.color ?? m!.color;

    return Padding(
      padding: EdgeInsets.fromLTRB(
        16,
        0,
        16,
        16 + MediaQuery.viewInsetsOf(context).bottom,
      ),
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            TextField(
              controller: _name,
              autofocus: widget.focusName,
              textCapitalization: TextCapitalization.sentences,
              decoration: InputDecoration(
                labelText: _isRegion
                    ? tr('Имя региона', 'Region name')
                    : tr('Имя маркера', 'Marker name'),
                border: const OutlineInputBorder(),
              ),
              onSubmitted: (_) {
                _saveName();
                Navigator.pop(context);
              },
            ),
            const SizedBox(height: 16),
            Wrap(
              spacing: 10,
              runSpacing: 10,
              children: [
                for (var i = 0; i < widget.palette.length; i++)
                  GestureDetector(
                    onTap: () => _do(
                      () => engine.setColor(
                        kind: _kind,
                        id: widget.item.id,
                        color: i,
                      ),
                      color: i,
                    ),
                    child: CircleAvatar(
                      radius: 18,
                      backgroundColor: widget.palette[i],
                      child: i == color
                          ? const Icon(Icons.check, color: Colors.black)
                          : null,
                    ),
                  ),
              ],
            ),
            if (r != null) ..._regionControls(r),
            const SizedBox(height: 8),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton.icon(
                style: TextButton.styleFrom(
                  foregroundColor: Theme.of(context).colorScheme.error,
                ),
                icon: const Icon(Icons.delete_outline),
                label: Text(tr('Удалить', 'Delete')),
                onPressed: () async {
                  await engine.delete(kind: _kind, id: widget.item.id);
                  widget.onChanged(null);
                  if (context.mounted) Navigator.pop(context);
                },
              ),
            ),
          ],
        ),
      ),
    );
  }

  List<Widget> _regionControls(engine.RegionView r) {
    final gain = (_dragGain ?? r.gain / 10).toDouble();
    return [
      const SizedBox(height: 8),
      Row(
        children: [
          const Icon(Icons.volume_up_outlined),
          const SizedBox(width: 12),
          Text(tr('Громкость', 'Volume')),
          const Spacer(),
          Text(gainLabel((gain * 10).round())),
        ],
      ),
      Slider(
        min: -24,
        max: 24,
        divisions: 96,
        value: gain.clamp(-24, 24),
        onChanged: r.normalize ? null : (v) => setState(() => _dragGain = v),
        onChangeEnd: (v) {
          _dragGain = null;
          _do(() => engine.setRegionGain(id: r.id, gain: (v * 10).round()));
        },
      ),
      if (r.normalize)
        Text(
          tr(
            'Включена нормализация (в «Обработке»)',
            'Normalising is on (on the Processing page)',
          ),
          style: Theme.of(context).textTheme.bodySmall,
        ),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: Text(tr('В экспорт', 'Export')),
        value: r.export_,
        onChanged: (v) =>
            _do(() => engine.setRegionExport(id: r.id, export_: v)),
      ),
    ];
  }
}
