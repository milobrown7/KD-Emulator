//! # android-jni (`libkd_emulator.so`)
//!
//! JNI surface for `com.kdemulator.bridge.NativeEmulatorBridge`:
//! loading games from Storage Access Framework file descriptors, library
//! metadata (titles, DS icons, GameCube banners), the emulation thread
//! (pinned to Prime/Mid cores), frame hand-off to the Kotlin `GameView`,
//! AAudio output, input, microphone, thermal status, save states, backup
//! memory, fast-forward, rewind, cheats, the mobile debugger and Animal
//! Crossing time travel.

pub mod session;

#[cfg(target_os = "android")]
mod android;

use jni::objects::{JByteArray, JByteBuffer, JClass, JIntArray, JShortArray, JString};
use jni::sys::{jboolean, jbyteArray, jfloat, jint, jlong, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use kd_core::input::button;
use kd_core::topology::ThreadRole;
use session::Session;
#[cfg(target_os = "android")]
use session::OUTPUT_RATE;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub(crate) fn log(msg: &str) {
    #[cfg(target_os = "android")]
    android::log(msg);
    #[cfg(not(target_os = "android"))]
    eprintln!("[kd-emulator] {msg}");
}

/// The newest finished frame, handed from the emulation thread to the UI.
#[derive(Default)]
pub struct FrameSlot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
    /// Increments on every published frame.
    pub seq: i64,
}

impl FrameSlot {
    fn publish(&mut self, f: &kd_core::FrameView<'_>) {
        self.width = f.width;
        self.height = f.height;
        self.pixels.clear();
        self.pixels.extend_from_slice(f.pixels);
        self.seq += 1;
    }
}

#[derive(Default)]
struct Runtime {
    session: Option<Arc<Mutex<Session>>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    frame: Arc<Mutex<FrameSlot>>,
    #[cfg(target_os = "android")]
    audio: Option<android::AudioStream>,
    cache_dir: String,
    emu_tid: Arc<AtomicI32>,
    last_frame_ns: Arc<AtomicI64>,
}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

fn with_runtime<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    let mut g = RUNTIME.lock().unwrap_or_else(|p| p.into_inner());
    f(g.get_or_insert_with(Runtime::default))
}

fn with_session<R>(default: R, f: impl FnOnce(&mut Session) -> R) -> R {
    let s = with_runtime(|rt| rt.session.clone());
    match s {
        Some(s) => f(&mut s.lock().unwrap_or_else(|p| p.into_inner())),
        None => default,
    }
}

/// Catch panics at the JNI boundary (unwinding into the JVM is UB).
fn guard<R>(default: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(e) => {
            let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned());
            log(&format!("native panic: {}", msg.unwrap_or_default()));
            default
        }
    }
}

fn new_jstring(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map(|j| j.into_raw()).unwrap_or(std::ptr::null_mut())
}

fn new_jbytes(env: &mut JNIEnv, b: &[u8]) -> jbyteArray {
    env.byte_array_from_slice(b).map(|a| a.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// Read a whole file from a (SAF-provided) descriptor without taking
/// ownership of it.
pub fn read_fd(fd: i32) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::FromRawFd;
    // SAFETY: dup gives us our own descriptor to close.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut f = unsafe { std::fs::File::from_raw_fd(dup) };
    f.seek(SeekFrom::Start(0))?;
    let mut data = Vec::with_capacity(f.metadata().map(|m| m.len() as usize).unwrap_or(0));
    f.read_to_end(&mut data)?;
    Ok(data)
}

/// Read up to `n` bytes from the start of a descriptor (library peeks).
pub fn read_fd_prefix(fd: i32, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    // SAFETY: pread into our own buffer; does not move the file offset.
    let got = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut libc::c_void, n, 0) };
    if got < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// Load from a descriptor: disc images are streamed, cartridges read whole.
