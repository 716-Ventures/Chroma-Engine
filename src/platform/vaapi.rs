#![allow(unsafe_code)]

use std::{
    ffi::{CStr, c_char, c_int, c_void},
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use libloading::Library;

use crate::transcode::VideoCodec;

const VA_STATUS_SUCCESS: c_int = 0;
const VA_ENTRYPOINT_VLD: c_int = 1;
const VA_PROFILE_H264_BASELINE: c_int = 5;
const VA_PROFILE_H264_MAIN: c_int = 6;
const VA_PROFILE_H264_HIGH: c_int = 7;
const VA_PROFILE_H264_CONSTRAINED_BASELINE: c_int = 13;
const VA_PROFILE_HEVC_MAIN: c_int = 17;
const VA_PROFILE_HEVC_MAIN10: c_int = 18;
const VA_CONFIG_ATTRIB_RT_FORMAT: c_int = 0;
const VA_RT_FORMAT_YUV420: u32 = 0x0000_0001;
const VA_RT_FORMAT_YUV420_10: u32 = 0x0000_0100;
const VA_PROGRESSIVE: c_int = 0x1;
const PROBE_SURFACE_WIDTH: u32 = 64;
const PROBE_SURFACE_HEIGHT: u32 = 64;
const MAX_REPORTED_PROFILES: c_int = 1_024;
const MAX_REPORTED_ENTRYPOINTS: c_int = 1_024;

type VaDisplay = *mut c_void;
type VaGetDisplayDrm = unsafe extern "C" fn(c_int) -> VaDisplay;
type VaInitialize = unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> c_int;
type VaTerminate = unsafe extern "C" fn(VaDisplay) -> c_int;
type VaMaxNumProfiles = unsafe extern "C" fn(VaDisplay) -> c_int;
type VaQueryConfigProfiles = unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> c_int;
type VaMaxNumEntrypoints = unsafe extern "C" fn(VaDisplay) -> c_int;
type VaQueryConfigEntrypoints =
    unsafe extern "C" fn(VaDisplay, c_int, *mut c_int, *mut c_int) -> c_int;
type VaQueryVendorString = unsafe extern "C" fn(VaDisplay) -> *const c_char;
type VaCreateConfig =
    unsafe extern "C" fn(VaDisplay, c_int, c_int, *mut VaConfigAttrib, c_int, *mut u32) -> c_int;
type VaDestroyConfig = unsafe extern "C" fn(VaDisplay, u32) -> c_int;
type VaCreateSurfaces =
    unsafe extern "C" fn(VaDisplay, u32, u32, u32, *mut u32, u32, *mut c_void, u32) -> c_int;
type VaDestroySurfaces = unsafe extern "C" fn(VaDisplay, *mut u32, c_int) -> c_int;
type VaCreateContext =
    unsafe extern "C" fn(VaDisplay, u32, c_int, c_int, c_int, *mut u32, c_int, *mut u32) -> c_int;
type VaDestroyContext = unsafe extern "C" fn(VaDisplay, u32) -> c_int;

#[repr(C)]
struct VaConfigAttrib {
    type_: c_int,
    value: u32,
}

#[derive(Debug)]
pub(super) struct VaapiDecodeProbe {
    pub h264: Option<VaapiCodecDevice>,
    pub hevc: Option<VaapiCodecDevice>,
    pub h264_failure: Option<String>,
    pub hevc_failure: Option<String>,
    pub failure: Option<String>,
}

#[derive(Debug)]
pub(super) struct VaapiCodecDevice {
    path: PathBuf,
    vendor: Option<String>,
}

impl VaapiDecodeProbe {
    pub fn supports(&self, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H264 => self.h264.is_some(),
            VideoCodec::Hevc => self.hevc.is_some(),
            VideoCodec::Av1 => false,
        }
    }

    pub fn unavailable_reason(&self, codec: VideoCodec) -> String {
        let codec_device = match codec {
            VideoCodec::H264 => self.h264.as_ref(),
            VideoCodec::Hevc => self.hevc.as_ref(),
            VideoCodec::Av1 => None,
        };
        if codec_device.is_none() && self.failure.is_some() {
            return self.failure.clone().unwrap_or_else(|| {
                "no accessible VA-API render node was found under /dev/dri".to_string()
            });
        }

        let codec_failure = match codec {
            VideoCodec::H264 => self.h264_failure.as_deref(),
            VideoCodec::Hevc => self.hevc_failure.as_deref(),
            VideoCodec::Av1 => None,
        };
        if codec_device.is_none()
            && let Some(failure) = codec_failure
        {
            return failure.to_string();
        }

        let device = codec_device.map_or_else(
            || "unknown device".to_string(),
            |device| device.path.display().to_string(),
        );
        let vendor = codec_device
            .and_then(|device| device.vendor.as_deref())
            .map_or_else(String::new, |vendor| format!(" ({vendor})"));
        if self.supports(codec) {
            return format!(
                "VA-API initialized a {codec:?} VLD context on {device}{vendor}, but packet decode is not executable in this build"
            );
        }
        format!(
            "VA-API opened {device}{vendor}, but the driver did not report a {codec:?} VLD profile"
        )
    }
}

