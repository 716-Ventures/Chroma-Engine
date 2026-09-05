#![allow(unsafe_code)]

use std::{ptr, slice, sync::OnceLock};

use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFTransform, MF_VERSION, MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup,
            MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
            MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
            MFVideoFormat_HEVC,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    },
    core::GUID,
};

use crate::transcode::VideoCodec;

#[derive(Debug)]
struct CodecProbe {
    opened: bool,
    failure: String,
}

#[derive(Debug)]
pub(super) struct WindowsMediaFoundationProbe {
    h264_decoder: CodecProbe,
    hevc_decoder: CodecProbe,
    h264_encoder: CodecProbe,
    hevc_encoder: CodecProbe,
}

impl WindowsMediaFoundationProbe {
    pub(super) fn decoder_opened(&self, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H264 => self.h264_decoder.opened,
            VideoCodec::Hevc => self.hevc_decoder.opened,
            VideoCodec::Av1 => false,
        }
    }

    pub(super) fn decoder_failure(&self, codec: VideoCodec) -> &str {
        match codec {
            VideoCodec::H264 => &self.h264_decoder.failure,
            VideoCodec::Hevc => &self.hevc_decoder.failure,
            VideoCodec::Av1 => {
                "Media Foundation AV1 hardware decode is outside the current backend scope"
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn encoder_opened(&self, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H264 => self.h264_encoder.opened,
            VideoCodec::Hevc => self.hevc_encoder.opened,
            VideoCodec::Av1 => false,
        }
    }

    #[allow(dead_code)]
    pub(super) fn encoder_failure(&self, codec: VideoCodec) -> &str {
        match codec {
            VideoCodec::H264 => &self.h264_encoder.failure,
            VideoCodec::Hevc => &self.hevc_encoder.failure,
            VideoCodec::Av1 => {
                "Media Foundation AV1 hardware encode is outside the current backend scope"
            }
        }
    }
}

pub(super) fn probe() -> &'static WindowsMediaFoundationProbe {
    static PROBE: OnceLock<WindowsMediaFoundationProbe> = OnceLock::new();
    PROBE.get_or_init(probe_media_foundation)
}

fn probe_media_foundation() -> WindowsMediaFoundationProbe {
    // SAFETY: COM and Media Foundation initialization are balanced before this function returns.
    unsafe {
        let com_initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            if com_initialized {
                CoUninitialize();
            }
            let failure = format!("Media Foundation startup failed: {error}");
            return WindowsMediaFoundationProbe {
                h264_decoder: failed(&failure),
                hevc_decoder: failed(&failure),
                h264_encoder: failed(&failure),
                hevc_encoder: failed(&failure),
            };
        }

        let result = WindowsMediaFoundationProbe {
            h264_decoder: probe_transform(
                MFT_CATEGORY_VIDEO_DECODER,
                MFVideoFormat_H264,
                TransformDirection::Decoder,
            ),
            hevc_decoder: probe_transform(
                MFT_CATEGORY_VIDEO_DECODER,
                MFVideoFormat_HEVC,
                TransformDirection::Decoder,
            ),
            h264_encoder: probe_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_H264,
                TransformDirection::Encoder,
            ),
            hevc_encoder: probe_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFVideoFormat_HEVC,
                TransformDirection::Encoder,
            ),
        };
        let _ = MFShutdown();
        if com_initialized {
            CoUninitialize();
        }
        result
    }
}

#[derive(Clone, Copy)]
enum TransformDirection {
    Decoder,
    Encoder,
}

unsafe fn probe_transform(
    category: GUID,
    subtype: GUID,
    direction: TransformDirection,
) -> CodecProbe {
    let media_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let (input, output) = match direction {
        TransformDirection::Decoder => (Some(&raw const media_type), None),
        TransformDirection::Encoder => (None, Some(&raw const media_type)),
    };
    let mut activations = ptr::null_mut();
    let mut count = 0_u32;
    // SAFETY: Media Foundation is initialized and owns the returned activation array.
    let result = unsafe {
        MFTEnumEx(
            category,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            input,
            output,
            &raw mut activations,
            &raw mut count,
        )
    };
    if let Err(error) = result {
        return CodecProbe {
            opened: false,
            failure: format!("hardware Media Foundation transform enumeration failed: {error}"),
        };
    }
    if activations.is_null() || count == 0 {
        if !activations.is_null() {
            // SAFETY: MFTEnumEx allocated this array with the COM task allocator.
            unsafe { CoTaskMemFree(Some(activations.cast())) };
        }
        return CodecProbe {
            opened: false,
            failure: "no matching hardware Media Foundation transform was registered".to_string(),
        };
    }

    // SAFETY: MFTEnumEx returned `count` initialized activation slots.
    let activation_slice = unsafe { slice::from_raw_parts_mut(activations, count as usize) };
    let mut activation_error = None;
    let mut opened = false;
    for activation in activation_slice.iter_mut() {
        if let Some(activation) = activation.take() {
            // SAFETY: The activation object came from MFTEnumEx for this transform category.
            match unsafe { activation.ActivateObject::<IMFTransform>() } {
                Ok(transform) => {
                    drop(transform);
                    opened = true;
                    break;
                }
                Err(error) => activation_error = Some(error.to_string()),
            }
        }
    }
    // Drop any activation references after the first successful one.
    for activation in activation_slice.iter_mut() {
        let _ = activation.take();
    }
    // SAFETY: MFTEnumEx allocated this array with the COM task allocator.
    unsafe { CoTaskMemFree(Some(activations.cast())) };

    CodecProbe {
        opened,
        failure: if opened {
            String::new()
        } else {
            format!(
                "hardware Media Foundation transform activation failed: {}",
                activation_error.unwrap_or_else(|| "no activation object was returned".to_string())
            )
        },
    }
}

fn failed(reason: &str) -> CodecProbe {
    CodecProbe {
        opened: false,
        failure: reason.to_string(),
    }
}
