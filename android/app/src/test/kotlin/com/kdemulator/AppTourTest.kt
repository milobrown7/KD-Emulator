package com.kdemulator

import android.content.Context
import android.graphics.Bitmap
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeLeft
import androidx.compose.ui.test.swipeRight
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.bridge.NativeEmulatorBridge.Buttons
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.data.AppServices
import com.kdemulator.data.ImportResult
import com.kdemulator.data.SaveStateKind
import com.kdemulator.input.InputHub
import com.kdemulator.skins.BuiltInSkins
import com.kdemulator.skins.SkinInputs
import com.kdemulator.ui.theme.IslandTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.io.File
import java.time.Duration

/**
 * Drives the real app (Compose UI + the host build of libkd_emulator.so)
 * through import, the library, playing, the pause menu, save states and
 * settings, saving a screenshot at each step to build/screenshots.
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi")
class AppTourTest {
    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private val context: Context get() = ApplicationProvider.getApplicationContext()
    private val shots = File(System.getProperty("kd.screenshots") ?: "build/screenshots").apply { mkdirs() }

    @Before
    fun handlerFramePump() {
        // See GameView.vsyncPump.
        com.kdemulator.ui.game.GameView.vsyncPump = false
    }

    private fun shot(name: String) {
        compose.waitForIdle()
        shadowOf(Looper.getMainLooper()).idle()
        // Draw the whole window (Compose content and the GameView) in software.
        val root = compose.activity.window.decorView
        val bmp = Bitmap.createBitmap(root.width, root.height, Bitmap.Config.ARGB_8888)
        root.draw(android.graphics.Canvas(bmp))
        File(shots, "$name.png").outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    /** Let the native thread run, then deliver vsyncs so GameView pulls frames. */
    private fun runGame(ms: Long) {
        compose.waitForIdle() // compose the game screen, which starts the emulation thread
        Thread.sleep(ms)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(100))
        compose.waitForIdle()
    }

    private fun findGameView(v: android.view.View): com.kdemulator.ui.game.GameView? = when (v) {
        is com.kdemulator.ui.game.GameView -> v
        is android.view.ViewGroup -> (0 until v.childCount).firstNotNullOfOrNull { findGameView(v.getChildAt(it)) }
        else -> null
    }

    private fun write(name: String, bytes: ByteArray) = File(context.cacheDir, name).apply { writeBytes(bytes) }

    @Test
    fun portraitTour() {
        NativeEmulatorBridge.nativeInit(context.cacheDir.path)
        val services = AppServices(context)
        compose.setContent { IslandTheme(darkTheme = true) { App(services, "Tensor G5 · 1+5+2 · page 16384 B", {}) } }
        shot("01_library_empty")

        val imports = listOf(
            write("KD Testcart.gba", Fixtures.gbaGradientRom()),
            write("Island Days.nds", Fixtures.dsRom("ISLANDDAYS", "KDID", "Island Days\nKD Homebrew", Fixtures.leafIcon())),
            write("wild_world.nds", Fixtures.dsRom("WILDWORLD", "ADME", "Animal Crossing\nWild World\nNintendo", Fixtures.houseIcon())),
        ).map { services.library.importFile(it) }
        imports.forEach { assertTrue("import $it", it is ImportResult.Added) }
        assertTrue("re-import is a duplicate", services.library.importFile(write("again.gba", Fixtures.gbaGradientRom())) is ImportResult.Duplicate)
        val games = services.library.games.value
        assertEquals(setOf(Console.GBA, Console.NDS), games.map { it.console }.toSet())
        val ww = games.first { it.code == "ADME" }
        assertTrue("Wild World recognised", ww.animalCrossing)
        assertNotNull("DS icon extracted", services.library.artwork(ww.id))
        assertTrue("disc images are rejected", services.library.importFile(write("disc.iso", ByteArray(0x8000))) is ImportResult.Failed)
        shot("02_library_gba")
        compose.onRoot().performTouchInput { swipeLeft() }
        shot("03_library_ds")
        compose.onRoot().performTouchInput { swipeRight() }
        compose.waitForIdle()

        // Play the GBA test cartridge.
        compose.onAllNodesWithText("KD TESTCART")[0].performClick()
        runGame(600)
        val gameView = findGameView(compose.activity.window.decorView)
        assertNotNull("GameView is on screen", gameView)
        val size = NativeEmulatorBridge.nativeGetFrameSize()
        assertEquals("native frame is 240x160", 240 shl 16 or 160, size)
        gameView!!.pullFrame()
        val frame = gameView.snapshot()
        assertNotNull("GameView pulled a frame", frame)
        val colours = IntArray(frame!!.width * frame.height).also { frame.getPixels(it, 0, frame.width, 0, 0, frame.width, frame.height) }.toSet().size
        assertTrue("the test cartridge's colour ramp reached the view ($colours colours)", colours > 100)
        shot("05_game_gba")

        // Press A on the built-in skin and check the input reached the hub.
        val root = compose.onRoot().fetchSemanticsNode().size
        val density = compose.density.density
        val rep = BuiltInSkins.layout(Console.GBA, root.width / density, root.height / density, false, true)
        val a = rep.items.first { (it.inputs as? SkinInputs.Buttons)?.names == listOf("a") }.frame
        compose.onRoot().performTouchInput { down(Offset(a.centerX() * density, a.centerY() * density)) }
        compose.waitForIdle()
        assertNotEquals("A pressed through the skin", 0, InputHub.touchButtons and Buttons.A)
        compose.onRoot().performTouchInput { up() }
        compose.waitForIdle()
        assertEquals(0, InputHub.touchButtons and Buttons.A)

        // Open the pause menu with the skin's menu button.
        val menu = rep.items.first { (it.inputs as? SkinInputs.Buttons)?.names == listOf("menu") }.frame
        compose.onRoot().performTouchInput { click(Offset(menu.centerX() * density, menu.centerY() * density)) }
        shot("06_pause_menu")

        compose.onNodeWithText("Save State").performClick()
        compose.onNodeWithContentDescription("New save state").performClick()
        shot("07_save_states")
        compose.onNodeWithContentDescription("Close").performClick()
        compose.onNodeWithText("Cheat Codes").performClick()
        shot("08_cheats")
        compose.onNodeWithContentDescription("Close").performClick()
        compose.onNodeWithText("Main Menu").performClick()
        compose.waitForIdle()
        val gba = games.first { it.console == Console.GBA }
        assertNotNull("leaving makes an auto save", services.states.latest(gba.id, SaveStateKind.AUTO))
        assertNotNull("auto save has a screenshot", services.states.latest(gba.id, SaveStateKind.AUTO)?.thumbnail())

        compose.onNodeWithContentDescription("Settings").performClick()
        shot("09_settings")
        compose.onNodeWithText("Nintendo DS").performClick()
        shot("10_skins_ds")
    }

    @Test
    fun landscapeAndDualScreen() {
        RuntimeEnvironment.setQualifiers("w891dp-h411dp-land-xxhdpi")
        NativeEmulatorBridge.nativeInit(context.cacheDir.path)
        val services = AppServices(context)
        services.library.importFile(write("KD Testcart.gba", Fixtures.gbaGradientRom()))
        services.library.importFile(write("wild_world.nds", Fixtures.dsRom("WILDWORLD", "ADME", "Animal Crossing\nWild World\nNintendo", Fixtures.houseIcon())))
        compose.setContent { IslandTheme(darkTheme = true) { App(services, "Tensor G5", {}) } }
        compose.onAllNodesWithText("KD TESTCART")[0].performClick()
        runGame(500)
        shot("11_game_gba_landscape")
    }
}

