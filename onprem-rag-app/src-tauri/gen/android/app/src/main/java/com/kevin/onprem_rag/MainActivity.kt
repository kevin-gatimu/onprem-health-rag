package com.kevin.onprem_rag

import android.graphics.Color
import android.os.Bundle
import android.view.View
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Transparent system bars, but pinned to the dark style rather than letting
    // DayNight choose: the web UI is dark-only, so a light-mode device would pick
    // dark status-bar icons and render them invisible against our dark band.
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)

    // Edge-to-edge (forced from targetSdk 35+) lays the WebView out *under* the
    // status and navigation bars, which is what hides the TopBar behind the clock
    // and notch. CSS env(safe-area-inset-*) can't be relied on to fix it: WebView
    // only reports those for non-fullscreen activities from M144, so on most
    // devices in the field they resolve to 0. Inset the native content view
    // instead — correct on every WebView version — and zero the insets we consumed
    // so the web layer never double-pads on top of it.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, windowInsets ->
      val types = WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
      val insets = windowInsets.getInsets(types)
      view.setPadding(insets.left, insets.top, insets.right, insets.bottom)
      WindowInsetsCompat.Builder(windowInsets)
        .setInsets(types, Insets.NONE)
        .build()
    }
  }
}
