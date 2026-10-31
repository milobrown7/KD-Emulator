package com.kdemulator.data

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.documentfile.provider.DocumentFile
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import java.io.File

data class RomEntry(
    val uri: Uri,
    val fileName: String,
    val console: Console,
    val gameCode: String,
    val title: String,
    /** Recognised as an Animal Crossing release by the native profile table. */
    val animalCrossing: Boolean,
)

/**
 * Storage Access Framework access: the ROM folder chosen by the user,
 * header peeks for the library, battery saves and per-game settings such as
 * the Animal Crossing clock offset.
 */
class RomRepository(private val context: Context) {
    private val prefs = context.getSharedPreferences("kd_emulator", Context.MODE_PRIVATE)
    private val extensions = setOf("gba", "agb", "mb", "nds", "srl", "iso", "gcm", "dol", "wbfs")

    var romFolder: Uri?
        get() = prefs.getString("rom_folder", null)?.let(Uri::parse)
        set(value) = prefs.edit().putString("rom_folder", value?.toString()).apply()

    fun persistFolder(uri: Uri) {
        context.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        romFolder = uri
    }

    /** Scan the chosen folder (one level deep plus subfolders). */
    fun scan(): List<RomEntry> {
        val root = romFolder?.let { DocumentFile.fromTreeUri(context, it) } ?: return emptyList()
        val out = mutableListOf<RomEntry>()
        fun walk(dir: DocumentFile, depth: Int) {
            for (f in dir.listFiles()) {
                if (f.isDirectory && depth < 2) walk(f, depth + 1)
                val name = f.name ?: continue
                if (!f.isFile || name.substringAfterLast('.').lowercase() !in extensions) continue
                peek(f.uri, name)?.let(out::add)
            }
        }
        walk(root, 0)
        // Animal Crossing first, then alphabetical.
        return out.sortedWith(compareByDescending<RomEntry> { it.animalCrossing }.thenBy { it.title.lowercase() })
    }

    fun peek(uri: Uri, name: String): RomEntry? {
        val info = context.contentResolver.openFileDescriptor(uri, "r")?.use { pfd ->
            NativeEmulatorBridge.nativePeekTitleFd(pfd.fd, name)
        } ?: return null
        val parts = info.split('|', limit = 3)
        if (parts.size < 3) return null
        val console = Console.fromId(parts[0].toIntOrNull() ?: return null) ?: return null
        val code = parts[1].trimEnd('\u0000')
        val acCodes = setOf("GAFJ", "GAEJ", "GAFE", "GAFP", "GAFU", "ADMJ", "ADME", "ADMP", "ADMK", "RUUJ", "RUUE", "RUUP", "RUUK")
        return RomEntry(uri, name, console, code, parts[2].ifBlank { name }, code.take(4) in acCodes)
    }

    /** Open a game; returns the console or null. Battery save is restored. */
    fun load(entry: RomEntry): Console? {
        val id = context.contentResolver.openFileDescriptor(entry.uri, "r")?.use { pfd ->
            NativeEmulatorBridge.nativeLoadRomFd(pfd.fd, entry.fileName)
        } ?: return null
        val console = Console.fromId(id) ?: return null
        saveFile(entry).takeIf { it.exists() }?.readBytes()?.let(NativeEmulatorBridge::nativeLoadBackup)
        NativeEmulatorBridge.nativeSetClockOffset(clockOffset(entry))
        return console
    }

    private fun saveFile(entry: RomEntry) = File(context.filesDir, "saves/${entry.gameCode.ifBlank { entry.fileName }}.sav")
    private fun stateFile(entry: RomEntry, slot: Int) = File(context.filesDir, "states/${entry.gameCode.ifBlank { entry.fileName }}.$slot.kdst")

    fun writeBackup(entry: RomEntry) {
        val data = NativeEmulatorBridge.nativeGetBackup()
        if (data.isNotEmpty()) saveFile(entry).apply { parentFile?.mkdirs() }.writeBytes(data)
    }

    fun saveState(entry: RomEntry, slot: Int): Boolean {
        val data = NativeEmulatorBridge.nativeSaveState()
        if (data.isEmpty()) return false
        stateFile(entry, slot).apply { parentFile?.mkdirs() }.writeBytes(data)
        return true
    }

    fun loadState(entry: RomEntry, slot: Int): Boolean =
        stateFile(entry, slot).takeIf { it.exists() }?.readBytes()?.let(NativeEmulatorBridge::nativeLoadState) ?: false

    /** Per-game time-travel offset in seconds. */
    fun clockOffset(entry: RomEntry): Long = prefs.getLong("clock_${entry.gameCode}", 0L)

    fun storeClockOffset(entry: RomEntry, seconds: Long) = prefs.edit().putLong("clock_${entry.gameCode}", seconds).apply()
}
