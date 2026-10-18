import 'package:flutter/material.dart';

import 'l10n.dart';

/// Full screen, landscape: the markup page gives up its title bar and its
/// buttons move to the top of the navigation rail, so the wave gets the
/// whole height. The page fills this in; the rail shows it while the markup
/// page is on screen.
class MarkupRail extends ChangeNotifier {
  bool _shown = false;
  bool _canUndo = false;
  bool _canRedo = false;
  bool _listOpen = false;
  VoidCallback? files, toggleList, undo, redo;

  bool get shown => _shown;
  bool get canUndo => _canUndo;
  bool get canRedo => _canRedo;
  bool get listOpen => _listOpen;

  /// Notifies only on a change: the page calls this on every build.
  void update({
    required bool shown,
    bool canUndo = false,
    bool canRedo = false,
    bool listOpen = false,
  }) {
    if (shown == _shown &&
        canUndo == _canUndo &&
        canRedo == _canRedo &&
        listOpen == _listOpen) {
      return;
    }
    _shown = shown;
    _canUndo = canUndo;
    _canRedo = canRedo;
    _listOpen = listOpen;
    // Not while a frame is being built: the rail belongs to the parent.
    WidgetsBinding.instance.addPostFrameCallback((_) => notifyListeners());
  }
}

final markupRail = MarkupRail();

/// The markup buttons at the top of the rail: files, the list panel, undo,
/// redo, then a divider before the page tabs.
class MarkupRailButtons extends StatelessWidget {
  const MarkupRailButtons({super.key});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: markupRail,
      builder: (context, _) {
        final r = markupRail;
        if (!r.shown) return const SizedBox.shrink();
        return Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            IconButton(
              tooltip: tr('Файлы', 'Files'),
              icon: const Icon(Icons.menu),
              onPressed: r.files,
            ),
            IconButton(
              tooltip: r.listOpen
                  ? tr('Скрыть список', 'Hide the list')
                  : tr('Маркеры и регионы', 'Markers and regions'),
              isSelected: r.listOpen,
              icon: const Icon(Icons.view_sidebar_outlined),
              selectedIcon: const Icon(Icons.view_sidebar),
              onPressed: r.toggleList,
            ),
            IconButton(
              tooltip: tr('Отменить', 'Undo'),
              icon: const Icon(Icons.undo),
              onPressed: r.canUndo ? r.undo : null,
            ),
            IconButton(
              tooltip: tr('Повторить', 'Redo'),
              icon: const Icon(Icons.redo),
              onPressed: r.canRedo ? r.redo : null,
            ),
            const SizedBox(width: 48, child: Divider(height: 16)),
          ],
        );
      },
    );
  }
}
