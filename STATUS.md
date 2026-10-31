# KD-Emulator — status

What is implemented, how each part is verified, and what is still missing.
Update this file in the same commit as the code it describes.

**Last verified:** 157 host tests passing (`cargo test --workspace`) and 2 app UI tests (Robolectric, `./gradlew testDebugUnitTest`). `cargo clippy --workspace --all-targets -- -D warnings`
is clean. `cargo check --target aarch64-linux-android -p android-jni` passes. `libkd_emulator.so` was built with
NDK r28 / cargo-ndk 4.1.2, and every `LOAD` segment is `Align 0x4000`, with 30 JNI exports.
`./gradlew assembleDebug` and `assembleRelease` (R8) both succeed (AGP 8.7.3, Kotlin 2.1.0, compileSdk 35). In
both APKs the stripped 1.1 MB `libkd_emulator.so` is stored uncompressed and passes `zipalign -c -P 16`. All 30
`native*` methods keep their names after R8, and their dex signatures match the Rust exports.

> **Honest summary.** KD-Emulator runs Game Boy Advance and Nintendo DS
> games. Both cores share one runtime and pass hardware-level tests on
> synthetic programs that run on the emulated CPUs. GameCube and Wii support
> was removed in 0.3.0: neither could reach gameplay in commercial games.

## Phases

### Phase 1 — workspace, 16 KB pages, runtime (`kd-core`): done
- [x] Linker flags `-z max-page-size=16384`, `-z common-page-size=16384` for aarch64 Android and Linux
- [x] `host_page_size()` from `sysconf` (reported in the debug panel)
- [x] Deterministic min-heap scheduler with per-slot generations (cancel, reschedule, FIFO ties, rebase)
- [x] Save states: `KDST` header, per-component tagged sections, CRC32, system/version checks
- [x] Lock-free SPSC audio ring, dynamic-rate linear resampler, underrun handling
- [x] Tensor topology from cpufreq sysfs (G1–G4 vs G5 1+5+2), hwcaps (SVE2/I8MM/BF16/dotprod), thread pinning
- [x] `ThermalPolicy`: ADPF status → frame skip at CRITICAL and above
- [x] `GuestClock`: time-travel offsets, civil date conversions, BCD
- [x] Animal Crossing profile table and per-system setup

### Phase 2 — ARM cores and GBA (`cpu-arm`, `core-gba`): done
- [x] ARM7TDMI and ARM946E-S (ARMv5TE): 4096-entry ARM / 1024-entry Thumb decode LUTs, 2-stage prefetch, banked modes, exceptions
- [x] CP15 (control, TCM regions, protection, high vectors, WFI); HLE SWI hook with HaltAndRepeat (IntrWait)
- [x] In-crate assembler (labels, ARM + Thumb) used for the HLE BIOS and tests
- [x] HLE BIOS: Div/Sqrt/ArcTan/ArcTan2, CpuSet/CpuFastSet, affine sets, BitUnPack, LZ77/RLE/Huffman/Diff, MidiKey2Freq, CRC16, Halt/IntrWait/VBlankIntrWait, SoftReset
- [x] GBA PPU: modes 0–5, affine BGs, sprites (incl. affine), windows, alpha/brightness, mosaic, NEON RGB555 → RGBA8888
- [x] APU: four PSG channels, FIFO A/B on timers, mixing
- [x] DMA (immediate/HBlank/VBlank/FIFO), timers with cascade, keypad IRQ
- [x] Saves: SRAM, Flash 64K/128K (IDs, erase, bank switch), EEPROM 512B/8KB
- [x] Cartridge GPIO: RTC (from `GuestClock`), solar sensor; tilt sensor
- [x] SIO JoyBus mode (the GBA side of the link cable), multiboot boot from EWRAM
- [x] Passes every ROM in jsmolka's gba-tests (vendored, MIT; `tests/gba_tests.rs`): arm, thumb, memory, bios,
  nes, unsafe, save none/SRAM/Flash 64K/Flash 128K, plus the hello/shades/stripes PPU demos. Fixed along the way:
  - Thumb `STMIA` with an empty register list stored PC + 8 instead of PC + 6;
  - halfword/word stores to SRAM/Flash ignored the low address bits (the CPU now passes unaligned store addresses
    and each bus aligns, except the 8-bit save chip);
  - blank SRAM read 0x00 instead of 0xFF;
  - the BIOS open-bus value after an IRQ return (word at 0x144) was missing from the HLE BIOS.
- [x] Passes DenSinH's FuzzARM (50 000 random ARM/Thumb data-processing, multiply and load/store cases; GPL-3, so
  run locally, not vendored)
- [x] HLE BIOS: SoundBias sets the bias level, HardReset and CustomHalt are implemented

### Phase 3 — Nintendo DS (`core-nds`): done (Wi-Fi excluded)
- [x] ARM9 + ARM7 run in lockstep slices of 64 master cycles; direct boot from cartridge header, HLE BIOS for both CPUs
- [x] Shared bus: main RAM, WRAMCNT split, ARM7 WRAM, divider, sqrt, IPC sync/FIFO with IRQs
- [x] VRAM banks A–I with page-table mapping (LCDC, BG/OBJ, ext palettes, texture/palette slots, ARM7)
- [x] 2D engines A/B: text/affine/extended BGs, sprites, windows, blending, master brightness, 3D on BG0
- [x] 3D geometry: packed FIFO, matrix stacks, 4-light hardware lighting with shininess, texcoord transforms, clipping
- [x] 3D rendering: Z/W-buffered rasteriser, all 7 texture formats, modulate/decal/toon/highlight, alpha test, translucency, fog
- [x] SPI: synthesized firmware with user settings + CRC, touchscreen (TSC), power management
- [x] RTC with time travel; game card protocol (KEY2 `0xB7` reads); AUXSPI EEPROM/Flash saves
- [x] Animal Crossing: Wild World gets a 256 KB Flash save hint (`ADM*`)
- [x] Sound (`spu.rs`): 16 channels (PCM8/PCM16/IMA-ADPCM, PSG square on 8–13, noise on 14–15), both capture units, mixer bypass bits, master volume; clocked every 2048 master cycles (32 728 Hz)
- [x] Microphone through TSC channel 6, fed from the phone's microphone

