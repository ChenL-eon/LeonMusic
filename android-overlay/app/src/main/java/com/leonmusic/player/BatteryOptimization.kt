package com.leonmusic.player

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.util.Log

/**
 * 电池优化白名单引导。
 *
 * ## 为什么这件事绕不过去
 *
 * 国产 ROM（MIUI / EMUI / ColorOS / FuntouchOS）和原生 Doze 都会在锁屏后
 * 一段时间杀掉后台进程。前台服务能挡一部分，但 Doze 的 `App Standby`
 * 仍可能在长时间熄屏后冻结我们的进程 —— 表现就是「听着听着突然停」。
 *
 * 唯一正规解法是让用户把应用加入「不受限制」名单：
 *
 * * 原生 / 类原生：`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`（有系统弹窗）
 * * 国产 ROM：该 Action 常被拦截，只能退到设置列表页让用户自己找
 *
 * ## 合规约束
 *
 * `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` 属于「敏感」权限，Google Play 要求
 * 必须在用户主动操作的上下文里申请，**不能静默弹**。所以这里：
 * 1. 只在第一次启动后引导一次（SharedPreferences 记标记）
 * 2. 用户拒绝后不再骚扰
 * 3. 任何时刻都可以在应用内设置页手动再次触发
 */
object BatteryOptimization {

    private const val TAG = "LeonBattery"
    private const val PREFS = "leon_prefs"
    private const val KEY_PROMPTED = "battery_prompted"

    fun isIgnoring(context: Context): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return true
        val pm = context.getSystemService(Context.POWER_SERVICE) as PowerManager
        return pm.isIgnoringBatteryOptimizations(context.packageName)
    }

    /** 是否已经引导过（避免每次冷启动都弹） */
    fun alreadyPrompted(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getBoolean(KEY_PROMPTED, false)

    fun markPrompted(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(KEY_PROMPTED, true)
            .apply()
    }

    /**
     * 拉起系统弹窗。调用前请确认 [isIgnoring] 为 false 且用户有意愿。
     *
     * 这里刻意**不**用 `@SuppressLint("BatteryLife")` 绕过的隐式调用方式，
     * 而是显式要求 `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` 权限（manifest 里已声明）。
     */
    @SuppressLint("BatteryLife")
    fun request(activity: Activity) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return
        val direct = Intent(
            Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
            Uri.parse("package:${activity.packageName}"),
        )
        try {
            activity.startActivity(direct)
            return
        } catch (t: Throwable) {
            Log.w(TAG, "direct request unsupported: ${t.message}")
        }
        // 国产 ROM 常见：直接申请被拒 → 退到应用电池优化列表
        try {
            activity.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        } catch (t: Throwable) {
            Log.w(TAG, "settings page unavailable: ${t.message}")
        }
    }
}
