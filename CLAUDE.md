# KD-Emulator — working notes for Claude

KD-Emulator is an Android emulator for Game Boy Advance and Nintendo DS,
written as a Rust workspace with a Kotlin / Jetpack Compose front end modelled
on Delta. It targets Google Tensor phones (G1–G4, and G5 with 1+5+2 ARMv9.2
clusters). Both systems have an **Animal Crossing profile** (see below).
GameCube and Wii support was removed in 0.3.0.

`STATUS.md` is the live implementation and test checklist. Update it in the same
commit as any change that adds, removes or fixes a feature.

## Ground rules

- No stubs: no `todo!()`, `unimplemented!()` or panicking placeholders on
  guest-reachable paths. When guest code hits something we don't emulate, log
  it with `tracing::warn!` (no-op or best-effort behaviour) and add it to the
  gaps list in `STATUS.md`.
- No Nintendo BIOS, firmware, keys or game data in the repo. The GBA and NDS
  BIOS are HLE (high-level emulation), and the NDS firmware is synthesized.
- Everything must pass `cargo test --workspace` on the host (x86_64 Linux),
  with no device attached. Code that only runs on Android, such as AAudio,
  sits behind `cfg(target_os = "android")` and has a host-testable
  equivalent.
- Keep `cargo clippy --workspace --all-targets -- -D warnings` clean.
  Formatting: `cargo fmt` (`rustfmt.toml`: max_width 120).
- MSRV is Rust 1.82 (`rust-version` in the workspace `Cargo.toml`).

## Build commands

```sh
# Host: tests and lints
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# Type-check the native library for Android
rustup target add aarch64-linux-android
cargo check --target aarch64-linux-android -p android-jni

# Native library only (needs NDK r28 and cargo-ndk >= 4)
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.0.13004108
cargo ndk -t arm64-v8a --platform 31 -o android/app/src/main/jniLibs \
    build --release -p android-jni

# APK (Gradle's preBuild runs the cargo-ndk step above automatically)
cd android && ./gradlew assembleDebug

# App UI tests: Robolectric on the JVM with a host build of libkd_emulator.so
# (screenshots land in android/app/build/screenshots). If Maven Central
# rate-limits the android-all download, set ROBOLECTRIC_REPO to a mirror.
cd android && ./gradlew testDebugUnitTest

# Signed release APK (unsigned when KD_KEYSTORE is not set)
KD_KEYSTORE=release.jks KD_KEYSTORE_PASSWORD=… KD_KEY_ALIAS=… KD_KEY_PASSWORD=… \
    ./gradlew assembleRelease
```

Releases: `.github/workflows/release.yml` runs on a `v*` tag push, or on a push that changes `RELEASE_NOTES.md`,
the app version or the workflow, and only when release `v<versionName>` does not exist yet. It runs the host tests, builds and signs
the APK, and publishes it with `RELEASE_NOTES.md` as the release text. Keep `versionCode`/`versionName` in
`android/app/build.gradle.kts` in step with the tag.

`android/local.properties` (gitignored) needs `sdk.dir=...` or `ANDROID_HOME`.
The app is arm64-v8a only, minSdk 31, targetSdk 35.

## Crate map

| Crate | Role |
|---|---|
| `kd-core` | Shared runtime: `EmulatorCore` trait, deterministic scheduler, sectioned save states (`KDST` + CRC32), lock-free SPSC audio ring and resampler, host page size, Tensor topology, hwcaps, `ThermalPolicy`, `GuestClock` (time travel), cheats, rewind, Animal Crossing profile table, input bits |
| `cpu-arm` | ARM7TDMI / ARM946E-S interpreter (4096-entry ARM and 1024-entry Thumb decode tables), CP15 + TCM, assembler, shared HLE BIOS routines |
| `core-gba` | GBA: bus, PPU, APU, DMA, timers, cartridge saves, GPIO RTC/solar sensor, tilt, JoyBus link mode, multiboot |
| `core-nds` | NDS: ARM9 + ARM7 in lockstep, VRAM banking, 2D engines, 3D geometry and rasterizer, IPC, SPI (firmware/touch/power), RTC, game card, saves |
| `android-jni` | `libkd_emulator.so`: session/runtime, JNI exports (`com.kdemulator.bridge.NativeEmulatorBridge`), frame slot, AAudio, emulation thread |

Android app: `android/app/src/main/kotlin/com/kdemulator/` (package
`com.kdemulator`).

## 16 KB page-size rules (Android 15+ / Pixel)

