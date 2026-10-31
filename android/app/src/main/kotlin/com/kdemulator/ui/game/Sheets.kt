package com.kdemulator.ui.game

import android.graphics.Bitmap
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.Circle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import com.kdemulator.bridge.NativeEmulatorBridge
import com.kdemulator.data.AppServices
import com.kdemulator.data.Cheat
import com.kdemulator.data.CheatFormat
import com.kdemulator.data.Game
import com.kdemulator.data.SaveStateEntry
import com.kdemulator.data.SaveStateKind
import com.kdemulator.ui.theme.Island
import java.text.DateFormat
import java.util.Date

@Composable
private fun SheetScaffold(title: String, onClose: () -> Unit, action: (@Composable () -> Unit)? = null, content: @Composable () -> Unit) {
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onClose) { Icon(Icons.Filled.Close, "Close") }
                Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                action?.invoke()
            }
            content()
        }
    }
}

/** Delta-style save state browser. In save mode "+" makes a new state and
 *  tapping a state overwrites it; in load mode tapping loads. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SaveStatesSheet(services: AppServices, game: Game, saving: Boolean, screenshot: () -> Bitmap?, onDone: (loaded: Boolean) -> Unit) {
    var version by remember { mutableIntStateOf(0) }
    val entries = remember(version) { services.states.list(game.id) }
    var confirmOverwrite by remember { mutableStateOf<SaveStateEntry?>(null) }
    var menuFor by remember { mutableStateOf<SaveStateEntry?>(null) }
    var renaming by remember { mutableStateOf<SaveStateEntry?>(null) }
    val aspect = game.console.screenWidth.toFloat() / game.console.screenHeight

    SheetScaffold(
        title = if (saving) "Save State" else "Load State",
        onClose = { onDone(false) },
        action = if (saving) ({
            IconButton(onClick = {
                services.states.save(game.id, SaveStateKind.GENERAL, screenshot())
                version++
            }) { Icon(Icons.Filled.Add, "New save state") }
        }) else null,
    ) {
        if (entries.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(if (saving) "Tap + to save your progress" else "No save states yet", style = MaterialTheme.typography.titleMedium)
            }
            return@SheetScaffold
        }
        LazyVerticalGrid(
            columns = GridCells.Adaptive(150.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            for (kind in SaveStateKind.entries) {
                val group = entries.filter { it.kind == kind }
                if (group.isEmpty()) continue
                item(span = { GridItemSpan(maxLineSpan) }, key = "h-$kind") {
                    Text(
                        when (kind) {
                            SaveStateKind.AUTO -> "Auto Save"
                            SaveStateKind.QUICK -> "Quick Save"
                            SaveStateKind.GENERAL -> "Save States"
                        },
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
                items(group, key = { it.id }) { e ->
                    Box {
                        Column(
                            Modifier.clip(RoundedCornerShape(14.dp)).combinedClickable(
                                onClick = {
                                    if (saving) {
                                        confirmOverwrite = e
                                    } else if (services.states.load(e)) {
                                        onDone(true)
                                    }
                                },
                                onLongClick = { menuFor = e },
                            ),
                        ) {
                            val thumb = remember(e.id, version) { e.thumbnail()?.asImageBitmap() }
                            Box(Modifier.fillMaxWidth().aspectRatio(aspect).background(Color.Black)) {
                                thumb?.let {
                                    Image(it, null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit, filterQuality = FilterQuality.None)
                                }
                            }
                            Text(
                                e.name ?: DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(e.createdAt)),
                                Modifier.padding(6.dp),
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 1,
                            )
                        }
                        DropdownMenu(expanded = menuFor == e, onDismissRequest = { menuFor = null }) {
                            DropdownMenuItem(text = { Text("Rename") }, onClick = { renaming = e; menuFor = null })
                            DropdownMenuItem(text = { Text("Delete") }, onClick = {
                                services.states.delete(game.id, e)
                                menuFor = null
                                version++
                            })
                        }
                    }
                }
            }
        }
    }

    confirmOverwrite?.let { e ->
        AlertDialog(
            onDismissRequest = { confirmOverwrite = null },
            title = { Text("Overwrite save state?") },
            text = { Text("This replaces the saved progress with where you are now.") },
            confirmButton = {
                TextButton(onClick = {
                    services.states.save(game.id, e.kind, screenshot(), replace = e)
                    confirmOverwrite = null
                    version++
                }) { Text("Overwrite") }
            },
            dismissButton = { TextButton(onClick = { confirmOverwrite = null }) { Text("Cancel") } },
        )
    }
    renaming?.let { e ->
        var name by remember { mutableStateOf(e.name.orEmpty()) }
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename save state") },
            text = { OutlinedTextField(name, { name = it }, singleLine = true, label = { Text("Name") }) },
            confirmButton = {
                TextButton(onClick = {
                    services.states.rename(game.id, e, name)
                    renaming = null
                    version++
                }) { Text("Save") }
            },
            dismissButton = { TextButton(onClick = { renaming = null }) { Text("Cancel") } },
        )
    }
}

/** Cheat list with on/off toggles and an editor validated by the native parser. */
@Composable
fun CheatsSheet(services: AppServices, game: Game, onClose: () -> Unit) {
    var version by remember { mutableIntStateOf(0) }
    val cheats = remember(version) { services.cheats.list(game.id) }
    var editing by remember { mutableStateOf<Cheat?>(null) }
    val formats = CheatFormat.forConsole(game.console)

    SheetScaffold(
        title = "Cheat Codes",
        onClose = onClose,
        action = {
            IconButton(onClick = { editing = Cheat(services.cheats.newId(), "", "", formats.first(), true) }) {
                Icon(Icons.Filled.Add, "Add cheat")
            }
        },
    ) {
        if (cheats.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
                Text(
                    "Tap + to add a cheat.\nSupported: ${formats.joinToString { it.label }}",
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            return@SheetScaffold
        }
        LazyColumn(contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 8.dp)) {
            items(cheats, key = { it.id }) { c ->
                Row(
                    Modifier.fillMaxWidth().clickable {
                        services.cheats.toggle(game.id, c.id)
                        version++
                    }.padding(horizontal = 20.dp, vertical = 14.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        if (c.enabled) Icons.Filled.CheckCircle else Icons.Outlined.Circle,
                        if (c.enabled) "On" else "Off",
                        tint = if (c.enabled) Island.Leaf else MaterialTheme.colorScheme.onBackground.copy(alpha = 0.4f),
                    )
                    Column(Modifier.weight(1f).padding(horizontal = 14.dp)) {
                        Text(c.name.ifBlank { "Untitled cheat" }, style = MaterialTheme.typography.titleMedium)
                        Text(c.format.label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onBackground.copy(alpha = 0.6f))
                    }
                    TextButton(onClick = { editing = c }) { Text("Edit") }
                }
            }
        }
    }

    editing?.let { c ->
        var name by remember(c.id) { mutableStateOf(c.name) }
        var code by remember(c.id) { mutableStateOf(c.code) }
        var format by remember(c.id) { mutableStateOf(c.format) }
        var error by remember(c.id) { mutableStateOf<String?>(null) }
        val existing = cheats.any { it.id == c.id }
        AlertDialog(
            onDismissRequest = { editing = null },
            title = { Text(if (existing) "Edit Cheat" else "New Cheat") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    OutlinedTextField(name, { name = it }, label = { Text("Name") }, singleLine = true)
                    if (formats.size > 1) {
                        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            formats.forEach { f -> FilterChip(selected = f == format, onClick = { format = f }, label = { Text(f.label) }) }
                        }
                    }
                    OutlinedTextField(
                        code,
                        { code = it.uppercase(); error = null },
                        label = { Text("Code") },
                        placeholder = { Text(format.example) },
                        minLines = 3,
                        textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters),
                        isError = error != null,
                        supportingText = { error?.let { Text(it) } },
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    error = services.cheats.validate(format, code)
                    if (error == null) {
                        services.cheats.upsert(game.id, c.copy(name = name.trim(), code = code.trim(), format = format))
                        editing = null
                        version++
                    }
                }) { Text("Save") }
            },
            dismissButton = {
                Row {
                    if (existing) {
                        TextButton(onClick = {
                            services.cheats.delete(game.id, c.id)
                            editing = null
                            version++
                        }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
                    }
                    TextButton(onClick = { editing = null }) { Text("Cancel") }
                }
            },
        )
    }
}

