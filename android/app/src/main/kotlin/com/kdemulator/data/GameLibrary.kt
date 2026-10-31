package com.kdemulator.data

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.util.UUID
import java.util.zip.ZipInputStream

/** A game in the library. */
data class Game(
    val id: String,
    val title: String,
    val console: Console,
    val code: String,
    val fileName: String,
    /** The cartridge, copied into app storage. */
    val localPath: String,
    val animalCrossing: Boolean,
    val addedAt: Long,
    val lastPlayedAt: Long = 0,
) {
    fun toJson(): JSONObject = JSONObject()
        .put("id", id).put("title", title).put("console", console.id).put("code", code)
        .put("fileName", fileName).put("localPath", localPath)
        .put("ac", animalCrossing).put("addedAt", addedAt).put("lastPlayedAt", lastPlayedAt)

    companion object {
        fun fromJson(o: JSONObject): Game? {
            val console = Console.fromId(o.optInt("console")) ?: return null
            return Game(
                id = o.getString("id"),
                title = o.optString("title"),
                console = console,
                code = o.optString("code"),
                fileName = o.optString("fileName"),
                localPath = o.optString("localPath").takeIf { it.isNotEmpty() && it != "null" } ?: return null,
                animalCrossing = o.optBoolean("ac"),
                addedAt = o.optLong("addedAt"),
                lastPlayedAt = o.optLong("lastPlayedAt"),
            )
        }
    }
}

/** Result of importing one picked file. */
sealed interface ImportResult {
    data class Added(val game: Game) : ImportResult
    data class Duplicate(val game: Game) : ImportResult
    data class Failed(val name: String, val reason: String) : ImportResult
}

/**
 * The game library, Delta style: imported cartridges are copied into app
 * storage (zips are unpacked). Metadata and artwork come from the game
 * itself (the DS banner icon) unless the player picks their own.
 */
class GameLibrary(private val context: Context) {
    private val root = context.filesDir
    private val gamesDir = File(root, "games")
    private val artDir = File(root, "artwork")
    private val savesDir = File(root, "saves")
    private val indexFile = File(root, "library.json")
    private val prefs = context.getSharedPreferences("kd_games", Context.MODE_PRIVATE)

    private val _games = MutableStateFlow(load())
    val games: StateFlow<List<Game>> = _games.asStateFlow()

    private fun load(): List<Game> = runCatching {
        val arr = JSONArray(indexFile.readText())
        (0 until arr.length()).mapNotNull { Game.fromJson(arr.getJSONObject(it)) }
    }.getOrDefault(emptyList())

    private fun persist(list: List<Game>) {
        val arr = JSONArray()
        list.forEach { arr.put(it.toJson()) }
        val tmp = File(root, "library.json.tmp")
        tmp.writeText(arr.toString())
        tmp.renameTo(indexFile)
        _games.value = list
    }

    private fun update(id: String, f: (Game) -> Game) = persist(_games.value.map { if (it.id == id) f(it) else it })

    fun game(id: String): Game? = _games.value.firstOrNull { it.id == id }

    // ------------------------------------------------------------ import

    private fun displayName(uri: Uri): String =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "game"

    /** Import files picked by the player (runs on a background thread). */
    fun import(uris: List<Uri>): List<ImportResult> = uris.flatMap { importOne(it) }

    private fun importOne(uri: Uri): List<ImportResult> {
        val name = displayName(uri)
        val ext = name.substringAfterLast('.', "").lowercase()
        return try {
            when {
                ext == "zip" -> importZip(uri, name)
                ext in EXTENSIONS -> {
                    val tmp = File(context.cacheDir, "import-${UUID.randomUUID()}").apply { parentFile?.mkdirs() }
                    context.contentResolver.openInputStream(uri)?.use { input -> tmp.outputStream().use { input.copyTo(it) } }
                        ?: return listOf(ImportResult.Failed(name, "cannot open file"))
                    listOf(addLocal(tmp, name))
                }
                else -> listOf(ImportResult.Failed(name, "not a supported game file"))
            }
        } catch (e: Exception) {
            listOf(ImportResult.Failed(name, e.message ?: e.javaClass.simpleName))
        }
    }

    private fun importZip(uri: Uri, zipName: String): List<ImportResult> {
        val out = mutableListOf<ImportResult>()
        context.contentResolver.openInputStream(uri)?.use { raw ->
            ZipInputStream(raw.buffered()).use { zip ->
                while (true) {
                    val entry = zip.nextEntry ?: break
                    val name = entry.name.substringAfterLast('/')
                    if (entry.isDirectory || name.substringAfterLast('.', "").lowercase() !in EXTENSIONS) continue
                    val tmp = File(context.cacheDir, "import-${UUID.randomUUID()}")
                    tmp.outputStream().use { zip.copyTo(it) }
                    out += addLocal(tmp, name)
                }
            }
        }
        if (out.isEmpty()) out += ImportResult.Failed(zipName, "the zip has no supported games")
        return out
    }

    /** Add a file in cache storage: identify it, then move it into the library. */
    fun addLocal(tmp: File, name: String): ImportResult {
        val info = ParcelFileDescriptor.open(tmp, ParcelFileDescriptor.MODE_READ_ONLY).use { identify(it, name) }
        if (info == null) {
            tmp.delete()
            return ImportResult.Failed(name, "not recognised as a GBA or DS game")
        }
        duplicateOf(info, tmp.length())?.let {
            tmp.delete()
            return ImportResult.Duplicate(it)
        }
        val id = UUID.randomUUID().toString()
        val dest = File(gamesDir, "$id/$name").apply { parentFile?.mkdirs() }
        if (!tmp.renameTo(dest)) {
            tmp.copyTo(dest, overwrite = true)
            tmp.delete()
        }
        val game = info.toGame(id, name, localPath = dest.absolutePath)
        info.icon?.let { saveArtwork(id, it) }
        persist(_games.value + game)
        return ImportResult.Added(game)
    }