fn load_fd(fd: i32, name: &str) -> jint {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".iso") || lower.ends_with(".gcm") {
        let prefix = read_fd_prefix(fd, 0x440).unwrap_or_default();
        if session::detect_system(name, &prefix) == Some(kd_core::System::GameCube) {
            use std::os::fd::FromRawFd;
            // SAFETY: dup gives the disc its own descriptor.
            let dup = unsafe { libc::dup(fd) };
            if dup < 0 {
                return -1;
            }
            let file = unsafe { std::fs::File::from_raw_fd(dup) };
            let core = core_gcn::disc::FileDisc::new(file)
                .map_err(|e| e.to_string())
                .and_then(|d| core_gcn::GameCube::boot_disc(Box::new(d)).map_err(|e| e.to_string()));
            return match core {
                Ok(gc) => install(name, Box::new(gc)),
                Err(e) => {
                    log(&format!("load failed: {e}"));
                    -1
                }
            };
        }
    }
    match read_fd(fd) {
        Ok(data) => load(name, data),
        Err(e) => {
            log(&format!("cannot read {name}: {e}"));
            -1
        }
    }
}

fn load(name: &str, data: Vec<u8>) -> jint {
    match session::create_core(name, data) {
        Ok(core) => install(name, core),
        Err(e) => {
            log(&format!("load failed: {e}"));
            -1
        }
    }
}

fn install(name: &str, core: Box<dyn kd_core::EmulatorCore>) -> jint {
    stop_thread();
    let system = core.system() as jint;
    let mut s = Session::new(core);
    let cache = with_runtime(|rt| rt.cache_dir.clone());
    s.gpu = detect_gpu(&cache);
    let consumer = s.take_audio_consumer();
    with_runtime(|rt| {
        rt.frame.lock().unwrap_or_else(|p| p.into_inner()).publish(&s.core.frame());
        rt.session = Some(Arc::new(Mutex::new(s)));
        #[cfg(target_os = "android")]
        {
            rt.audio = consumer.and_then(|c| android::AudioStream::start(c, OUTPUT_RATE));
        }
        #[cfg(not(target_os = "android"))]
        drop(consumer);
    });
    log(&format!("loaded {name} as system {system}"));
    system
}

/// Query the Vulkan vendor once (PowerVR DXT vs Mali) and keep the
/// pipeline cache in the app's cacheDir.
fn detect_gpu(cache_dir: &str) -> kd_core::thermal::GpuVendorProfile {
    let path = format!("{cache_dir}/pipeline_cache.bin");
    let blob = std::fs::read(&path).ok();
    match gpu_gx_vk::vulkan::VulkanContext::new(&[], &[], blob.as_deref(), 1, 1) {
        Ok(ctx) => {
            if let Ok(data) = ctx.save_pipeline_cache() {
                let _ = std::fs::write(&path, data);
            }
            log(&format!("GPU: {} → {}", ctx.device_name(), ctx.profile.vendor.name()));
            ctx.profile.vendor
        }
        Err(e) => {
            log(&format!("Vulkan unavailable ({e}); software GX"));
            kd_core::thermal::GpuVendorProfile::Generic
        }
    }
}

fn start_thread() {
    with_runtime(|rt| {
        let Some(session) = rt.session.clone() else {
            return;
        };
        if rt.thread.is_some() {
            return;
        }
        rt.running.store(true, Ordering::SeqCst);
        let running = rt.running.clone();
        let tid = rt.emu_tid.clone();
        let last_ns = rt.last_frame_ns.clone();
        let frame = rt.frame.clone();
        let handle = std::thread::Builder::new()
            .name("kd-emu".into())
            .spawn(move || {
                // SAFETY: gettid has no preconditions.
                tid.store(unsafe { libc::gettid() }, Ordering::SeqCst);
                let pinned = {
                    let s = session.lock().unwrap_or_else(|p| p.into_inner());
                    s.topology.pin_current_thread(ThreadRole::EmuCpu)
                };
                log(&format!("emulation thread pinned to {pinned:?}"));
                let mut deadline = Instant::now();
                while running.load(Ordering::SeqCst) {
                    let period = {
                        let mut s = session.lock().unwrap_or_else(|p| p.into_inner());
                        let t0 = Instant::now();
                        s.step();
                        frame.lock().unwrap_or_else(|p| p.into_inner()).publish(&s.core.frame());
                        last_ns.store(t0.elapsed().as_nanos() as i64, Ordering::Relaxed);
                        s.frame_period()
                    };
                    deadline += period;
                    let now = Instant::now();
                    if deadline > now {
                        std::thread::sleep(deadline - now);
                    } else if now - deadline > period * 4 {
                        deadline = now; // we fell far behind; don't spiral
                    }
                }
            })
            .expect("spawn emulation thread");
        rt.thread = Some(handle);
    });
}

