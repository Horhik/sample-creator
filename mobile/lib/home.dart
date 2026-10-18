import 'package:flutter/material.dart';

import 'editor.dart';
import 'processing.dart';
import 'review.dart';
import 'src/rust/api/review.dart' as review_api;
import 'display.dart';
import 'listening.dart';
import 'remote_ui.dart';
import 'sync_ui.dart';
import 'waveform.dart' show loadWaveStyle;
import 'l10n.dart';

/// The three pages of the desktop app: markup, region review, processing.
/// The player is shared; whichever page is shown takes it over.
class HomeShell extends StatefulWidget {
  const HomeShell({super.key});

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    applyFullscreen();
    syncAll();
    reconnectShared();
    loadListening();
    loadWaveStyle();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  // Leaving the app (or the recorder about to be unplugged): write the
  // markup out to volumes that keep it.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.paused) syncAll();
    if (state == AppLifecycleState.resumed) applyFullscreen();
  }

  int _page = 0;
  String? _file = review_api.getSetting(key: 'mobile.last_file');

  /// Set by "в разметку": the region to show once the file is open.
  int? _focusRegion;
  int _focusToken = 0;

  void _openFile(String path, {int? region}) {
    review_api.setSetting(key: 'mobile.last_file', value: path);
    setState(() {
      _file = path;
      _page = 0;
      _focusRegion = region;
      _focusToken++;
    });
  }

  void _switch(int page) {
    if (page == _page) return;
    setState(() => _page = page);
  }

  // A getter: labels follow the current language.
  static List<(IconData, String)> get _destinations => [
    (Icons.edit_note, tr('Разметка', 'Markup')),
    (Icons.view_list_outlined, tr('Обзор регионов', 'Region review')),
    (Icons.tune, tr('Обработка', 'Processing')),
  ];

  @override
  Widget build(BuildContext context) {
    final pages = IndexedStack(
      index: _page,
      children: [
        EditorPage(
          path: _file,
          active: _page == 0,
          focusRegion: _focusRegion,
          focusToken: _focusToken,
          onOpen: _openFile,
        ),
        ReviewPage(active: _page == 1, onOpen: _openFile),
        ProcessingPage(active: _page == 2),
      ],
    );
    // Landscape: the tabs move to a narrow rail on the left, leaving the
    // height to the waveform.
    if (MediaQuery.orientationOf(context) == Orientation.landscape) {
      return Scaffold(
        body: Row(
          children: [
            SafeArea(
              right: false,
              child: NavigationRail(
                selectedIndex: _page,
                onDestinationSelected: _switch,
                labelType: NavigationRailLabelType.none,
                destinations: [
                  for (final (icon, label) in _destinations)
                    NavigationRailDestination(
                      icon: Tooltip(message: label, child: Icon(icon)),
                      label: Text(label),
                    ),
                ],
              ),
            ),
            const VerticalDivider(width: 1),
            Expanded(child: pages),
          ],
        ),
      );
    }
    return Scaffold(
      body: pages,
      bottomNavigationBar: NavigationBar(
        selectedIndex: _page,
        onDestinationSelected: _switch,
        height: 64,
        destinations: [
          for (final (icon, label) in _destinations)
            NavigationDestination(icon: Icon(icon), label: label),
        ],
      ),
    );
  }
}