/** Synthetic games built byte by byte (no commercial data). */
object Fixtures {
    private fun put32le(b: ByteArray, at: Int, v: Long) { for (i in 0..3) b[at + i] = (v shr (8 * i)).toByte() }
    private fun put16le(b: ByteArray, at: Int, v: Int) { b[at] = v.toByte(); b[at + 1] = (v shr 8).toByte() }
    private fun ascii(b: ByteArray, at: Int, s: String) = s.toByteArray(Charsets.US_ASCII).copyInto(b, at)

    /** GBA cartridge whose code switches to mode 3 and paints a colour ramp. */
    fun gbaGradientRom(): ByteArray {
        val rom = ByteArray(0x200)
        put32le(rom, 0, 0xEA00002E) // b 0xC0
        val prog = longArrayOf(
            0xE3A00301, // mov r0, #0x04000000
            0xE3A01B01, // mov r1, #0x400
            0xE2811003, // add r1, r1, #3      (mode 3, BG2 on)
            0xE1C010B0, // strh r1, [r0]
            0xE3A00406, // mov r0, #0x06000000
            0xE3A02000, // mov r2, #0
            0xE0C020B2, // loop: strh r2, [r0], #2
            0xE2822001, // add r2, r2, #1
            0xE3520C96, // cmp r2, #0x9600     (240*160 pixels)
            0xBAFFFFFB, // blt loop
            0xEAFFFFFE, // b .
        )
        prog.forEachIndexed { i, w -> put32le(rom, 0xC0 + i * 4, w) }
        ascii(rom, 0xA0, "KD TESTCART")
        ascii(rom, 0xAC, "AKDE")
        ascii(rom, 0xB0, "01")
        rom[0xB2] = 0x96.toByte()
        var chk = 0
        for (i in 0xA0..0xBC) chk -= rom[i].toInt() and 0xFF
        rom[0xBD] = ((chk - 0x19) and 0xFF).toByte()
        return rom
    }

