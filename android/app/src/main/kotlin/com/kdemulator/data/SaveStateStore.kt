package com.kdemulator.data

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import com.kdemulator.bridge.NativeEmulatorBridge
import org.json.JSONObject
import java.io.File
import java.util.UUID

/** Auto saves are written when leaving a game; quick saves by a skin button. */
enum class SaveStateKind { AUTO, QUICK, GENERAL }

data class SaveStateEntry(
    val id: String,
    val kind: SaveStateKind,
    val name: String?,
    val createdAt: Long,
    val stateFile: File,
    val thumbnailFile: File,
) {
    fun thumbnail(): Bitmap? = thumbnailFile.takeIf { it.exists() }?.let { BitmapFactory.decodeFile(it.path) }
}

/**
 * Save states per game: `files/states/<gameId>/<id>.state` with a PNG
 * screenshot and a small JSON sidecar. One auto save and one quick save
 * are kept; general saves are unlimited.
 */
class SaveStateStore(context: Context) {
    private val root = File(context.filesDir, "states")

    private fun dir(gameId: String) = File(root, gameId).apply { mkdirs() }

    fun list(gameId: String): List<SaveStateEntry> {
        val d = File(root, gameId)
        val metas = d.listFiles { f -> f.name.endsWith(".json") } ?: return emptyList()
        return metas.mapNotNull { meta ->
            runCatching {
                val o = JSONObject(meta.readText())
                val id = meta.name.removeSuffix(".json")
                SaveStateEntry(
                    id,
                    SaveStateKind.valueOf(o.getString("kind")),
                    o.optString("name").takeIf { it.isNotEmpty() },
                    o.getLong("createdAt"),
                    File(d, "$id.state"),
                    File(d, "$id.png"),
                )
            }.getOrNull()?.takeIf { it.stateFile.exists() }
        }.sortedByDescending { it.createdAt }
    }

    fun latest(gameId: String, kind: SaveStateKind): SaveStateEntry? = list(gameId).firstOrNull { it.kind == kind }

    /**
     * Snapshot the running game. [screenshot] is the current frame; [replace]
     * overwrites an existing entry (keeps its name).
     */
    fun save(gameId: String, kind: SaveStateKind, screenshot: Bitmap?, replace: SaveStateEntry? = null, name: String? = null): SaveStateEntry? {
        val data = NativeEmulatorBridge.nativeSaveState()
        if (data.isEmpty()) return null
        val singleton = if (kind != SaveStateKind.GENERAL) latest(gameId, kind) else null
        val id = replace?.id ?: singleton?.id ?: UUID.randomUUID().toString()
        val d = dir(gameId)
        File(d, "$id.state").writeBytes(data)
        screenshot?.let { bmp -> File(d, "$id.png").outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 90, it) } }
        val meta = JSONObject().put("kind", kind.name).put("createdAt", System.currentTimeMillis())
        (name ?: replace?.name)?.let { meta.put("name", it) }
        File(d, "$id.json").writeText(meta.toString())
        return list(gameId).firstOrNull { it.id == id }
    }

    fun load(entry: SaveStateEntry): Boolean =
        entry.stateFile.takeIf { it.exists() }?.readBytes()?.let(NativeEmulatorBridge::nativeLoadState) ?: false

    fun rename(gameId: String, entry: SaveStateEntry, name: String) {
        val f = File(dir(gameId), "${entry.id}.json")
        val o = runCatching { JSONObject(f.readText()) }.getOrDefault(JSONObject())
        f.writeText(o.put("name", name.trim()).toString())
    }

    fun delete(gameId: String, entry: SaveStateEntry) {
        val d = dir(gameId)
        listOf("state", "png", "json").forEach { File(d, "${entry.id}.$it").delete() }
    }
}
