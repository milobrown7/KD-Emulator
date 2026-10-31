package com.kdemulator.data

import android.content.Context
import com.kdemulator.skins.SkinRepository

/** Everything the screens share, created once per process. */
class AppServices(context: Context) {
    val library = GameLibrary(context)
    val states = SaveStateStore(context)
    val cheats = CheatStore(context)
    val settings = AppSettings(context)
    val skins = SkinRepository(context)
}
