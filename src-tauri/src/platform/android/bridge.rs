//! Rust ↔ Kotlin 的 JNI 桥。
//!
//! 方向一：Rust → Kotlin（`LeonBridge` 静态方法），用于更新通知与 MediaSession。
//! 方向二：Kotlin → Rust（[`Java_com_leonmusic_player_LeonBridge_nativeCommand`]），
//! 通知栏按钮、锁屏、耳机线控都从这里进来。
//!
//! 所有调用都做了「拿不到 JVM 就静默降级」的处理：这样同一份代码在
//! 桌面 `cargo check` 时不会被平台代码拖累。

use crossbeam_channel::Sender;
use parking_lot::Mutex;

use crate::audio::engine::EngineCommand;

const BRIDGE_CLASS: &str = "com/leonmusic/player/LeonBridge";

static COMMAND_SINK: Mutex<Option<Sender<EngineCommand>>> = Mutex::new(None);

pub fn install_command_sink(tx: Sender<EngineCommand>) {
    *COMMAND_SINK.lock() = Some(tx);
}

/// 调用 Kotlin 侧的 `LeonBridge.dispatch(action, payload)`。
pub fn call(action: &str, payload: &str) {
    #[cfg(target_os = "android")]
    {
        if let Err(e) = call_inner(action, payload) {
            // 用 warn 而不是 debug：桥断了的表现是「通知栏不动」，
            // 用户很难自己归因，日志必须默认可见。
            tracing::warn!("android bridge call failed ({action}): {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (action, payload);
    }
}

#[cfg(target_os = "android")]
fn call_inner(action: &str, payload: &str) -> Result<(), String> {
    use jni::objects::{JString, JValue};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;

    let class = bridge_class(&mut env)?;
    let a: JString = env.new_string(action).map_err(|e| e.to_string())?.into();
    let p: JString = env.new_string(payload).map_err(|e| e.to_string())?.into();

    let result = env.call_static_method(
        class,
        "dispatch",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JValue::Object(&a), JValue::Object(&p)],
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            // JNI 调用失败后异常仍然挂在 env 上，不清掉会在返回 Java 时
            // 变成 FATAL EXCEPTION（整个进程被杀）。
            let _ = env.exception_clear();
            Err(format!("call dispatch: {e}"))
        }
    }
}

/// 拿到 `LeonBridge` 的 Class 对象。
///
/// # 为什么不能直接 `env.find_class(...)`
///
/// `FindClass` 的行为取决于**当前线程**：在 Java 线程上它用该方法的
/// 定义类加载器，而在 Rust 自己的线程（引擎线程、ticker 线程）上它只会用
/// bootstrap classloader，于是：
///
/// ```text
/// java.lang.ClassNotFoundException: Didn't find class
///   "com.leonmusic.player.LeonBridge" on path: DexPathList[[directory "."]]
/// ```
///
/// 进程随即被 ART 杀掉。正确做法是从 Application context 取出
/// `ClassLoader` 再 `loadClass`——它对任何线程都有效。
///
/// 这里不做缓存：调用频率是「每次状态推送」（约 1 次/秒），
/// 相比解码/重采样可以忽略，换来的是没有全局可变状态。
#[cfg(target_os = "android")]
fn bridge_class<'local>(
    env: &mut jni::JNIEnv<'local>,
) -> Result<jni::objects::JClass<'local>, String> {
    use jni::objects::{JObject, JValue};

    let ctx = ndk_context::android_context();
    // ndk_context 里存的是 wry 创建的全局引用，不需要（也不能）删除
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };

    let loader = env
        .call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("getClassLoader: {e}"))?;

    let name = env
        .new_string(BRIDGE_CLASS.replace('/', "."))
        .map_err(|e| e.to_string())?;

    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&JObject::from(name))],
        )
        .and_then(|v| v.l());

    match class {
        Ok(c) => Ok(jni::objects::JClass::from(c)),
        Err(e) => {
            let _ = env.exception_clear();
            Err(format!("loadClass {BRIDGE_CLASS}: {e}"))
        }
    }
}

pub fn sdk_int() -> Option<i32> {
    #[cfg(target_os = "android")]
    {
        use jni::JavaVM;
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let mut env = vm.attach_current_thread().ok()?;
        let class = env.find_class("android/os/Build$VERSION").ok()?;
        let field = env
            .get_static_field(class, "SDK_INT", "I")
            .ok()?;
        field.i().ok()
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Kotlin → Rust
// ---------------------------------------------------------------------------

/// 由 `LeonBridge.nativeCommand(action)` 调用。
///
/// 必须保持 `extern "system"` + 精确的符号名，Kotlin 侧用
/// `external fun nativeCommand(action: String)` 声明。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_leonmusic_player_LeonBridge_nativeCommand<'local>(
    mut env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    action: jni::objects::JString<'local>,
) {
    let Ok(action) = env.get_string(&action) else {
        return;
    };
    let action: String = action.into();
    dispatch_from_kotlin(&action);
}

pub fn dispatch_from_kotlin(action: &str) {
    let cmd = match action {
        "play" => EngineCommand::Resume,
        "pause" => EngineCommand::Pause,
        "toggle" => EngineCommand::Toggle,
        "next" => EngineCommand::Next,
        "prev" => EngineCommand::Prev,
        "stop" => EngineCommand::Stop,
        // 来电 / 其他应用播放 → 必须让位，直接暂停（恢复由 Kotlin 侧判断）
        "focus-loss" => EngineCommand::Pause,
        // 导航播报 / 通知提示音 → 只压低音量，不打断播放
        "duck" => EngineCommand::Duck(true),
        "unduck" => EngineCommand::Duck(false),
        _ => return,
    };
    if let Some(tx) = COMMAND_SINK.lock().as_ref() {
        let _ = tx.send(cmd);
    }
    tracing::debug!("android media command: {action}");
}

/// Kotlin 启动后主动拉一次完整状态，用于 WebView 重建后的 UI 恢复。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_leonmusic_player_LeonBridge_nativeSnapshot<'local>(
    env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
) -> jni::sys::jstring {
    let json = crate::state::snapshot_json();
    match env.new_string(json) {
        Ok(s) => s.into_raw() as jni::sys::jstring,
        Err(_) => std::ptr::null_mut(),
    }
}
