## KD-Emulator 0.2.0

An Android emulator for Game Boy Advance, Nintendo DS, GameCube and Wii, with a Delta-style app and an
Animal Crossing profile on every console.

**Install:** download `KD-Emulator-v0.2.0.apk` below and open it on your phone. You'll need to allow
installs from your browser or file manager. The app is for 64-bit ARM phones running Android 12 or newer.
It's tuned for Google Tensor (Pixel 6 and later), and its native code is aligned for 16 KB pages.

### What's new
- **Delta-style library:** one page per console, artwork tiles and a long-press menu. Artwork is the DS icon or
  GameCube banner from the game itself, or a picture you choose. The long-press menu has resume, start over,
  rename, change artwork, cheats, import/export save file and delete.
- **Importing:** pick game files or `.zip` archives. Cartridges are copied into the app. Large GameCube/Wii
  discs stay where they are.
- **Controller skins:** built-in skins for every console in portrait and landscape. All fingers are tracked
  at once, you can slide between buttons, and the d-pad does diagonals. The DS bottom screen takes touch,
  and the Wii pointer follows your finger. You can import `.deltaskin` files for GBA and DS, and set opacity and
  haptics.
- **Pause menu:** save and load state (with screenshots), cheat codes, fast forward (2–8×), rewind, hold
  buttons, screenshot, time travel, DS microphone and debug info.
- **Auto save:** leaving a game saves its state, and opening it again resumes where you were. Battery saves
  are written automatically.
- **Cheats:**
  - GBA: GameShark/Action Replay v1–v2, CodeBreaker and raw codes.
  - DS: Action Replay.
  - GameCube and Wii: Gecko.
- **DS sound:** the full 16-channel sound chip is now emulated, including the capture units, so DS games
  have audio. The phone's microphone can be used for blowing and voice.
- **Animal Crossing:** Wild World and City Folk are recognised with a 🍃 badge. Time travel moves only the
  game's clock.

### Known limits
- GBA and DS are the most complete.
- **GameCube and Wii games don't reach gameplay yet**, Animal Crossing included. The audio chip's ucode
  handshake, memory card unlock and hardware lighting are still missing.
- Wii retail discs need your own console's common key.
- This release hasn't been tested on a physical phone yet. The app was exercised with JVM UI tests and
  the emulator cores with 240+ host tests.
- No BIOS or firmware files are needed for GBA, DS or GameCube. Use dumps of games you own.

KD-Emulator isn't affiliated with Nintendo or with Delta.
