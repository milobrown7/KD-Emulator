## KD-Emulator 0.3.0

A Game Boy Advance and Nintendo DS emulator for Android, with a Delta-style app.

**Install:** download `KD-Emulator-v0.3.0.apk` below and open it on your phone (allow installs from your browser or
file manager). The app needs a 64-bit ARM phone on Android 12 or newer. If you installed 0.2.0, uninstall it first:
each release is signed with a fresh key until a permanent signing key is set up.

### Changes
- **GameCube and Wii removed.** Neither could run commercial games, so the app is now GBA and DS only.
- **GBA accuracy:** the emulator now passes every ROM in jsmolka's public GBA test suite (ARM, Thumb, memory,
  BIOS, save types, PPU demos) and 50,000 randomised CPU tests from FuzzARM. Fixes:
  - Thumb store-multiple with an empty register list stored the wrong PC value.
  - SRAM/Flash saves written with halfword or word stores went to the wrong byte. Games that save this way now
    keep their saves correctly.
  - Brand-new SRAM saves now start blank (0xFF) like a real cartridge, so games detect a fresh save properly.
  - The BIOS value games can read after an interrupt now matches real hardware.
  - The BIOS calls SoundBias, HardReset and CustomHalt now work.
- When the phone gets critically hot, the screen updates less often (every 2nd or 3rd frame) to let it cool.

### Still true from 0.2.0
- Delta-style library, controller skins (including `.deltaskin` import), save states with screenshots, rewind,
  fast forward, cheats (GameShark, CodeBreaker, Action Replay), auto-resume, DS sound and microphone.
- Not yet tested on a physical phone: the app is exercised by JVM UI tests and the emulator by 157 host tests
  plus the public GBA test ROMs.
- No BIOS or firmware files are needed. Use dumps of games you own.

KD-Emulator isn't affiliated with Nintendo or with Delta.
