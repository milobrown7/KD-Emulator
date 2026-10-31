# KD-Emulator — status

What is implemented, how each part is verified, and what is still missing.
Update this file in the same commit as the code it describes.

**Last verified:** 214 host tests passing (`cargo test --workspace`). `cargo clippy --workspace --all-targets -- -D warnings`
is clean. `cargo check --target aarch64-linux-android -p android-jni` passes. `libkd_emulator.so` was built with
NDK r28 / cargo-ndk 4.1.2, and every `LOAD` segment is `Align 0x4000`, with 20 JNI exports.
`./gradlew assembleDebug` and `assembleRelease` (R8) both succeed (AGP 8.7.3, Kotlin 2.1.0, compileSdk 35). In
both APKs the stripped 1.1 MB `libkd_emulator.so` is stored uncompressed and passes `zipalign -c -P 16`. All 20
`native*` methods keep their names after R8, and their dex signatures match the Rust exports.

> **Honest summary.** The four cores share one runtime and each passes
> hardware-level tests on synthetic programs, written into the tests, that run
> on the emulated CPUs. GBA and DS are the most complete. GameCube and Wii boot
> DOLs and discs through to GX rendering, but they **cannot run commercial
> games such as Animal Crossing / City Folk yet** (see the gaps list).

## Phases

### Phase 1 — workspace, 16 KB pages, runtime (`kd-core`): done
- [x] Linker flags `-z max-page-size=16384`, `-z common-page-size=16384` for aarch64 Android and Linux
- [x] `host_page_size()` from `sysconf`; `PageAwareArena` (memfd, page-rounded, mirror views, `discard`)
- [x] `JitArena`: memfd mapped twice (RW and RX, W^X); `flush_icache` (`dc cvau`/`ic ivau`/`dsb ish`/`isb`); runs code on aarch64 hosts
- [x] Deterministic min-heap scheduler with per-slot generations (cancel, reschedule, FIFO ties, rebase)
- [x] Save states: `KDST` header, per-component tagged sections, CRC32, system/version checks
- [x] Lock-free SPSC audio ring, dynamic-rate linear resampler, underrun handling
- [x] Tensor topology from cpufreq sysfs (G1–G4 vs G5 1+5+2), hwcaps (SVE2/I8MM/BF16/dotprod), thread pinning
- [x] `ThermalPolicy`: ADPF status → internal scale / async uber-shaders / DSP HLE
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
- [x] SIO JoyBus mode (GameCube link), multiboot boot from EWRAM

### Phase 3 — Nintendo DS (`core-nds`): done (sound and Wi-Fi excluded)
- [x] ARM9 + ARM7 run in lockstep slices of 64 master cycles; direct boot from cartridge header, HLE BIOS for both CPUs
- [x] Shared bus: main RAM, WRAMCNT split, ARM7 WRAM, divider, sqrt, IPC sync/FIFO with IRQs
- [x] VRAM banks A–I with page-table mapping (LCDC, BG/OBJ, ext palettes, texture/palette slots, ARM7)
- [x] 2D engines A/B: text/affine/extended BGs, sprites, windows, blending, master brightness, 3D on BG0
- [x] 3D geometry: packed FIFO, matrix stacks, 4-light hardware lighting with shininess, texcoord transforms, clipping
- [x] 3D rendering: Z/W-buffered rasteriser, all 7 texture formats, modulate/decal/toon/highlight, alpha test, translucency, fog
- [x] SPI: synthesized firmware with user settings + CRC, touchscreen (TSC), power management
- [x] RTC with time travel; game card protocol (KEY2 `0xB7` reads); AUXSPI EEPROM/Flash saves
- [x] Animal Crossing: Wild World gets a 256 KB Flash save hint (`ADM*`)

### Phase 4 — PowerPC (`cpu-ppc`): done (JIT covers an integer subset)
- [x] Gekko/Broadway interpreter: integer, FP single/double, CR, SPRs, exceptions, decrementer, `sc`/`rfi`, `dcbz`
- [x] Paired singles: GQR decoding, quantized loads/stores (`psq_l`/`psq_st`), saturation
- [x] MMU: BATs (OS standard set), hashed page-table walk, two-level 4 KB SoftTLB, DSI on unmapped access
- [x] AArch64 encoder + block JIT for integer ALU/branch subset; checked against the interpreter by an AArch64 interpreter in the tests

### Phase 5 — GX and DSP (`gpu-gx-vk`, `dsp-gc`): done (software raster path)
- [x] Command FIFO parser across partial pushes; BP/CP/XF loads; primitive triangulation
- [x] Vertex loader: VCD/VAT, direct/indexed attributes, fixed-point fractions, matrix indices, colour formats
- [x] XF: position matrices, perspective/orthographic projection, viewport
- [x] Texture decoders: I4/I8/IA4/IA8/RGB565/RGB5A3/RGBA8/C4/C8/C14X2/CMPR
- [x] TEV: stage/order decoding, CPU evaluation, alpha-test analysis
- [x] Shaders: GLSL generation → SPIR-V via naga; uber-shader variants; `OpKill` only when alpha test can fail; vendor compute local sizes (32 PowerVR / 64 Mali)
- [x] Software rasteriser: depth, blending, scissor, culling; EFB → XFB (YUYV) copies
- [x] `RendererProfile` (Mali / PowerVR / Generic); `KDPC` pipeline-cache container; Vulkan instance/device bring-up (skipped when no ICD is present)
- [x] DSP: DSPCR, mailboxes (IPL boot mail), ARAM DMA, AI DMA; GC ADPCM; AX-style HLE voice mixer
- [x] DSP LLE core: loads/stores, addressing, loops, branches, basic accumulator maths

