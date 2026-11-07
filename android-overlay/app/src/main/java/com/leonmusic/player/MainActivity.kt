package com.leonmusic.player

import android.os.Bundle
import android.util.Log
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts

// 注意：不要 `import app.tauri.TauriActivity`。
// Tauri 的 codegen 会把 TauriActivity 生成到 **应用自己的包** 里
// （`gen/android/app/src/main/java/<pkg>/generated/TauriActivity.kt`，
//  其 package 是 com.leonmusic.player），所以这里直接继承同包类。
// 写成 app.tauri.TauriActivity 会直接 Unresolved reference。

/**
 * 应用唯一 Activity。
 *
 * ## 它做什么，不做什么
 *
 * 只负责三件事：
 * 1. 把 Application context 交给 [LeonBridge]（Rust 的 JNI 调用需要一个长期存活的 Context）
 * 2. 拉起 [PlaybackService]，让进程从启动那一刻就具备后台播放能力
 * 3. 申请运行时权限 + 引导电池优化白名单
 *
 * **不做**：播放、状态、队列 —— 全部在 Rust。
 *
 * ## 为什么 onDestroy 不停服务
 *
 * 用户按返回键 / 切后台时 Activity 会销毁，但音乐应该继续。服务的生命周期
 * 只跟「是否在播放」有关：Rust 推 `clear` 时服务只退出前台、不自杀，
 * 真正销毁发生在「从最近任务划掉且没在播」（`onTaskRemoved`）。
 */
class MainActivity : TauriActivity() {

    /**
     * 权限回调必须注册在 Activity STARTED 之前，
     * 所以放在属性初始化（构造期），不能写在 onCreate 里。
     */
    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { result ->
        val denied = result.filterValues { granted -> !granted }.keys
        if (denied.isNotEmpty()) {
            // 播放本身不依赖这些权限；通知被拒只是没有锁屏/通知栏控制
            Log.i(TAG, "permissions denied: $denied")
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        // 把状态栏 / 手势条高度写进 CSS 变量。必须在 super 之后：
        // 此时 decorView 才存在，WebView 也才能被找到。
        WindowInsetsBridge.attach(this)

        // Rust 的 setup 会立刻 dispatch("init")，这时必须已经有 context，
        // 否则第一条消息只能进待重放队列。
        // attach 内部会顺带把 PlaybackService 拉起来，不需要再单独调用。
        LeonBridge.attach(applicationContext)

        val missing = Permissions.missing(this)
        if (missing.isNotEmpty()) {
            permissionLauncher.launch(missing)
        }
    }

    override fun onResume() {
        super.onResume()
        promptBatteryOptimizationOnce()
    }

    /**
     * 只在首次启动引导一次。用户拒绝后不再打扰（符合 Google Play 对
     * `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` 的「用户主动」要求）。
     */
    private fun promptBatteryOptimizationOnce() {
        if (BatteryOptimization.isIgnoring(this)) return
        if (BatteryOptimization.alreadyPrompted(this)) return
        BatteryOptimization.markPrompted(this)
        // 等首屏渲染完再弹，避免和 Tauri 的启动动画抢焦点
        window.decorView.postDelayed({
            if (!isFinishing && !isDestroyed) {
                BatteryOptimization.request(this)
            }
        }, 2500)
    }

    companion object {
        private const val TAG = "LeonMain"
    }
}
