# JNI 是按「类名 + 方法名 + 签名」在运行时查方法的，R8 看不到这层引用。
# 不加 keep 时 release 包会混淆甚至裁掉这些入口，表现是：
#   Java_com_leonmusic_player_LeonBridge_nativeCommand 还在，
#   但 Rust 推过来的 metadata / state 全部 NoSuchMethodError —— 通知栏与锁屏一片空白。
# debug 包不开 R8，所以这个坑只在 release / 真机上出现。
-keep class com.leonmusic.player.LeonBridge {
    public static void attach(android.content.Context);
    public static void dispatch(java.lang.String, java.lang.String);
    public static java.lang.String query(java.lang.String);
    public static java.lang.String installApk(java.lang.String);
    public static native void nativeCommand(java.lang.String);
    public static native java.lang.String nativeSnapshot();
}

# Service 在 AndroidManifest.xml 里按类名声明，AGP 已默认保留类名；
# 这里额外保住成员名，方便 logcat 里按方法名过滤播放状态问题。
-keep class com.leonmusic.player.PlaybackService {
    public protected *;
}

# MediaButtonReceiver 同样只在 manifest 里出现，R8 可能把它当无引用代码裁掉。
-keep class androidx.media.session.MediaButtonReceiver { *; }
