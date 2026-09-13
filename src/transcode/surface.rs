//! Owned native frame lifetime across asynchronous decode/encode. Core Video
//! retains the actual format, stride, depth and color attachments on the buffer.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct AppleVideoFrame {
    pub pts: crate::TimePoint,
    pub dts: crate::TimePoint,
    pub duration: crate::TimeDelta,
    pub format: super::RawVideoFormat,
    pub keyframe: bool,
    pub buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
// SAFETY: Ownership crosses the VideoToolbox callback boundary only after
// decoding has completed for this image. The buffer has its own CF retain and
// no borrowed CPU address, lock, or mutable plane slice. All transfer is through
// a mutex; the receiving thread only submits it read-only to VideoToolbox or
// locks it read-only for the BGRA adapter. Do not implement Sync: sharing this
// wrapper concurrently or adding mutable pixel access requires a new audit.
unsafe impl Send for AppleVideoFrame {}