### Phase 6 — GameCube and Wii (`core-gcn`, `core-wii`): done (partial hardware)
- [x] Flipper bus: MEM1 in `PageAwareArena`, MMIO CP/PE/VI/PI/MI/DSP/DI/SI/EXI/AI, gather pipe → GX, PI interrupts
- [x] VI field timing and XFB presentation; PE tokens/finish
- [x] Discs: ISO/GCM (in memory or streamed via `pread`), header parsing, DOL load/build
- [x] HLE apploader: runs the disc's own apploader on the PPC, then jumps to the DOL
- [x] EXI: IPL RTC (from `GuestClock`) and SRAM checksum; Memory Card 59/251/1019 (ID, read, erase, program)
- [x] SI: controller polling/status, GBA link device on port 2 (JoyBus into `core-gba`)
- [x] Wii: Hollywood IPC and IRQ registers, MEM2, legacy block at `0x0D006000`
- [x] IOS HLE: `/dev/fs` (NAND in memory), `/dev/es`, `/dev/di`, `/dev/sdio/slot0`, `/dev/stm`, BT node
- [x] WBFS read (sparse sectors), WAD parsing; Wii Remote HID reports (core buttons, IR dots, Nunchuk, Classic)

### Phase 7 — Android (`android-jni`, `android/`): done
- [x] `libkd_emulator.so`: `Session` (system detection, per-core frame loop, audio resample to 48 kHz), runtime thread pinned to the fast cluster
- [x] ANativeWindow CPU blit; AAudio low-latency callback stream; SAF file descriptors (whole read or streamed GameCube discs)
- [x] JNI exports for `com.kdemulator.bridge.NativeEmulatorBridge` (20 functions); button bits checked against the Kotlin source by a test
- [x] Compose app:
  - `RomLibraryScreen`: SAF folder/file pickers, Animal Crossing cards
  - `EmulationSurfaceScreen`: SurfaceView, menu, save states, time-travel dialog
  - `TouchControllerOverlay`: classic, stylus, dual-stick and pointer layouts
  - `MobileDebuggerSheet`
  - `HardwareGamepadManager`
  - `SensorMotionManager`: tilt, gyro pointer, light sensor
  - `TensorThermalHintManager`: ADPF thermal listener and performance hint session
- [x] Island theme (original art: leaf launcher icon, palette); no Nintendo assets

## Animal Crossing fit, per system

| System | Profile turns on | Works today | Not yet |
|---|---|---|---|
| GBA | JoyBus link companion; multiboot images from the GameCube game boot via `Gba::new_multiboot` | JoyBus reset/status/read/write; multiboot boot; GBA wired to GameCube port 2 | Animal Crossing's island transfer uses the GameCube game, so it inherits the GameCube gaps |
| NDS (Wild World `ADM*`) | Stylus layout, time-travel RTC, 256 KB Flash save | Direct boot path, RTC with time travel, Flash save, touch | Sound is silent; not verified with a retail dump in this repo |
| GameCube (`GAF*`/`GAE*`) | Memory Card 251 in slot A, GBA on port 2, time-travel RTC | Disc header detection, HLE apploader, Card 251, IPL RTC time travel | Retail game can't reach gameplay (see gaps) |
| Wii (City Folk `RUU*`) | Pointer layout, SD card, time-travel | WBFS detection, IOS SD/NAND nodes, pointer reports | Retail discs need the user's common key; BT stack is HLE |

The time-travel clock is per game and saved in app preferences. It shifts only the guest RTC.

## Known gaps

Each gap that guest code can reach is also logged at runtime with `tracing::warn!`.

**GameCube / Wii**
- AX HLE is not wired to the DSP mailbox: games that upload the AX ucode wait on DSP mail.
- Memory card unlock (DSP ucode) isn't emulated.
- DSP LLE is a subset: no multiply/extended ops. It also needs the user's own IROM/DROM.
- GX is software-rendered. The Vulkan path covers instance/device, the vendor profile, the pipeline cache and EFB setup. Draw submission and presentation use the CPU blit.
- XF lighting isn't computed: channels pass the material/vertex colour. Texgen is position/texcoord passthrough.
- The JIT covers integer ALU/branch only and runs only on aarch64. The rest falls back to the interpreter.
- Wii: no common-key decryption (it must come from the user), no RVZ, no WAD install, no Bluetooth HCI stack (Wii Remote reports are synthesized).

**NDS**
- Sound is not emulated (silence).
- Wi-Fi is not emulated.
- Display capture, 3D edge marking and anti-aliasing are not emulated.
- ARM9 cycle timing is approximate.

**GBA**
- Prefetch-buffer timing is approximate.

**Not verified on hardware:** no Tensor device in CI. The Android-only paths (AAudio, ANativeWindow, ADPF, the JIT on device) build and type-check but have only been run as host equivalents.

## Verification log

| Check | Command | Result |
|---|---|---|
| Host tests | `cargo test --workspace` | 214 passed |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Android type-check | `cargo check --target aarch64-linux-android -p android-jni` | ok |
| Native library | `cargo ndk -t arm64-v8a --platform 31 ... build --release -p android-jni` | ok |
| 16 KB alignment | `llvm-readelf -lW libkd_emulator.so` | all `LOAD` `Align 0x4000` |
| APK (debug + release/R8) | `cd android && ./gradlew assembleDebug assembleRelease` | ok |
| APK 16 KB zip alignment | `zipalign -c -P 16 4 app-*.apk` | ok; `.so` stored uncompressed |
| JNI after R8 | `dexdump` of release dex vs `android-jni` exports | 20/20 present, signatures match |