    private fun duplicateOf(info: Info, size: Long): Game? = _games.value.firstOrNull { g ->
        g.code == info.code && g.console == info.console && g.code.isNotBlank() &&
            File(g.localPath).length() == size
    }

    private data class Info(val console: Console, val code: String, val title: String, val ac: Boolean, val icon: Bitmap?) {
        fun toGame(id: String, fileName: String, localPath: String) = Game(
            id, title.ifBlank { fileName.substringBeforeLast('.') }, console, code, fileName,
            localPath, ac, System.currentTimeMillis(),
        )
    }

    private fun identify(pfd: ParcelFileDescriptor, name: String): Info? {
        val pixels = IntArray(96 * 32)
        val kv = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGameInfoFd(pfd.fd, name, pixels))
        val console = Console.fromId(kv["system"]?.toIntOrNull() ?: return null) ?: return null
        val w = kv["icon_w"]?.toIntOrNull() ?: 0
        val h = kv["icon_h"]?.toIntOrNull() ?: 0
        val icon = if (w > 0 && h > 0) Bitmap.createBitmap(pixels, w, h, Bitmap.Config.ARGB_8888) else null
        return Info(console, kv["code"].orEmpty().trimEnd('\u0000'), kv["title"].orEmpty(), kv["ac"] == "1", icon)
    }

    // ------------------------------------------------------------ editing

    fun rename(id: String, title: String) = update(id) { it.copy(title = title.trim().ifEmpty { it.title }) }

    fun markPlayed(id: String) = update(id) { it.copy(lastPlayedAt = System.currentTimeMillis()) }

    fun delete(id: String) {
        val g = game(id) ?: return
        File(gamesDir, id).deleteRecursively()
        artworkFile(id).delete()
        backupFile(g).delete()
        File(root, "states/$id").deleteRecursively()
        File(root, "cheats/$id.json").delete()
        persist(_games.value.filterNot { it.id == id })
    }

    fun artworkFile(id: String) = File(artDir, "$id.png")

    fun artwork(id: String): Bitmap? = artworkFile(id).takeIf { it.exists() }?.let { BitmapFactory.decodeFile(it.path) }

    private fun saveArtwork(id: String, bitmap: Bitmap) {
        artworkFile(id).apply { parentFile?.mkdirs() }.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
        artworkVersion.value++
    }

    /** Player-chosen artwork (any image the photo picker returns). */
    fun setArtwork(id: String, uri: Uri): Boolean {
        val bmp = context.contentResolver.openInputStream(uri)?.use(BitmapFactory::decodeStream) ?: return false
        val scale = 512f / maxOf(bmp.width, bmp.height)
        val sized = if (scale < 1f) Bitmap.createScaledBitmap(bmp, (bmp.width * scale).toInt(), (bmp.height * scale).toInt(), true) else bmp
        saveArtwork(id, sized)
        return true
    }

    /** Bumped whenever artwork changes so tiles reload it. */
    val artworkVersion = MutableStateFlow(0)

    // ------------------------------------------------------------ running

    /** Open a descriptor for the game file. */
    fun open(game: Game): ParcelFileDescriptor? =
        File(game.localPath).takeIf { it.exists() }?.let { ParcelFileDescriptor.open(it, ParcelFileDescriptor.MODE_READ_ONLY) }

    /** Boot the game in native code and restore its battery save and clock. */
    fun boot(game: Game): Boolean {
        val id = open(game)?.use { NativeEmulatorBridge.nativeLoadRomFd(it.fd, game.fileName) } ?: return false
        if (Console.fromId(id) == null) return false
        backupFile(game).takeIf { it.exists() }?.readBytes()?.let(NativeEmulatorBridge::nativeLoadBackup)
        NativeEmulatorBridge.nativeSetClockOffset(clockOffset(game))
        markPlayed(game.id)
        return true
    }

    fun backupFile(game: Game) = File(savesDir, "${game.id}.sav")

    /** Write the battery save if the game has one. */
    fun writeBackup(game: Game) {
        val data = NativeEmulatorBridge.nativeGetBackup()
        if (data.isNotEmpty()) backupFile(game).apply { parentFile?.mkdirs() }.writeBytes(data)
    }

    /** Replace the battery save with an imported file (game must not be running). */
    fun importBackup(game: Game, uri: Uri): Boolean {
        val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return false
        if (bytes.isEmpty()) return false
        backupFile(game).apply { parentFile?.mkdirs() }.writeBytes(bytes)
        return true
    }

    fun exportBackup(game: Game, uri: Uri): Boolean {
        val f = backupFile(game).takeIf { it.exists() } ?: return false
        context.contentResolver.openOutputStream(uri)?.use { out -> f.inputStream().use { it.copyTo(out) } } ?: return false
        return true
    }

    /** Per-game time-travel offset in seconds. */
    fun clockOffset(game: Game): Long = prefs.getLong("clock_${game.id}", 0)

    fun storeClockOffset(game: Game, seconds: Long) = prefs.edit().putLong("clock_${game.id}", seconds).apply()

    /** Copy an asset-free file into place (used by tests and the share intent). */
    fun importFile(file: File): ImportResult {
        val tmp = File(context.cacheDir, "import-${UUID.randomUUID()}")
        FileOutputStream(tmp).use { out -> file.inputStream().use { it.copyTo(out) } }
        return addLocal(tmp, file.name)
    }

    companion object {
        val EXTENSIONS = setOf("gba", "agb", "mb", "nds", "srl")
    }
}