pub(super) fn decode_probe() -> &'static VaapiDecodeProbe {
    static PROBE: OnceLock<VaapiDecodeProbe> = OnceLock::new();
    PROBE.get_or_init(probe_decode_support)
}

fn probe_decode_support() -> VaapiDecodeProbe {
    let api = match VaApi::load() {
        Ok(api) => api,
        Err(error) => return failed_probe(error),
    };
    let devices = render_nodes();
    if devices.is_empty() {
        return failed_probe("no DRM render nodes were found under /dev/dri".to_string());
    }

    let mut h264 = None;
    let mut hevc = None;
    let mut h264_failure = None;
    let mut hevc_failure = None;
    let mut initialized_device = false;
    let mut last_failure = None;
    for device in devices {
        match api.probe_device(&device) {
            Ok(device_probe) => {
                initialized_device = true;
                if device_probe.h264_vld && h264.is_none() {
                    h264 = Some(VaapiCodecDevice {
                        path: device.clone(),
                        vendor: device_probe.vendor.clone(),
                    });
                    h264_failure = None;
                } else if h264.is_none()
                    && let Some(failure) = device_probe.h264_failure
                {
                    h264_failure = Some(failure);
                }
                if device_probe.hevc_vld && hevc.is_none() {
                    hevc = Some(VaapiCodecDevice {
                        path: device,
                        vendor: device_probe.vendor,
                    });
                    hevc_failure = None;
                } else if hevc.is_none()
                    && let Some(failure) = device_probe.hevc_failure
                {
                    hevc_failure = Some(failure);
                }
            }
            Err(error) => last_failure = Some(error),
        }
    }

    if initialized_device {
        VaapiDecodeProbe {
            h264,
            hevc,
            h264_failure,
            hevc_failure,
            failure: None,
        }
    } else {
        failed_probe(last_failure.unwrap_or_else(|| "VA-API initialization failed".to_string()))
    }
}

fn failed_probe(failure: String) -> VaapiDecodeProbe {
    VaapiDecodeProbe {
        h264: None,
        hevc: None,
        h264_failure: None,
        hevc_failure: None,
        failure: Some(failure),
    }
}

fn render_nodes() -> Vec<PathBuf> {
    let mut devices = std::fs::read_dir("/dev/dri")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    devices.sort_unstable();
    devices
}

