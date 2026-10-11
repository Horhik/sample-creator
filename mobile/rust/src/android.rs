//! Hands the app context to `ndk-context`, which cpal needs to open audio
//! streams. Called once from `MainActivity` before Dart touches the engine.

use jni_sys::{JNIEnv, JavaVM, jclass, jobject};

/// `MainActivity.initAndroidContext(context: Context)`
///
/// # Safety
/// Called by the JVM with a valid environment and a live `Context`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_io_github_horhik_sample_1creator_MainActivity_initAndroidContext(
    env: *mut JNIEnv,
    _class: jclass,
    context: jobject,
) {
    unsafe {
        let funcs = &**env;
        let (Some(get_vm), Some(global_ref)) = (funcs.GetJavaVM, funcs.NewGlobalRef) else {
            return;
        };
        let mut vm: *mut JavaVM = std::ptr::null_mut();
        if get_vm(env, &mut vm) != 0 || vm.is_null() {
            return;
        }
        // A global ref keeps the context valid for the life of the process.
        let context = global_ref(env, context);
        ndk_context::initialize_android_context(vm.cast(), context.cast());
    }
}