- `.cargo/config.toml` links every aarch64 target with
  `-z max-page-size=16384` and `-z common-page-size=16384`. Check with
  `llvm-readelf -lW libkd_emulator.so`: every `LOAD` must show `Align 0x4000`.
- Never hard-code 4096. Use `kd_core::arena::host_page_size()` (from
  `sysconf(_SC_PAGESIZE)`) if page size ever matters.
- The APK keeps `.so` files uncompressed (`useLegacyPackaging = false`,
  `extractNativeLibs=false`), so they must be 16 KB aligned inside the zip.

## Tensor rules

- Thermal: ADPF thermal status reaches `ThermalPolicy::on_status_changed`
  through `nativeSetThermalStatus`. GBA/DS emulation is light, so the only
  mitigation is presenting every 2nd frame at CRITICAL and every 3rd at
  EMERGENCY and above.
- The `PerformanceHintManager` session is fed the real frame time each frame.
- CPU topology: read `/sys/devices/system/cpu/cpu*/cpufreq` to find clusters.
  Pin the emulation thread to the fastest cluster with `sched_setaffinity`.
  On G5, ARMv9.2 features come from hwcaps (SVE2, BF16, I8MM), never from
  the SoC name.

## Memory maps (as implemented)

**GBA:**

| Address | Region |
|---|---|
| `0x00000000` | BIOS (16 KB, HLE stub; the IRQ dispatcher is at `0x128`) |
| `0x02000000` | EWRAM 256 KB |
| `0x03000000` | IWRAM 32 KB |
| `0x04000000` | I/O |
| `0x05000000` | Palette |
| `0x06000000` | VRAM |
| `0x07000000` | OAM |
| `0x08000000` / `0x0A000000` / `0x0C000000` | ROM (wait states 0/1/2) |
| `0x0E000000` | SRAM / Flash. EEPROM sits at the top of ROM space. |

GPIO (RTC/solar sensor) is at `0x080000C4`–`0x080000C9`.

**NDS:**

ARM9:

| Address | Region |
|---|---|
| `0x00000000` | ITCM 32 KB (base/size and enable from CP15) |
| CP15-set (default `0x027C0000`) | DTCM (16 KB) |
| `0x02000000` | Main RAM 4 MB |
| `0x03000000` | Shared WRAM |
| `0x04000000` | I/O |
| `0x05000000` | Palette |
| `0x06000000` | VRAM (through bank page tables) |
| `0x07000000` | OAM |
| `0xFFFF0000` | BIOS (HLE) |

ARM7:

| Address | Region |
|---|---|
| `0x00000000` | BIOS (HLE) |
| `0x02000000` | Main RAM |
| `0x03000000` | Shared WRAM |
| `0x03800000` | ARM7 WRAM 64 KB |
| `0x04000000` | I/O (Wi-Fi at `0x04800000` is not emulated) |
| `0x06000000` | VRAM C/D as ARM7 WRAM |

Games boot directly: the header is copied to `0x027FFE00` and the secure area
is skipped.

## Animal Crossing profiles

`kd_core::animal_crossing` maps game codes to a per-system setup. The UI gets
it through `nativeGetAnimalCrossingInfo`.

| System | Codes | Setup |
|---|---|---|
| GBA | (no standalone title) | JoyBus link mode. Multiboot images (`.mb`) boot through `Gba::new_multiboot`. |
| NDS | ADMJ/ADME/ADMP/ADMK | Stylus layout, RTC time travel, 256 KB FLASH save |

Time travel only shifts the guest clock (`GuestClock` offset, saved per game).
It never changes the host clock.

## Conventions

- Save states: one section per component, using the
  `kd_core::savestate::{StateWriter, StateReader}` tags. Bump a section's
  version when you change its layout.
- Scheduler events are keyed by slot. Cancelling bumps the slot's generation;
  never scan the heap.
- Audio: the emulation thread produces samples and the AAudio callback
  consumes them through the SPSC ring. Never lock on the audio thread.
- JNI: export names must match `NativeEmulatorBridge.kt`. The
  `BUTTON_BITS` test in `android-jni` checks that the button bits match the
  Kotlin constants.
- Frames reach the UI through `FrameSlot` (`nativeCopyFrame` into a direct
  buffer). `GameView` draws each skin screen rect, so never present from
  native code.
- Controller skins use Delta's `info.json` model (`skins/SkinModel.kt`).
  Built-in skins are generated per screen size in `BuiltInSkins.kt`; input
  names follow Delta's (`a`, `b`, `up`, `menu`, `toggleFastForward`, …), plus
  `rewind`.