fn stop_thread() {
    let handle = with_runtime(|rt| {
        rt.running.store(false, Ordering::SeqCst);
        rt.thread.take()
    });
    if let Some(h) = handle {
        let _ = h.join();
    }
}

// ============================================================ JNI exports

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeInit<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    cache_dir: JString<'l>,
) -> jstring {
    let dir: String = env.get_string(&cache_dir).map(|s| s.into()).unwrap_or_default();
    let summary = guard(String::new(), || {
        with_runtime(|rt| rt.cache_dir = dir.clone());
        let topo = kd_core::topology::CpuTopology::detect();
        let feat = kd_core::topology::CpuFeatures::detect();
        format!(
            "{} {} | page {} B | ARMv9.2 {}",
            topo.generation(),
            topo.layout(),
            kd_core::arena::host_page_size(),
            if feat.is_armv9_class() { "yes" } else { "no" }
        )
    });
    new_jstring(&mut env, &summary)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeLoadRomFd<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    fd: jint,
    name: JString<'l>,
) -> jint {
    let name: String = env.get_string(&name).map(|s| s.into()).unwrap_or_default();
    guard(-1, || load_fd(fd, &name))
}

/// Random-access reads from a descriptor (does not move its offset).
fn pread_exact(fd: i32, off: u64, buf: &mut [u8]) -> bool {
    let mut done = 0;
    while done < buf.len() {
        // SAFETY: pread into our own buffer.
        let got = unsafe {
            libc::pread(
                fd,
                buf[done..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - done,
                (off + done as u64) as libc::off_t,
            )
        };
        if got <= 0 {
            return false;
        }
        done += got as usize;
    }
    true
}

/// Library metadata as `key=value` lines plus the game's own artwork (if
/// any) as Android ARGB pixels; `icon_w`/`icon_h` give its size.
pub fn game_info_text(info: &session::GameInfo) -> (String, Vec<i32>) {
    let (w, h, argb) = match &info.icon {
        Some((w, h, px)) => {
            let argb = px
                .iter()
                .map(|&p| {
                    let (r, g, b, a) = (p & 0xFF, (p >> 8) & 0xFF, (p >> 16) & 0xFF, p >> 24);
                    (a << 24 | r << 16 | g << 8 | b) as i32
                })
                .collect();
            (*w, *h, argb)
        }
        None => (0, 0, Vec::new()),
    };
    let text = format!(
        "system={}\ncode={}\ntitle={}\nac={}\nicon_w={w}\nicon_h={h}\n",
        info.system as u8,
        info.code,
        info.title.replace('\n', " "),
        info.animal_crossing as u8
    );
    (text, argb)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGameInfoFd<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    fd: jint,
    name: JString<'l>,
    icon_out: JIntArray<'l>,
) -> jstring {
    let name: String = env.get_string(&name).map(|s| s.into()).unwrap_or_default();
    let (text, argb) = guard((String::new(), Vec::new()), || {
        let mut read = |off: u64, buf: &mut [u8]| pread_exact(fd, off, buf);
        session::game_info(&name, &mut read).map(|i| game_info_text(&i)).unwrap_or_default()
    });
    if !argb.is_empty() && !icon_out.is_null() {
        let cap = env.get_array_length(&icon_out).unwrap_or(0) as usize;
        if cap >= argb.len() {
            let _ = env.set_int_array_region(&icon_out, 0, &argb);
        }
    }
    new_jstring(&mut env, &text)
}

/// Frame dimensions packed as `width << 16 | height` (0 before any frame).
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetFrameSize(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    with_runtime(|rt| {
        let f = rt.frame.lock().unwrap_or_else(|p| p.into_inner());
        (f.width << 16 | f.height) as jint
    })
}

/// Copy the newest frame (RGBA8888) into a direct buffer if it is newer
/// than `last_seq`. Returns the frame's sequence number, or -1 if the
/// buffer is too small.
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeCopyFrame<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    buffer: JByteBuffer<'l>,
    last_seq: jlong,
) -> jlong {
    guard(-1, || {
        let Ok(ptr) = env.get_direct_buffer_address(&buffer) else { return -1 };
        let cap = env.get_direct_buffer_capacity(&buffer).unwrap_or(0);
        let slot = with_runtime(|rt| rt.frame.clone());
        let f = slot.lock().unwrap_or_else(|p| p.into_inner());
        if f.seq == last_seq {
            return f.seq;
        }
        let bytes = f.pixels.len() * 4;
        if cap < bytes {
            return -1;
        }
        // SAFETY: the direct buffer holds at least `bytes` bytes and does
        // not overlap the slot's Vec.
        unsafe { std::ptr::copy_nonoverlapping(f.pixels.as_ptr() as *const u8, ptr, bytes) };
        f.seq
    })
}

