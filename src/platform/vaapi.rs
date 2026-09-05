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

#[derive(Debug)]
pub(super) struct VaapiDecodeProbe {
    pub device: Option<PathBuf>,
    pub vendor: Option<String>,
    pub h264_vld: bool,
    pub hevc_vld: bool,
    pub failure: Option<String>,
}

impl VaapiDecodeProbe {
    pub fn supports(&self, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H264 => self.h264_vld,
            VideoCodec::Hevc => self.hevc_vld,
            VideoCodec::Av1 => false,
        }
    }

    pub fn unavailable_reason(&self, codec: VideoCodec) -> String {
        if self.device.is_none() {
            return self.failure.clone().unwrap_or_else(|| {
                "no accessible VA-API render node was found under /dev/dri".to_string()
            });
        }

        let device = self.device.as_ref().map_or_else(
            || "unknown device".to_string(),
            |path| path.display().to_string(),
        );
        let vendor = self
            .vendor
            .as_deref()
            .map_or_else(String::new, |vendor| format!(" ({vendor})"));
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

    let mut last_failure = None;
    for device in devices {
        match api.probe_device(&device) {
            Ok((vendor, h264_vld, hevc_vld)) => {
                return VaapiDecodeProbe {
                    device: Some(device),
                    vendor,
                    h264_vld,
                    hevc_vld,
                    failure: None,
                };
            }
            Err(error) => last_failure = Some(error),
        }
    }

    failed_probe(last_failure.unwrap_or_else(|| "VA-API initialization failed".to_string()))
}

fn failed_probe(failure: String) -> VaapiDecodeProbe {
    VaapiDecodeProbe {
        device: None,
        vendor: None,
        h264_vld: false,
        hevc_vld: false,
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
                _core: core,
                _drm: drm,
            })
        }
    }

    fn probe_device(&self, path: &Path) -> Result<(Option<String>, bool, bool), String> {
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

    unsafe fn query_device(
        &self,
        display: VaDisplay,
        path: &Path,
    ) -> Result<(Option<String>, bool, bool), String> {
        // SAFETY: The caller supplies an initialized display and all output buffers below are
        // allocated to the maximum sizes reported by that same display.
        unsafe {
            let max_profiles = (self.max_num_profiles)(display);
            if max_profiles <= 0 {
                return Err(format!("{} reported no VA-API profiles", path.display()));
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

            let h264_vld = profiles.iter().copied().any(|profile| {
                matches!(
                    profile,
                    VA_PROFILE_H264_BASELINE
                        | VA_PROFILE_H264_MAIN
                        | VA_PROFILE_H264_HIGH
                        | VA_PROFILE_H264_CONSTRAINED_BASELINE
                ) && self.has_vld_entrypoint(display, profile)
            });
            let hevc_vld = profiles.iter().copied().any(|profile| {
                matches!(profile, VA_PROFILE_HEVC_MAIN | VA_PROFILE_HEVC_MAIN10)
                    && self.has_vld_entrypoint(display, profile)
            });
            let vendor_ptr = (self.query_vendor_string)(display);
            let vendor = (!vendor_ptr.is_null())
                .then(|| CStr::from_ptr(vendor_ptr).to_string_lossy().into_owned());
            Ok((vendor, h264_vld, hevc_vld))
        }
    }

    unsafe fn has_vld_entrypoint(&self, display: VaDisplay, profile: c_int) -> bool {
        // SAFETY: The display is initialized and the buffer is sized from vaMaxNumEntrypoints.
        unsafe {
            let max_entrypoints = (self.max_num_entrypoints)(display);
            if max_entrypoints <= 0 {
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
