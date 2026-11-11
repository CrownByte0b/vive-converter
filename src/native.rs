//! `com.vivenotes.data.onenote.OneNoteConverter.convert` on Android, Linux and Windows:
//!
//! ```kotlin
//! @JvmStatic external fun convert(input: String, fileName: String, seed: String, output: String): String
//! ```
//!
//! The symbol is named after that package, class and method, so renaming any of them on the
//! Kotlin side breaks the call at run time, not at build time.

use crate::app::{convert_to_bundle, failure};
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use serde_json::Value;
use std::any::Any;
use std::path::Path;

/// The parser recurses through a page's outlines, and JVM/ART worker threads can have small
/// stacks. The conversion gets a thread of its own, with room to spare on every platform.
const STACK_BYTES: usize = 64 << 20;

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_vivenotes_data_onenote_OneNoteConverter_convert<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    input: JString<'caller>,
    file_name: JString<'caller>,
    seed: JString<'caller>,
    output: JString<'caller>,
) -> JString<'caller> {
    env.with_env(|env| -> jni::errors::Result<_> {
        let input = input.try_to_string(env)?;
        let file_name = file_name.try_to_string(env)?;
        let seed = seed.try_to_string(env)?;
        let output = output.try_to_string(env)?;
        let report = std::thread::Builder::new()
            .name("vive-converter".to_owned())
            .stack_size(STACK_BYTES)
            .spawn(move || {
                convert_to_bundle(Path::new(&input), &file_name, &seed, Path::new(&output))
            })
            .map_err(|error| failure("failed", &format!("starting the converter: {error}")))
            .and_then(|thread| thread.join().map_err(panicked))
            .unwrap_or_else(|report| report);
        JString::from_str(env, report.to_string())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

fn panicked(payload: Box<dyn Any + Send>) -> Value {
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned());
    failure("failed", &format!("the converter panicked: {message}"))
}