/// Run `n` frames synchronously while the emulation thread is paused
/// (frame advance; also used by instrumentation tests).
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeRunFrames(
    _env: JNIEnv,
    _class: JClass,
    n: jint,
) {
    guard((), || {
        if with_runtime(|rt| rt.thread.is_some()) {
            return;
        }
        let slot = with_runtime(|rt| rt.frame.clone());
        with_session((), |s| {
            for _ in 0..n.max(0) {
                s.step();
            }
            slot.lock().unwrap_or_else(|p| p.into_inner()).publish(&s.core.frame());
        })
    })
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetSpeed(
    _env: JNIEnv,
    _class: JClass,
    speed: jint,
) {
    guard((), || with_session((), |s| s.set_speed(speed.max(1) as u32)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetVolume(
    _env: JNIEnv,
    _class: JClass,
    volume: jint,
) {
    guard((), || with_session((), |s| s.volume = volume.clamp(0, 256) as u32))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetRewindInterval(
    _env: JNIEnv,
    _class: JClass,
    frames: jint,
) {
    guard((), || with_session((), |s| s.set_rewind_interval(frames.max(0) as u32)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetRewinding(
    _env: JNIEnv,
    _class: JClass,
    on: jboolean,
) {
    guard((), || with_session((), |s| s.set_rewinding(on != JNI_FALSE)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetRewindAvailable(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    guard(0, || with_session(0, |s| s.rewind_available() as jint))
}

/// Replace the active cheats (see `Session::set_cheats` for the format).
/// Returns the errors, one per line (empty when every cheat parsed).
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetCheats<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    spec: JString<'l>,
) -> jstring {
    let spec: String = env.get_string(&spec).map(|s| s.into()).unwrap_or_default();
    let errors = guard(Vec::new(), || with_session(Vec::new(), |s| s.set_cheats(&spec)));
    new_jstring(&mut env, &errors.join("\n"))
}

/// Check a cheat before saving it. Returns an error message, or "" if OK.
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeValidateCheat<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    format: JString<'l>,
    code: JString<'l>,
) -> jstring {
    let format: String = env.get_string(&format).map(|s| s.into()).unwrap_or_default();
    let code: String = env.get_string(&code).map(|s| s.into()).unwrap_or_default();
    let msg = guard("internal error".to_string(), || match kd_core::cheats::CheatFormat::from_id(&format) {
        Some(f) => kd_core::cheats::Cheat::parse(f, &code).err().unwrap_or_default(),
        None => format!("unknown format {format}"),
    });
    new_jstring(&mut env, &msg)
}

/// Host microphone samples (16-bit mono) for the DS.
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativePushMic<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    samples: JShortArray<'l>,
    count: jint,
) {
    let n = env.get_array_length(&samples).unwrap_or(0).min(count).max(0) as usize;
    let mut buf = vec![0i16; n];
    if env.get_short_array_region(&samples, 0, &mut buf).is_ok() {
        guard((), || with_session((), |s| s.core.push_mic(&buf)))
    }
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeStart(_env: JNIEnv, _class: JClass) {
    guard((), start_thread)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativePause(_env: JNIEnv, _class: JClass) {
    guard((), stop_thread)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeStop(_env: JNIEnv, _class: JClass) {
    guard((), || {
        stop_thread();
        with_runtime(|rt| {
            rt.session = None;
            #[cfg(target_os = "android")]
            {
                rt.audio = None;
            }
        });
    })
}

#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetInput(
    _env: JNIEnv,
    _class: JClass,
    buttons: jint,
    stick_x: jfloat,
    stick_y: jfloat,
    c_x: jfloat,
    c_y: jfloat,
    trigger_l: jfloat,
    trigger_r: jfloat,
    touch_x: jint,
    touch_y: jint,
    pointer_x: jfloat,
    pointer_y: jfloat,
    pointer_valid: jboolean,
    tilt_x: jfloat,
    tilt_y: jfloat,
    light: jint,
) {
    guard((), || {
        with_session((), |s| {
            let i = &mut s.input;
            i.buttons = buttons as u32;
            i.stick = (stick_x, stick_y);
            i.c_stick = (c_x, c_y);
            i.triggers = (trigger_l, trigger_r);
            i.touch = (touch_x >= 0 && touch_y >= 0).then_some((touch_x as u16, touch_y as u16));
            i.pointer = (pointer_valid != JNI_FALSE).then_some((pointer_x, pointer_y));
            i.tilt = (tilt_x, tilt_y);
            i.accel = [tilt_x, tilt_y, 1.0];
            i.light = light.clamp(0, 255) as u8;
        })
    })
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetThermalStatus(
    _env: JNIEnv,
    _class: JClass,
    status: jint,
) {
    guard((), || {
        let changed = with_session(false, |s| s.set_thermal_status(status));
        if changed {
            log(&format!("thermal status {status}: performance knobs updated"));
        }
    })
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSaveState(
    mut env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    let data = guard(Vec::new(), || with_session(Vec::new(), |s| s.core.save_state()));
    new_jbytes(&mut env, &data)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeLoadState<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    data: JByteArray<'l>,
) -> jboolean {
    let bytes = env.convert_byte_array(&data).unwrap_or_default();
    let ok = guard(false, || with_session(false, |s| s.core.load_state(&bytes).is_ok()));
    if ok {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetBackup(
    mut env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    let data =
        guard(Vec::new(), || with_session(Vec::new(), |s| s.core.backup().map(|b| b.to_vec()).unwrap_or_default()));
    new_jbytes(&mut env, &data)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeLoadBackup<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    data: JByteArray<'l>,
) {
    let bytes = env.convert_byte_array(&data).unwrap_or_default();
    guard((), || with_session((), |s| s.core.load_backup(&bytes)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeTimeTravel(
    _env: JNIEnv,
    _class: JClass,
    seconds: jlong,
) {
    guard((), || with_session((), |s| s.time_travel_by(seconds)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeSetClockOffset(
    _env: JNIEnv,
    _class: JClass,
    seconds: jlong,
) {
    guard((), || with_session((), |s| s.set_clock_offset(seconds)))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetClockOffset(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    guard(0, || with_session(0, |s| s.clock_offset()))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetAnimalCrossingInfo(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let s = guard(String::new(), || with_session(String::new(), |s| s.animal_crossing_info()));
    new_jstring(&mut env, &s)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetDebugInfo(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let s = guard(String::new(), || with_session("no game loaded\n".to_string(), |s| s.debug_info()));
    new_jstring(&mut env, &s)
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetEmuThreadTid(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    with_runtime(|rt| rt.emu_tid.load(Ordering::SeqCst))
}

#[no_mangle]
pub extern "system" fn Java_com_kdemulator_bridge_NativeEmulatorBridge_nativeGetLastFrameNanos(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    with_runtime(|rt| rt.last_frame_ns.load(Ordering::Relaxed))
}

/// Bit values shared with the Kotlin side (`NativeEmulatorBridge.Buttons`).
pub const BUTTON_BITS: [(&str, u32); 21] = [
    ("A", button::A),
    ("B", button::B),
    ("X", button::X),
    ("Y", button::Y),
    ("L", button::L),
    ("R", button::R),
    ("Z", button::Z),
    ("START", button::START),
    ("SELECT", button::SELECT),
    ("UP", button::UP),
    ("DOWN", button::DOWN),
    ("LEFT", button::LEFT),
    ("RIGHT", button::RIGHT),
    ("ONE", button::ONE),
    ("TWO", button::TWO),
    ("PLUS", button::PLUS),
    ("MINUS", button::MINUS),
    ("HOME", button::HOME),
    ("C", button::C),
    ("LID_CLOSED", button::LID_CLOSED),
    ("DEBUG", button::DEBUG),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_bits_match_kotlin_constants() {
        let kt = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../android/app/src/main/kotlin/com/kdemulator/bridge/NativeEmulatorBridge.kt"
        ))
        .expect("Kotlin bridge source");
        let kotlin: std::collections::HashMap<&str, u32> = kt
            .lines()
            .filter_map(|l| l.trim().strip_prefix("const val "))
            .filter_map(|l| {
                let (name, expr) = l.split_once(" = ")?;
                let shift = expr.trim().strip_prefix("1 shl ")?.parse::<u32>().ok()?;
                Some((name.trim(), 1u32 << shift))
            })
            .collect();
        assert_eq!(kotlin.len(), BUTTON_BITS.len(), "Kotlin Buttons and BUTTON_BITS list different buttons");
        for (name, bit) in BUTTON_BITS {
            assert_eq!(kotlin.get(name), Some(&bit), "button {name}");
        }
    }

    #[test]
    fn read_fd_reads_whole_file_without_closing_it() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join(format!("kd_fd_{}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"KD-Emulator").unwrap();
        let f = std::fs::File::open(&path).unwrap();
        assert_eq!(read_fd(f.as_raw_fd()).unwrap(), b"KD-Emulator");
        assert_eq!(read_fd(f.as_raw_fd()).unwrap(), b"KD-Emulator", "original fd still usable");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn runtime_load_start_stop_cycle() {
        let mut rom = vec![0u8; 0x200];
        rom[0..4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0xB2] = 0x96;
        assert_eq!(load("loop.gba", rom), kd_core::System::Gba as jint);
        start_thread();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let frames = with_session(0, |s| s.stats.frames);
        stop_thread();
        assert!(frames >= 2, "emulation thread produced {frames} frames");
        let (seq, w, h, len) = with_runtime(|rt| {
            let f = rt.frame.lock().unwrap();
            (f.seq, f.width, f.height, f.pixels.len())
        });
        assert!(seq as u64 >= frames, "every frame published to the UI slot ({seq} vs {frames})");
        assert_eq!((w, h, len), (240, 160, 240 * 160));
        assert!(with_runtime(|rt| rt.emu_tid.load(Ordering::SeqCst)) > 0);
        assert_eq!(load("nothing.txt", vec![1, 2, 3]), -1);
    }
}
