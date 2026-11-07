package com.leonmusic.player

import android.app.Activity
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import java.lang.ref.WeakReference

/**
 * 把系统栏 inset 注入到 WebView 的 CSS 变量（`--sat` / `--sab`）。
 *
 * ## 为什么不直接用 CSS 的 `env(safe-area-inset-*)`
 *
 * Chrome for Android 只把「刘海（display cutout）」算作 safe area，
 * 导航栏 / 手势条一律报 `0`。实测：
 * - 模拟器（`navigation_mode=2`，手势导航）`env(safe-area-inset-bottom) = 0`，
 *   而截图里手势条正好压在底部 tab 的文字上；
 * - 真机反而报 20px，属于厂商 WebView 的额外扩展，不能依赖。
 *
 * 只有 iOS 的 Safari 会完整上报四个方向，所以 Android 侧必须自己算。
 *
 * ## 时序问题
 *
 * `evaluateJavascript` 要等页面加载出 `document.documentElement` 才有意义，
 * 而 insets 在首帧之前就算好了。这里用「有限次重试」而不是猜一个固定延迟：
 * 猜短了白推一次，猜长了底部会先闪一下未让开的样子。
 */
object WindowInsetsBridge {

    private const val TAG = "LeonInsets"

    /** 最多重试 24 次 × 250ms ≈ 6 秒，足够覆盖冷启动到首屏 */
    private const val MAX_ATTEMPTS = 24
    private const val RETRY_DELAY_MS = 250L

    private var insetTopPx = 0
    private var insetBottomPx = 0
    private var attempts = 0

    /** 弱引用避免 Activity 销毁后泄漏；换 Activity 时 attach 会覆盖 */
    private var activityRef: WeakReference<Activity>? = null

    fun attach(activity: Activity) {
        activityRef = WeakReference(activity)
        val decor = activity.window.decorView
        ViewCompat.setOnApplyWindowInsetsListener(decor) { v, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            if (bars.top != insetTopPx || bars.bottom != insetBottomPx) {
                insetTopPx = bars.top
                insetBottomPx = bars.bottom
                attempts = 0
                push(activity)
            }
            // 继续往下分发，别把 wry 自己的 inset 处理吃掉
            ViewCompat.onApplyWindowInsets(v, insets)
        }
        // 首次 insets 可能在 onCreate 之后才稳定，补推一次
        decor.postDelayed({ push(activity) }, 300)
    }

    /**
     * 系统栏图标的明暗。
     *
     * edge-to-edge 之后状态栏和手势条都是透明的，铺的是应用自己的背景，
     * 唯一还归系统管的就是那几个图标 —— 深色主题下必须把图标切成浅色，
     * 否则就是「深色背景 + 深色图标」，等于看不见。
     *
     * 由 Rust 的 `app_set_dark_appearance` 经 [LeonBridge] 转发过来。
     */
    fun setSystemBarAppearance(dark: Boolean) {
        val activity = activityRef?.get() ?: return
        activity.runOnUiThread {
            if (activity.isFinishing || activity.isDestroyed) return@runOnUiThread
            try {
                WindowCompat.getInsetsController(activity.window, activity.window.decorView).apply {
                    isAppearanceLightStatusBars = !dark
                    isAppearanceLightNavigationBars = !dark
                }
            } catch (t: Throwable) {
                Log.w(TAG, "setSystemBarAppearance failed: ${t.message}")
            }
        }
    }

    private fun push(activity: Activity) {
        if (activity.isFinishing || activity.isDestroyed) return
        val web = findWebView(activity.window.decorView)
        if (web == null) {
            retry(activity)
            return
        }
        val density = activity.resources.displayMetrics.density.coerceAtLeast(1f)
        val top = insetTopPx / density
        val bottom = insetBottomPx / density
        // 接收函数由前端 main.ts 定义并返回 true，用于判断 JS 是否已就绪
        val js = "window.__leonInsets ? window.__leonInsets($top, $bottom) : false"

        web.post {
            try {
                web.evaluateJavascript(js) { result ->
                    // 函数还不存在时返回 "false"
                    if (result != "true") retry(activity)
                }
            } catch (t: Throwable) {
                Log.w(TAG, "evaluateJavascript failed: ${t.message}")
            }
        }
    }

    private fun retry(activity: Activity) {
        if (attempts >= MAX_ATTEMPTS) {
            Log.w(TAG, "give up injecting insets after $MAX_ATTEMPTS attempts")
            return
        }
        attempts++
        activity.window.decorView.postDelayed({ push(activity) }, RETRY_DELAY_MS)
    }

    /** wry 的 WebView 藏在 TauriActivity 的 content view 里，没有公开引用，只能自己找。 */
    private fun findWebView(root: View): WebView? {
        if (root is WebView) return root
        if (root is ViewGroup) {
            for (i in 0 until root.childCount) {
                findWebView(root.getChildAt(i))?.let { return it }
            }
        }
        return null
    }
}
