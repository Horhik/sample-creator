<p align="center">
  <img src="data/icons/hicolor/scalable/apps/io.github.horhik.SampleCreator.svg" width="96" alt="">
</p>

<h1 align="center">Sample Creator</h1>

<p align="center">
  Mark up audio recordings and cut them into samples.<br>
  Linux (GTK4 / libadwaita) and Android.
</p>

<p align="center"><b>English</b> | <a href="README.ru.md">Русский</a></p>

---

Listen to a recording, drop markers, select regions and name them. No files
appear yet: regions stay drafts until you press Export and pick a folder.
The recordings themselves are never changed and nothing is written next to
them — handy when they sit right on a recorder's card.

Made for going through lots of audio files: WAV/BWF (16/24/32-bit int,
32-bit float), MP3 and FLAC. Recordings several hours long are fine: they are
read as a stream.

Desktop and phone screenshots are in [docs/SCREENSHOTS.md](docs/SCREENSHOTS.md).

## Features

- **Markup.** Waveform per channel, markers and regions with names and eight
  colours, undo/redo, autosave. Recorder marks (`cue`) are picked up, the start
  time comes from `bext`. A region can be made louder right here.
- **Region review.** Every region of every recording with a mini waveform,
  preview (processed regions play their result), colour filters, a jump back
  to the recording, deleting the ticked ones.
- **Export** only on request: name template, format (as the source, 24-bit,
  32-bit float), stereo as is, one channel or L/R as separate files. Each file
  gets `bext` pointing back at its source.
- **Region processing.** Speed ×0.25…×4, like a record or keeping the pitch;
  cutting into parts, reordering, overlapping and fading them on the result
  timeline; gain, normalising, trimming silence, a compressor and speech noise
  reduction. Results are cached, so previews start at once.
- **Markup file on the volume** (optional): `sample-creator.json` on the
  recorder's card carries the markup between the phone and the computer.
- **Desktop:** keyboard first (`?` lists every key), right-click menu.
- **Phone:** gestures, landscape layout, Material You colours, recorders over
  USB OTG.
- **English and Russian** interface: the system language by default, or pick
  one in the settings.

## Install

**Android 8+** — the APK from the [releases](../../releases) (arm64).

**Linux with Nix:**

```sh
nix run github:Horhik/sample-creator
```

Without Nix, and building the Android app: [docs/BUILDING.md](docs/BUILDING.md).

## How it is built

Rust: a shared [`engine`](engine) crate (WAV/BWF/MP3/FLAC reading, waveform,
playback on cpal, SQLite, export, processing), the desktop app on relm4 +
libadwaita, the phone app on Flutter through flutter_rust_bridge
([`mobile`](mobile)). The detailed plan and decisions are in
[AGENTS.md](AGENTS.md) (in Russian). Changes: [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)
