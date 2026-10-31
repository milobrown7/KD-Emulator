package com.kdemulator.data

import android.content.Context
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.UUID

/** Cheat code formats; ids match `kd_core::cheats::CheatFormat::id`. */
enum class CheatFormat(val id: String, val label: String, val example: String) {
    GAMESHARK("gameshark", "GameShark", "XXXXXXXX YYYYYYYY"),
    CODEBREAKER("codebreaker", "CodeBreaker", "XXXXXXXX YYYY"),
    RAW("raw", "Raw", "02000000:FF"),
    ACTION_REPLAY("actionreplay", "Action Replay", "XXXXXXXX YYYYYYYY"),
    GECKO("gecko", "Gecko", "XXXXXXXX YYYYYYYY");

    companion object {
        fun forConsole(c: Console): List<CheatFormat> = when (c) {
            Console.GBA -> listOf(GAMESHARK, CODEBREAKER, RAW)
            Console.NDS -> listOf(ACTION_REPLAY, RAW)
            Console.GAMECUBE, Console.WII -> listOf(GECKO)
        }

        fun fromId(id: String) = entries.firstOrNull { it.id == id }
    }
}

data class Cheat(val id: String, val name: String, val code: String, val format: CheatFormat, val enabled: Boolean)

/** Per-game cheats in `files/cheats/<gameId>.json`. */
class CheatStore(context: Context) {
    private val root = File(context.filesDir, "cheats")

    private fun file(gameId: String) = File(root, "$gameId.json")

    fun list(gameId: String): List<Cheat> = runCatching {
        val arr = JSONArray(file(gameId).readText())
        (0 until arr.length()).mapNotNull { i ->
            val o = arr.getJSONObject(i)
            val format = CheatFormat.fromId(o.getString("format")) ?: return@mapNotNull null
            Cheat(o.getString("id"), o.getString("name"), o.getString("code"), format, o.optBoolean("enabled", true))
        }
    }.getOrDefault(emptyList())

    private fun write(gameId: String, cheats: List<Cheat>) {
        root.mkdirs()
        val arr = JSONArray()
        cheats.forEach {
            arr.put(JSONObject().put("id", it.id).put("name", it.name).put("code", it.code).put("format", it.format.id).put("enabled", it.enabled))
        }
        file(gameId).writeText(arr.toString())
    }

    /** Validate with the native parser; returns an error message or null. */
    fun validate(format: CheatFormat, code: String): String? =
        NativeEmulatorBridge.nativeValidateCheat(format.id, code).ifEmpty { null }

    fun upsert(gameId: String, cheat: Cheat) {
        val list = list(gameId)
        write(gameId, if (list.any { it.id == cheat.id }) list.map { if (it.id == cheat.id) cheat else it } else list + cheat)
    }

    fun newId(): String = UUID.randomUUID().toString()

    fun delete(gameId: String, id: String) = write(gameId, list(gameId).filterNot { it.id == id })

    fun toggle(gameId: String, id: String) = write(gameId, list(gameId).map { if (it.id == id) it.copy(enabled = !it.enabled) else it })

    /** Push the enabled cheats to native code; returns parse errors. */
    fun activate(gameId: String): String {
        val spec = list(gameId).filter { it.enabled }.joinToString(NativeEmulatorBridge.CHEAT_RECORD_SEP.toString()) {
            it.format.id + NativeEmulatorBridge.CHEAT_FIELD_SEP + it.code
        }
        return NativeEmulatorBridge.nativeSetCheats(spec)
    }
}
