package com.kdemulator.ui.game

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.BugReport
import androidx.compose.material.icons.filled.Code
import androidx.compose.material.icons.filled.FastForward
import androidx.compose.material.icons.filled.FastRewind
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Restore
import androidx.compose.material.icons.filled.Save
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.Spa
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.kdemulator.bridge.NativeEmulatorBridge.Console
import com.kdemulator.ui.theme.Island

enum class PauseAction { SAVE_STATE, LOAD_STATE, CHEATS, FAST_FORWARD, REWIND, HOLD_BUTTONS, SCREENSHOT, TIME_TRAVEL, MICROPHONE, DEBUGGER, MAIN_MENU }

private data class MenuItem(val action: PauseAction, val label: String, val icon: ImageVector, val selected: Boolean = false, val tint: Color? = null)

/** The pause menu: a grid of round actions over the blurred game, like Delta. */
@Composable
fun PauseMenu(
    title: String,
    console: Console,
    isAnimalCrossing: Boolean,
    fastForward: Boolean,
    micOn: Boolean,
    heldCount: Int,
    rewindStates: Int,
    onResume: () -> Unit,
    onAction: (PauseAction) -> Unit,
) {
    val items = buildList {
        add(MenuItem(PauseAction.SAVE_STATE, "Save State", Icons.Filled.Save))
        add(MenuItem(PauseAction.LOAD_STATE, "Load State", Icons.Filled.Restore))
        add(MenuItem(PauseAction.CHEATS, "Cheat Codes", Icons.Filled.Code))
        add(MenuItem(PauseAction.FAST_FORWARD, "Fast Forward", Icons.Filled.FastForward, selected = fastForward))
        if (rewindStates > 0) add(MenuItem(PauseAction.REWIND, "Rewind", Icons.Filled.FastRewind))
        add(MenuItem(PauseAction.HOLD_BUTTONS, if (heldCount > 0) "Hold Buttons ($heldCount)" else "Hold Buttons", Icons.Filled.PanTool, selected = heldCount > 0))
        add(MenuItem(PauseAction.SCREENSHOT, "Screenshot", Icons.Filled.PhotoCamera))
        add(
            MenuItem(
                PauseAction.TIME_TRAVEL,
                "Time Travel",
                if (isAnimalCrossing) Icons.Filled.Spa else Icons.Filled.Schedule,
                tint = if (isAnimalCrossing) Island.Leaf else null,
            ),
        )
        if (console == Console.NDS) add(MenuItem(PauseAction.MICROPHONE, "Microphone", if (micOn) Icons.Filled.Mic else Icons.Filled.MicOff, selected = micOn))
        add(MenuItem(PauseAction.DEBUGGER, "Debug Info", Icons.Filled.BugReport))
    }

    Box(
        Modifier.fillMaxSize()
            .background(Color(0xB30B1218))
            .clickable(interactionSource = MutableInteractionSource(), indication = null, onClick = onResume)
            .safeDrawingPadding(),
    ) {
        Column(Modifier.align(Alignment.Center).widthIn(max = 520.dp).padding(horizontal = 20.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = { onAction(PauseAction.MAIN_MENU) }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, null, tint = Color.White)
                    Text("Main Menu", Modifier.padding(start = 6.dp), color = Color.White)
                }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onResume) {
                    Text("Resume", Modifier.padding(end = 6.dp), color = Island.Leaf, fontWeight = FontWeight.Bold)
                    Icon(Icons.Filled.PlayArrow, null, tint = Island.Leaf)
                }
            }
            Text(
                title,
                Modifier.fillMaxWidth().padding(vertical = 8.dp),
                color = Color.White,
                style = MaterialTheme.typography.titleLarge,
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.height(8.dp))
            LazyVerticalGrid(
                columns = GridCells.Fixed(4),
                verticalArrangement = Arrangement.spacedBy(18.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(items, key = { it.action }) { item -> MenuButton(item) { onAction(item.action) } }
            }
        }
    }
}

@Composable
private fun MenuButton(item: MenuItem, onClick: () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.clickable(onClick = onClick)) {
        val bg = if (item.selected) Island.Leaf else Color.White.copy(alpha = 0.12f)
        Box(
            Modifier.size(62.dp).clip(CircleShape).background(bg).border(1.dp, Color.White.copy(alpha = 0.35f), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Icon(item.icon, item.label, tint = item.tint ?: Color.White, modifier = Modifier.size(28.dp))
        }
        Text(
            item.label,
            Modifier.padding(top = 6.dp),
            color = Color.White,
            fontSize = 12.sp,
            textAlign = TextAlign.Center,
            maxLines = 2,
            lineHeight = 14.sp,
        )
    }
}