struct VaApi {
    _core: Library,
    _drm: Library,
    get_display_drm: VaGetDisplayDrm,
    initialize: VaInitialize,
    terminate: VaTerminate,
    max_num_profiles: VaMaxNumProfiles,
    query_config_profiles: VaQueryConfigProfiles,
    max_num_entrypoints: VaMaxNumEntrypoints,
    query_config_entrypoints: VaQueryConfigEntrypoints,
    query_vendor_string: VaQueryVendorString,
    create_config: VaCreateConfig,
    destroy_config: VaDestroyConfig,
    create_surfaces: VaCreateSurfaces,
    destroy_surfaces: VaDestroySurfaces,
    create_context: VaCreateContext,
    destroy_context: VaDestroyContext,
}

struct DeviceProbe {
    vendor: Option<String>,
    h264_vld: bool,
    hevc_vld: bool,
    h264_failure: Option<String>,
    hevc_failure: Option<String>,
}

impl VaApi {
    fn load() -> Result<Self, String> {
        // SAFETY: The libraries remain owned by `Self` for at least as long as every copied
        // function pointer. Each symbol is loaded with the signature declared by libva's stable C
        // ABI, and no call occurs until both libraries and all symbols have loaded successfully.
        unsafe {
            let core = load_library(&["libva.so.2", "libva.so"])?;
            let drm = load_library(&["libva-drm.so.2", "libva-drm.so"])?;
            Ok(Self {
                get_display_drm: load_symbol(&drm, b"vaGetDisplayDRM\0")?,
                initialize: load_symbol(&core, b"vaInitialize\0")?,
                terminate: load_symbol(&core, b"vaTerminate\0")?,
                max_num_profiles: load_symbol(&core, b"vaMaxNumProfiles\0")?,
                query_config_profiles: load_symbol(&core, b"vaQueryConfigProfiles\0")?,
                max_num_entrypoints: load_symbol(&core, b"vaMaxNumEntrypoints\0")?,
                query_config_entrypoints: load_symbol(&core, b"vaQueryConfigEntrypoints\0")?,
                query_vendor_string: load_symbol(&core, b"vaQueryVendorString\0")?,
                create_config: load_symbol(&core, b"vaCreateConfig\0")?,
                destroy_config: load_symbol(&core, b"vaDestroyConfig\0")?,
                create_surfaces: load_symbol(&core, b"vaCreateSurfaces\0")?,
                destroy_surfaces: load_symbol(&core, b"vaDestroySurfaces\0")?,
                create_context: load_symbol(&core, b"vaCreateContext\0")?,
                destroy_context: load_symbol(&core, b"vaDestroyContext\0")?,
                _core: core,
                _drm: drm,
            })
        }
    }

    fn probe_device(&self, path: &Path) -> Result<DeviceProbe, String> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;

