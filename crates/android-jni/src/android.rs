//! Android NDK glue: `ANativeWindow` presentation, AAudio low-latency
//! output and logcat. Compiled only for `target_os = "android"`.

use kd_core::audio::AudioConsumer;
use kd_core::{FrameView, StereoFrame};
use std::ffi::{c_void, CString};

#[repr(C)]
pub struct ANativeWindow {
    _private: [u8; 0],
}

#[repr(C)]
struct ANativeWindowBuffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
}

const WINDOW_FORMAT_RGBA_8888: i32 = 1;

#[link(name = "android")]
extern "C" {
    fn ANativeWindow_fromSurface(env: *mut jni::sys::JNIEnv, surface: jni::sys::jobject) -> *mut ANativeWindow;
    fn ANativeWindow_release(window: *mut ANativeWindow);
    fn ANativeWindow_setBuffersGeometry(window: *mut ANativeWindow, w: i32, h: i32, format: i32) -> i32;
    fn ANativeWindow_lock(window: *mut ANativeWindow, out: *mut ANativeWindowBuffer, dirty: *mut c_void) -> i32;
    fn ANativeWindow_unlockAndPost(window: *mut ANativeWindow) -> i32;
}

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

pub fn log(text: &str) {
    let tag = c"KD-Emulator";
    if let Ok(msg) = CString::new(text) {
        // SAFETY: valid C strings.
        unsafe { __android_log_write(4, tag.as_ptr(), msg.as_ptr()) };
    }
}

/// An owned `ANativeWindow` reference.
pub struct NativeWindow {
    ptr: *mut ANativeWindow,
    size: (i32, i32),
}

// SAFETY: ANativeWindow is reference counted and thread-safe for
// lock/unlockAndPost from a single producer thread.
unsafe impl Send for NativeWindow {}

impl NativeWindow {
    /// # Safety
    /// `env` and `surface` must be valid JNI handles for this thread.
    pub unsafe fn from_surface(env: *mut jni::sys::JNIEnv, surface: jni::sys::jobject) -> Option<Self> {
        let ptr = ANativeWindow_fromSurface(env, surface);
        (!ptr.is_null()).then_some(Self { ptr, size: (0, 0) })
    }

    /// Copy an RGBA8888 frame into the window.
    pub fn present(&mut self, frame: &FrameView<'_>) {
        let (w, h) = (frame.width as i32, frame.height as i32);
        unsafe {
            if self.size != (w, h) {
                ANativeWindow_setBuffersGeometry(self.ptr, w, h, WINDOW_FORMAT_RGBA_8888);
                self.size = (w, h);
            }
            let mut buf = std::mem::zeroed::<ANativeWindowBuffer>();
            if ANativeWindow_lock(self.ptr, &mut buf, std::ptr::null_mut()) != 0 {
                return;
            }
            let dst = buf.bits as *mut u32;
            let rows = h.min(buf.height) as usize;
            let cols = w.min(buf.width) as usize;
            for y in 0..rows {
                let src = &frame.pixels[y * frame.width as usize..][..cols];
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst.add(y * buf.stride as usize), cols);
            }
            ANativeWindow_unlockAndPost(self.ptr);
        }
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        unsafe { ANativeWindow_release(self.ptr) };
    }
}

// ---------------------------------------------------------------- AAudio

type AAudioStreamBuilder = c_void;
type AAudioStream = c_void;
type DataCallback = extern "C" fn(*mut AAudioStream, *mut c_void, *mut c_void, i32) -> i32;

const AAUDIO_FORMAT_PCM_I16: i32 = 1;
const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;
const AAUDIO_CALLBACK_RESULT_CONTINUE: i32 = 0;

#[link(name = "aaudio")]
extern "C" {
    fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> i32;
    fn AAudioStreamBuilder_setFormat(b: *mut AAudioStreamBuilder, format: i32);
    fn AAudioStreamBuilder_setChannelCount(b: *mut AAudioStreamBuilder, count: i32);
    fn AAudioStreamBuilder_setSampleRate(b: *mut AAudioStreamBuilder, rate: i32);
    fn AAudioStreamBuilder_setPerformanceMode(b: *mut AAudioStreamBuilder, mode: i32);
    fn AAudioStreamBuilder_setDataCallback(b: *mut AAudioStreamBuilder, cb: DataCallback, user: *mut c_void);
    fn AAudioStreamBuilder_openStream(b: *mut AAudioStreamBuilder, stream: *mut *mut AAudioStream) -> i32;
    fn AAudioStreamBuilder_delete(b: *mut AAudioStreamBuilder) -> i32;
    fn AAudioStream_requestStart(s: *mut AAudioStream) -> i32;
    fn AAudioStream_requestStop(s: *mut AAudioStream) -> i32;
    fn AAudioStream_close(s: *mut AAudioStream) -> i32;
}

extern "C" fn audio_callback(_stream: *mut AAudioStream, user: *mut c_void, data: *mut c_void, frames: i32) -> i32 {
    // SAFETY: `user` is the boxed consumer owned by AudioStream, `data`
    // holds `frames` interleaved stereo i16 frames.
    unsafe {
        let consumer = &mut *(user as *mut AudioConsumer);
        let out = std::slice::from_raw_parts_mut(data as *mut StereoFrame, frames as usize);
        consumer.pop_into(out);
    }
    AAUDIO_CALLBACK_RESULT_CONTINUE
}

/// A running low-latency AAudio output stream.
pub struct AudioStream {
    stream: *mut AAudioStream,
    _consumer: Box<AudioConsumer>,
}

unsafe impl Send for AudioStream {}

impl AudioStream {
    pub fn start(consumer: AudioConsumer, rate: u32) -> Option<Self> {
        let mut consumer = Box::new(consumer);
        unsafe {
            let mut builder = std::ptr::null_mut();
            if AAudio_createStreamBuilder(&mut builder) != 0 {
                return None;
            }
            AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
            AAudioStreamBuilder_setChannelCount(builder, 2);
            AAudioStreamBuilder_setSampleRate(builder, rate as i32);
            AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
            AAudioStreamBuilder_setDataCallback(
                builder,
                audio_callback,
                &mut *consumer as *mut AudioConsumer as *mut c_void,
            );
            let mut stream = std::ptr::null_mut();
            let ok = AAudioStreamBuilder_openStream(builder, &mut stream) == 0;
            AAudioStreamBuilder_delete(builder);
            if !ok {
                return None;
            }
            AAudioStream_requestStart(stream);
            Some(Self { stream, _consumer: consumer })
        }
    }
}

impl Drop for AudioStream {
    fn drop(&mut self) {
        unsafe {
            AAudioStream_requestStop(self.stream);
            AAudioStream_close(self.stream);
        }
    }
}
