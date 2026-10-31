# KD-Emulator

An Android emulator for Game Boy Advance, Nintendo DS, GameCube and Wii, tuned for Google Tensor phones (G1–G5).
It has a Rust core and a Kotlin / Jetpack Compose app modelled on Delta:
- an artwork library with one page per console;
- controller skins (including imported `.deltaskin` files);
- save states with screenshots, rewind, fast-forward and cheats;
- auto-resume.

**Download:** the APK is on the [Releases](../../releases) page.

<p>
<img src="docs/screenshots/library-ds.jpg" width="180" alt="DS library page with game icons">
<img src="docs/screenshots/game.jpg" width="180" alt="A GBA game with the built-in controller skin">
<img src="docs/screenshots/pause-menu.jpg" width="180" alt="Pause menu">
<img src="docs/screenshots/save-states.jpg" width="180" alt="Save state browser">
</p>

The screenshots come from the app's JVM UI tests running synthetic test cartridges.

Every system has an **Animal Crossing profile**. When it recognises an Animal Crossing game code, the emulator sets
the console up the way that game expects:

- **Game Boy Advance:** link-cable companion. The GameCube game's GBA side-programs boot over JoyBus multiboot.
- **Nintendo DS** (*Wild World*): stylus-first controls, a time-travel clock and a 256 KB Flash save.
- **GameCube** (*Animal Crossing*): Memory Card 251, a GBA plugged into controller port 2 and a time-travel clock.
- **Wii** (*City Folk*): pointer controls, SD card and a time-travel clock.

Time travel changes only the game's clock, never your phone's. The offset is remembered per game.

> GameCube and Wii support is still partial: commercial games, Animal Crossing included, don't reach gameplay yet.
> See [STATUS.md](STATUS.md) for exactly what works and what's missing.

## Building

```sh
cargo test --workspace                       # host tests, no device needed
cd android && ./gradlew assembleDebug        # APK; builds libkd_emulator.so via cargo-ndk
cd android && ./gradlew testDebugUnitTest    # UI tests (Robolectric + host build of the native library)
```

The APK build needs the Android SDK (platform 35), NDK r28, `rustup target add aarch64-linux-android` and
`cargo install cargo-ndk`. The app targets arm64-v8a, Android 12+ (minSdk 31). Its native library is linked for
16 KB pages.

[CLAUDE.md](CLAUDE.md) covers the architecture, memory maps and the Tensor GPU/thermal rules.

## Games and firmware

No BIOS, firmware, keys or game data are included or needed for GBA, DS and GameCube: the BIOS and firmware are
emulated at a high level. Use dumps of games you own. Wii retail discs need your own console's common key.
Animal Crossing is a trademark of Nintendo. This project isn't affiliated with or endorsed by Nintendo, and its
"Island" theme and icon are original artwork.
