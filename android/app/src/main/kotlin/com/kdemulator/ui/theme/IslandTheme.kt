package com.kdemulator.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * "Island" theme — a cosy, Animal Crossing–inspired palette (original
 * colours: leaf, sand, sky, wood and sunset) with soft rounded shapes.
 */
object Island {
    val Leaf = Color(0xFF5FAD56)
    val DeepLeaf = Color(0xFF2F6B2A)
    val Sand = Color(0xFFF7EBD3)
    val Cream = Color(0xFFFFF8E7)
    val Wood = Color(0xFF8D6E4A)
    val Sky = Color(0xFF7EC8E3)
    val Lagoon = Color(0xFF4FB3A9)
    val Sunset = Color(0xFFF2A65A)
    val Night = Color(0xFF1E2A38)
    val NightSurface = Color(0xFF2A3A4A)

    /** Accent for a system's Animal Crossing profile (from native `accent`). */
    fun accent(hex: String?): Color =
        hex?.toLongOrNull(16)?.let { Color(0xFF000000 or it) } ?: Leaf
}

private val LightColors = lightColorScheme(
    primary = Island.Leaf,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFCDEBC4),
    onPrimaryContainer = Island.DeepLeaf,
    secondary = Island.Wood,
    onSecondary = Color.White,
    tertiary = Island.Sky,
    background = Island.Cream,
    onBackground = Color(0xFF3B3226),
    surface = Island.Sand,
    onSurface = Color(0xFF3B3226),
    surfaceVariant = Color(0xFFEFE0C2),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFF8FD486),
    onPrimary = Color(0xFF113B10),
    primaryContainer = Island.DeepLeaf,
    secondary = Color(0xFFD9B88F),
    tertiary = Island.Sky,
    background = Island.Night,
    onBackground = Color(0xFFF1E9DA),
    surface = Island.NightSurface,
    onSurface = Color(0xFFF1E9DA),
    surfaceVariant = Color(0xFF34495E),
)

private val Rounded = FontFamily.SansSerif

private val IslandTypography = Typography(
    headlineMedium = TextStyle(fontFamily = Rounded, fontWeight = FontWeight.Bold, fontSize = 28.sp),
    titleLarge = TextStyle(fontFamily = Rounded, fontWeight = FontWeight.Bold, fontSize = 20.sp),
    titleMedium = TextStyle(fontFamily = Rounded, fontWeight = FontWeight.SemiBold, fontSize = 16.sp),
    bodyMedium = TextStyle(fontFamily = Rounded, fontSize = 14.sp, lineHeight = 20.sp),
    labelLarge = TextStyle(fontFamily = Rounded, fontWeight = FontWeight.SemiBold, fontSize = 14.sp),
)

private val IslandShapes = Shapes(
    small = RoundedCornerShape(12.dp),
    medium = RoundedCornerShape(20.dp),
    large = RoundedCornerShape(28.dp),
)

@Composable
fun IslandTheme(darkTheme: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (darkTheme) DarkColors else LightColors,
        typography = IslandTypography,
        shapes = IslandShapes,
        content = content,
    )
}