    /** 32x32 palette-index icons. */
    fun leafIcon(): (Int, Int) -> Int = { x, y ->
        val dx = x - 16.0
        val dy = y - 16.0
        val u = (dx + dy) / 1.4
        val v = (dx - dy) / 1.4
        when {
            kotlin.math.abs(v) < 1.2 && kotlin.math.abs(u) < 13 -> 2
            (u * u) / 170 + (v * v) / 45 < 1 -> 1
            else -> 0
        }
    }

    fun houseIcon(): (Int, Int) -> Int = { x, y ->
        when {
            y in 6..15 && kotlin.math.abs(x - 16) <= (y - 4) -> 3 // roof
            y in 16..27 && x in 7..25 -> if (x in 14..18 && y >= 20) 2 else 4 // walls + door
            y >= 28 -> 1 // grass
            else -> 0
        }
    }

    private val PALETTE = intArrayOf(0, 0x1F0A /* leaf */, 0x0D45 /* dark */, 0x0C7F /* roof red */, 0x5BBF /* sand */)

    fun dsRom(title: String, code: String, bannerTitle: String, icon: (Int, Int) -> Int): ByteArray {
        val rom = ByteArray(0x2000)
        ascii(rom, 0, title.take(12))
        ascii(rom, 0x0C, code)
        ascii(rom, 0x10, "01")
        put32le(rom, 0x68, 0x1000)
        val b = 0x1000
        put16le(rom, b, 1)
        for (y in 0 until 32) for (x in 0 until 32) {
            val idx = icon(x, y) and 0xF
            val tile = (y / 8) * 4 + x / 8
            val at = b + 0x20 + tile * 32 + (y % 8) * 4 + (x % 8) / 2
            rom[at] = (rom[at].toInt() or if (x % 2 == 0) idx else idx shl 4).toByte()
        }
        PALETTE.forEachIndexed { i, c -> put16le(rom, b + 0x220 + i * 2, c) }
        bannerTitle.toCharArray().forEachIndexed { i, ch -> put16le(rom, b + 0x340 + i * 2, ch.code) }
        return rom
    }
}