        // SAFETY: `file` remains open for the entire display lifetime. The returned display is
        // checked for null and initialized before queries. `vaTerminate` runs before `file` drops.
        unsafe {
            let display = (self.get_display_drm)(file.as_raw_fd());
            if display.is_null() {
                return Err(format!("vaGetDisplayDRM rejected {}", path.display()));
            }
            let mut major = 0;
            let mut minor = 0;
            let status = (self.initialize)(display, &mut major, &mut minor);
            if status != VA_STATUS_SUCCESS {
                return Err(format!(
                    "vaInitialize failed for {} with status {status}",
                    path.display()
                ));
            }

            let result = self.query_device(display, path);
            let terminate_status = (self.terminate)(display);
            if terminate_status != VA_STATUS_SUCCESS {
                return Err(format!(
                    "vaTerminate failed for {} with status {terminate_status}",
                    path.display()
                ));
            }
            result
        }
    }

    unsafe fn query_device(&self, display: VaDisplay, path: &Path) -> Result<DeviceProbe, String> {
        // SAFETY: The caller supplies an initialized display and all output buffers below are
        // allocated to the maximum sizes reported by that same display.
        unsafe {
            let max_profiles = (self.max_num_profiles)(display);
            if !(1..=MAX_REPORTED_PROFILES).contains(&max_profiles) {
                return Err(format!(
                    "{} reported an invalid VA-API profile capacity of {max_profiles}",
                    path.display()
                ));
            }
            let mut profiles = vec![0; max_profiles as usize];
            let mut profile_count = 0;
            let status =
                (self.query_config_profiles)(display, profiles.as_mut_ptr(), &mut profile_count);
            if status != VA_STATUS_SUCCESS || !(0..=max_profiles).contains(&profile_count) {
                return Err(format!(
                    "vaQueryConfigProfiles failed for {} with status {status}",
                    path.display()
                ));
            }
            profiles.truncate(profile_count as usize);

            let h264_profile = profiles.iter().copied().find(|profile| {
                matches!(
                    *profile,
                    VA_PROFILE_H264_BASELINE
                        | VA_PROFILE_H264_MAIN
                        | VA_PROFILE_H264_HIGH
                        | VA_PROFILE_H264_CONSTRAINED_BASELINE
                ) && self.has_vld_entrypoint(display, *profile)
            });
            let hevc_profile = profiles.iter().copied().find(|profile| {
                matches!(*profile, VA_PROFILE_HEVC_MAIN | VA_PROFILE_HEVC_MAIN10)
                    && self.has_vld_entrypoint(display, *profile)
            });
            let (h264_vld, h264_failure) =
                self.probe_decode_context(display, path, "H264", h264_profile, VA_RT_FORMAT_YUV420);
            let hevc_rt_format = if hevc_profile == Some(VA_PROFILE_HEVC_MAIN10) {
                VA_RT_FORMAT_YUV420_10
            } else {
                VA_RT_FORMAT_YUV420
            };
            let (hevc_vld, hevc_failure) =
                self.probe_decode_context(display, path, "HEVC", hevc_profile, hevc_rt_format);
            let vendor_ptr = (self.query_vendor_string)(display);
            let vendor = (!vendor_ptr.is_null())
                .then(|| CStr::from_ptr(vendor_ptr).to_string_lossy().into_owned());
            Ok(DeviceProbe {
                vendor,
                h264_vld,
                hevc_vld,
                h264_failure,
                hevc_failure,
            })
        }
    }

    unsafe fn probe_decode_context(
        &self,
        display: VaDisplay,
        path: &Path,
        codec: &str,
        profile: Option<c_int>,
        rt_format: u32,
    ) -> (bool, Option<String>) {
        let Some(profile) = profile else {
            return (
                false,
                Some(format!(
                    "{} did not report a {codec} VLD profile",
                    path.display()
                )),
            );
        };

        // SAFETY: The display is initialized. Each ID is used only after successful creation and
        // every successfully created VA object is destroyed in reverse dependency order.
        unsafe {
            let mut attribute = VaConfigAttrib {
                type_: VA_CONFIG_ATTRIB_RT_FORMAT,
                value: rt_format,
            };
            let mut config = 0;
            let status = (self.create_config)(
                display,
                profile,
                VA_ENTRYPOINT_VLD,
                &mut attribute,
                1,
                &mut config,
            );
            if status != VA_STATUS_SUCCESS {
                return (
                    false,
                    Some(format!(
                        "{codec} VA-API config creation failed on {} with status {status}",
                        path.display()
                    )),
                );
            }

            let mut surface = 0;
            let surface_status = (self.create_surfaces)(
                display,
                rt_format,
                PROBE_SURFACE_WIDTH,
                PROBE_SURFACE_HEIGHT,
                &mut surface,
                1,
                std::ptr::null_mut(),
                0,
            );
            if surface_status != VA_STATUS_SUCCESS {
                let _ = (self.destroy_config)(display, config);
                return (
                    false,
                    Some(format!(
                        "{codec} VA-API surface allocation failed on {} with status {surface_status}",
                        path.display()
                    )),
                );
            }

            let mut context = 0;
            let context_status = (self.create_context)(
                display,
                config,
                PROBE_SURFACE_WIDTH as c_int,
                PROBE_SURFACE_HEIGHT as c_int,
                VA_PROGRESSIVE,
                &mut surface,
                1,
                &mut context,
            );
            if context_status != VA_STATUS_SUCCESS {
                let _ = (self.destroy_surfaces)(display, &mut surface, 1);
                let _ = (self.destroy_config)(display, config);
                return (
                    false,
                    Some(format!(
                        "{codec} VA-API context creation failed on {} with status {context_status}",
                        path.display()
                    )),
                );
            }

            let destroy_context_status = (self.destroy_context)(display, context);
            let destroy_surface_status = (self.destroy_surfaces)(display, &mut surface, 1);
            let destroy_config_status = (self.destroy_config)(display, config);
            if destroy_context_status != VA_STATUS_SUCCESS
                || destroy_surface_status != VA_STATUS_SUCCESS
                || destroy_config_status != VA_STATUS_SUCCESS
            {
                return (
                    false,
                    Some(format!(
                        "{codec} VA-API probe cleanup failed on {} (context {destroy_context_status}, surface {destroy_surface_status}, config {destroy_config_status})",
                        path.display()
                    )),
                );
            }
            (true, None)
        }
    }

    unsafe fn has_vld_entrypoint(&self, display: VaDisplay, profile: c_int) -> bool {
        // SAFETY: The display is initialized and the buffer is sized from vaMaxNumEntrypoints.
        unsafe {
            let max_entrypoints = (self.max_num_entrypoints)(display);
            if !(1..=MAX_REPORTED_ENTRYPOINTS).contains(&max_entrypoints) {
                return false;
            }
            let mut entrypoints = vec![0; max_entrypoints as usize];
            let mut entrypoint_count = 0;
            let status = (self.query_config_entrypoints)(
                display,
                profile,
                entrypoints.as_mut_ptr(),
                &mut entrypoint_count,
            );
            if status != VA_STATUS_SUCCESS || !(0..=max_entrypoints).contains(&entrypoint_count) {
                return false;
            }
            entrypoints[..entrypoint_count as usize].contains(&VA_ENTRYPOINT_VLD)
        }
    }
}

