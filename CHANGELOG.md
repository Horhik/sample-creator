# Changes

<p><b>English</b> | <a href="CHANGELOG.ru.md">Русский</a></p>

## 0.3.1

- Hold a recording from a computer in the file list to listen to it from the
  start while it streams; let go to stop. What was heard is kept, and opening
  the recording then downloads only the rest.
- A cancelled or broken download goes on later from where it stopped.
- Listening volume next to the transport (phone and desktop): −24…+24 dB for
  everything that plays, loud enough to clip a quiet take; export is not
  affected.
- Stability: local files play again at once after a shared one (the player
  no longer waits on the network); a computer that does not answer is given
  up on in 10 seconds instead of hanging every request; the region review no
  longer asks the network while drawing and checks each recording once; its
  mini waveforms no longer download whole shared recordings in the
  background.

## 0.3.0

- Folder sharing: right-click a folder in the desktop tree → Share folder…,
  scan the QR code on the phone, and its recordings open on the phone as if
  they were local: markup, review, processing and export work as usual. It
  works on the home network and over the internet (iroh: a direct connection
  where possible, otherwise through public relays; encrypted end to end). The
  code is the password, and access is read-only. The phone remembers shared
  folders and reconnects when the computer shares the folder again.
  A recording from a computer is downloaded whole before it opens (progress,
  speed, time left, cancel); the copy stays in the app cache, so it opens at
  once next time and also while the computer is away.
- The phone APK is smaller (20 MB instead of 29): native libraries are
  compressed inside it.

## 0.2.0

- English and Russian interface. The system language by default; pick one in
  the settings (desktop: the ☰ menu, after a restart; phone: the side panel,
  at once).
- Region volume and dynamics: gain, normalising, trimming silence, a
  compressor and speech noise reduction (RNNoise). Gain can be raised right in
  markup, and the region is heard louder at once.
- Processed audio is cached: previews without recomputing, also after a
  restart; the result is rendered ahead when you leave the processing page.
- Region review: a processed region shows and plays its result, the play
  button no longer flips back while rendering and can stop playback, a line
  follows playback on the phone, and the ticked regions can be deleted after a
  confirmation.
- Processing: Original and Result are pinned at the top.
- Phone: processing in landscape keeps the region list in a narrow side
  column; haptics when a selection starts and when a marker or an edge is
  caught and released.
- Desktop: the open file is highlighted in the tree.
- The project is described without tying it to one recorder model.

## 0.1.0

First release: marking up recordings, region review and export, processing
(speed, cutting with overlaps and fades), the markup file on the volume; the
GTK4 desktop app and the Android app.
