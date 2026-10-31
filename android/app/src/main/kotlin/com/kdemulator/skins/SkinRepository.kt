package com.kdemulator.skins

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.pdf.PdfRenderer
import android.net.Uri
import android.os.ParcelFileDescriptor
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import java.io.File
import java.util.UUID
import java.util.zip.ZipInputStream

/**
 * Controller skins: the built-in Island skin for every console plus skins
 * the player imports. `.deltaskin` files (Delta's format: a zip with
 * `info.json` and PDF/PNG artwork) work for GBA and DS.
 */
class SkinRepository(private val context: Context) {
    private val root = File(context.filesDir, "skins")

    fun skins(console: Console): List<ControllerSkin> = listOf(BuiltInSkins.skin(console)) + imported().filter { it.console == console }

    fun find(console: Console, id: String?): ControllerSkin =
        id?.let { want -> skins(console).firstOrNull { it.id == want } } ?: BuiltInSkins.skin(console)

    private fun imported(): List<ControllerSkin> =
        root.listFiles()?.filter { it.isDirectory }?.mapNotNull { dir ->
            runCatching { ControllerSkin.parse(File(dir, "info.json").readText(), dir)?.copy(id = dir.name) }.getOrNull()
        }.orEmpty()

    /** Unpack and validate a skin file. Returns the skin or an error message. */
    fun import(uri: Uri): Result<ControllerSkin> = runCatching {
        val dir = File(root, UUID.randomUUID().toString())
        dir.mkdirs()
        try {
            context.contentResolver.openInputStream(uri)?.use { raw ->
                ZipInputStream(raw.buffered()).use { zip ->
                    while (true) {
                        val e = zip.nextEntry ?: break
                        if (e.isDirectory) continue
                        // Flatten paths and refuse anything that would escape the folder.
                        val name = e.name.substringAfterLast('/')
                        if (name.isEmpty() || name.startsWith(".")) continue
                        val out = File(dir, name)
                        require(out.canonicalPath.startsWith(dir.canonicalPath)) { "bad entry ${e.name}" }
                        out.outputStream().use { zip.copyTo(it) }
                    }
                }
            } ?: error("cannot open the file")
            val info = File(dir, "info.json").takeIf { it.exists() } ?: error("not a skin: info.json is missing")
            val skin = ControllerSkin.parse(info.readText(), dir) ?: error("this skin is for a console KD-Emulator doesn't support")
            skin.copy(id = dir.name)
        } catch (e: Throwable) {
            dir.deleteRecursively()
            throw e
        }
    }

    fun delete(skin: ControllerSkin) {
        if (!skin.builtIn) File(root, skin.id).deleteRecursively()
    }

    companion object {
        /** Render skin artwork (PDF page 0 or a bitmap) at [width]x[height] pixels. */
        fun renderAsset(file: File, width: Int, height: Int): Bitmap? = runCatching {
            if (width <= 0 || height <= 0 || !file.exists()) return null
            if (file.extension.equals("pdf", ignoreCase = true)) {
                ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use { fd ->
                    PdfRenderer(fd).use { pdf ->
                        pdf.openPage(0).use { page ->
                            val bmp = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                            bmp.eraseColor(Color.TRANSPARENT)
                            page.render(bmp, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                            bmp
                        }
                    }
                }
            } else {
                BitmapFactory.decodeFile(file.path)?.let { Bitmap.createScaledBitmap(it, width, height, true) }
            }
        }.getOrNull()
    }
}
