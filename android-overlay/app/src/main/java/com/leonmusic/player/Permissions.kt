package com.leonmusic.player

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat

/**
 * 运行时权限。
 *
 * ## 为什么权限清单要按版本分叉
 *
 * * Android 13（API 33）起，读取音频文件用 `READ_MEDIA_AUDIO`，
 *   `READ_EXTERNAL_STORAGE` 对媒体文件已经失效
 * * 同一版本起，通知必须 `POST_NOTIFICATIONS`，否则前台服务的通知不显示
 *   （播放本身不受影响，但用户看不到控制按钮，体验很不完整）
 * * Android 12 及以下仍然只有 `READ_EXTERNAL_STORAGE`
 *
 * `WRITE_EXTERNAL_STORAGE` 不申请：应用只读媒体库，写操作都在
 * 应用私有目录（`getExternalFilesDir` / `filesDir`），无需存储写权限。
 *
 * ## SAF 说明
 *
 * 用户选了「通过文件夹导入」时走的是 `ACTION_OPEN_DOCUMENT_TREE`，
 * 拿到的 tree URI 需要 `takePersistableUriPermission` 长期持有；
 * 那条路径**完全不需要**上面的存储权限（这是 Android 的推荐做法）。
 */
object Permissions {

    fun required(): List<String> {
        val list = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            list += Manifest.permission.READ_MEDIA_AUDIO
            list += Manifest.permission.POST_NOTIFICATIONS
        } else {
            list += Manifest.permission.READ_EXTERNAL_STORAGE
        }
        return list
    }

    fun missing(context: Context): Array<String> = required()
        .filter {
            ContextCompat.checkSelfPermission(context, it) != PackageManager.PERMISSION_GRANTED
        }
        .toTypedArray()

    fun canPostNotifications(context: Context): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            ContextCompat.checkSelfPermission(
                context,
                Manifest.permission.POST_NOTIFICATIONS,
            ) == PackageManager.PERMISSION_GRANTED
        } else {
            true
        }
}