private const val HOUR = 3_600L
private const val DAY = 86_400L

/** Shift the guest RTC: seasons, holidays and visitors follow it in Animal Crossing. */
@Composable
fun TimeTravelDialog(isAnimalCrossing: Boolean, onShift: (Long) -> Unit, onReset: () -> Unit, onDismiss: () -> Unit) {
    var clock by remember { mutableStateOf(NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetAnimalCrossingInfo())["clock"]) }
    var offset by remember { mutableStateOf(NativeEmulatorBridge.nativeGetClockOffset()) }
    fun refresh() {
        clock = NativeEmulatorBridge.parseInfo(NativeEmulatorBridge.nativeGetAnimalCrossingInfo())["clock"]
        offset = NativeEmulatorBridge.nativeGetClockOffset()
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = { TextButton(onClick = onDismiss) { Text("Done") } },
        dismissButton = { TextButton(onClick = { onReset(); refresh() }) { Text("Back to real time") } },
        title = { Text(if (isAnimalCrossing) "🍃 Time Travel" else "Console Clock") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                clock?.let { Text("Game clock: $it") }
                Text(
                    when {
                        offset == 0L -> "Following real time"
                        offset > 0 -> "Ahead by ${describe(offset)}"
                        else -> "Behind by ${describe(-offset)}"
                    },
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (isAnimalCrossing) {
                    Text("Only the game's clock moves — your phone's clock is never changed.", style = MaterialTheme.typography.bodyMedium)
                }
                Spacer(Modifier.height(4.dp))
                for ((label, secs) in listOf("hour" to HOUR, "day" to DAY, "month" to 30 * DAY)) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilledTonalButton(onClick = { onShift(-secs); refresh() }) { Text("−1 $label") }
                        FilledTonalButton(onClick = { onShift(secs); refresh() }) { Text("+1 $label") }
                    }
                }
            }
        },
    )
}

private fun describe(secs: Long): String {
    val d = secs / DAY
    val h = (secs % DAY) / HOUR
    return buildString {
        if (d > 0) append("$d day${if (d == 1L) "" else "s"}")
        if (h > 0) append((if (isNotEmpty()) " " else "") + "$h hour${if (h == 1L) "" else "s"}")
        if (isEmpty()) append("${secs / 60} min")
    }
}