unsafe fn load_library(names: &[&str]) -> Result<Library, String> {
    let mut errors = Vec::new();
    for name in names {
        // SAFETY: Callers retain the returned library and load only known libva ABI symbols.
        match unsafe { Library::new(name) } {
            Ok(library) => return Ok(library),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(format!(
        "could not load VA-API runtime ({})",
        errors.join("; ")
    ))
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    // SAFETY: The requested types exactly match the named functions in libva's stable C ABI.
    unsafe { library.get::<T>(name) }
        .map(|symbol| *symbol)
        .map_err(|error| {
            format!(
                "could not load VA-API symbol {}: {error}",
                String::from_utf8_lossy(name).trim_end_matches('\0')
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialized_context_is_opened_but_not_claimed_executable() {
        let probe = VaapiDecodeProbe {
            h264: Some(VaapiCodecDevice {
                path: PathBuf::from("/dev/dri/renderD128"),
                vendor: Some("Test VA driver".to_string()),
            }),
            hevc: None,
            h264_failure: None,
            hevc_failure: Some("HEVC profile unavailable".to_string()),
            failure: None,
        };

        assert!(probe.supports(VideoCodec::H264));
        assert!(!probe.supports(VideoCodec::Hevc));
        assert_eq!(
            probe.unavailable_reason(VideoCodec::H264),
            "VA-API initialized a H264 VLD context on /dev/dri/renderD128 (Test VA driver), but packet decode is not executable in this build"
        );
        assert_eq!(
            probe.unavailable_reason(VideoCodec::Hevc),
            "HEVC profile unavailable"
        );
    }

    #[test]
    fn loader_failure_is_preserved_for_each_codec() {
        let probe = failed_probe("libva runtime unavailable".to_string());

        assert_eq!(
            probe.unavailable_reason(VideoCodec::H264),
            "libva runtime unavailable"
        );
        assert_eq!(
            probe.unavailable_reason(VideoCodec::Hevc),
            "libva runtime unavailable"
        );
    }
}