### Phase 7 — Android (`android-jni`, `android/`): done
- [x] `libkd_emulator.so`: `Session` (system detection, per-core frame loop, audio resample to 48 kHz), runtime thread pinned to the fast cluster
- [x] AAudio low-latency callback stream; cartridges read from file descriptors
- [x] Button bits checked against the Kotlin source by a test

### Phase 8 — Delta-style app: done
- [x] Runtime (`kd-core`, `android-jni`):
  - fast-forward 2–8× with pitch-matched audio;
  - hold-to-rewind (delta-compressed history, 96 MiB budget);
  - per-frame cheats: GBA GameShark/AR v1–v2 (TEA), CodeBreaker, raw; DS Action Replay;
  - volume control;
  - `peek`/`poke` on every core.
- [x] Library metadata read with random access: DS banner icon and title, GBA header title
- [x] Frames handed to a Kotlin `GameView` (shared slot + direct-buffer copy, GPU-scaled per screen rect); replaces the `ANativeWindow` blit
- [x] Library:
  - one page per console with artwork tiles (DS icon, generated cartridge art, or a picked photo);
  - import of files and `.zip` archives (cartridges copied into app storage);
  - long-press menu: resume, start over, cheats, rename, artwork, import/export save, delete.
- [x] Game screen:
  - auto save with screenshot on exit and auto-resume;
  - pause menu: save/load state, cheats, fast forward, rewind, hold buttons, screenshot, time travel, DS mic, debug info;
  - save-state browser (Auto/Quick/General, thumbnails, rename, delete);
  - cheat editor validated by the native parser.
- [x] Controller skins:
  - built-in layouts for GBA and DS in portrait and landscape;
  - Delta `.deltaskin` import (`info.json` representations, items, `extendedEdges`, screens, PDF/PNG artwork);
  - multi-touch with sliding, 8-way d-pad, thumbsticks (imported skins) and the DS touchscreen;
  - opacity and haptics settings.
- [x] Settings: skins per console, opacity, haptics, smooth scaling, auto-resume, fast-forward speed, rewind interval, volume, DS mic
- [x] Island theme (original art: leaf launcher icon, palette); no Nintendo or Delta assets

## Animal Crossing fit, per system

| System | Profile turns on | Works today | Not yet |
|---|---|---|---|
| GBA | JoyBus link mode; multiboot images (`.mb`) boot via `Gba::new_multiboot` | JoyBus reset/status/read/write; multiboot boot | The GameCube host side was removed with the GameCube core |
| NDS (Wild World `ADM*`) | Stylus layout, time-travel RTC, 256 KB Flash save | Direct boot path, RTC with time travel, Flash save, touch, sound, microphone | Not verified with a retail dump in this repo |

The time-travel clock is per game and saved in app preferences. It shifts only the guest RTC.

## Known gaps

Each gap that guest code can reach is also logged at runtime with `tracing::warn!`.

**NDS**
- Sound plays back nearest-sample (as the hardware does); the capture "add to channel" bit is ignored.
- Wi-Fi is not emulated.
- Display capture, 3D edge marking and anti-aliasing are not emulated.
- ARM9 cycle timing is approximate.

**GBA**
- Prefetch-buffer timing is approximate.
- The BIOS's own sound driver (SWI 0x1A–0x1E, 0x20–0x24, 0x28–0x2A) is a no-op; commercial games link their own.

**App**
- Rewinding from the pause menu steps back about 3 seconds; there is no rewind timeline view yet.
- Imported `.deltaskin` files: thumbstick images are drawn as plain circles, and `ipad`-only skins use the iPad layout.

**Not verified on hardware:** no Tensor device in CI. The Android-only paths (AAudio, ADPF, haptics, the microphone) build and type-check, and the app UI runs in Robolectric tests against the host build of the native library, but nothing has run on a physical phone.

## Verification log

| Check | Command | Result |
|---|---|---|
| Host tests | `cargo test --workspace` | 157 passed |
| GBA test ROMs | `cargo test -p core-gba --test gba_tests` (jsmolka gba-tests) | all 13 ROMs pass |
| CPU fuzzing | FuzzARM ROMs, 1500 frames each (local) | 5/5 "End of testing" |
| App UI tests | `cd android && ./gradlew testDebugUnitTest` (Robolectric, host `libkd_emulator.so`) | 2 passed: import → library pages → play → touch A → pause menu → save state → cheats → settings → skins; landscape game screen; screenshots in `app/build/screenshots` |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Android type-check | `cargo check --target aarch64-linux-android -p android-jni` | ok |
| Native library | `cargo ndk -t arm64-v8a --platform 31 ... build --release -p android-jni` | ok |
| 16 KB alignment | `llvm-readelf -lW libkd_emulator.so` | all `LOAD` `Align 0x4000` |
| APK (debug + release/R8) | `cd android && ./gradlew assembleDebug assembleRelease` | ok |
| APK 16 KB zip alignment | `zipalign -c -P 16 4 app-*.apk` | ok; `.so` stored uncompressed |
| JNI after R8 | `dexdump` of release dex vs `android-jni` exports | 30/30 present, names and signatures match |
| Release signing | `KD_KEYSTORE=… ./gradlew assembleRelease`, `apksigner verify` | ok (v0.2.0, versionCode 2) |
