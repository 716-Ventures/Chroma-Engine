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
const VA_FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");
const VA_FOURCC_P010: u32 = u32::from_le_bytes(*b"P010");
const PROBE_SURFACE_WIDTH: u32 = 64;
const PROBE_SURFACE_HEIGHT: u32 = 64;
const MAX_MAPPED_IMAGE_BYTES: u32 = 256 * 1024 * 1024;
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
type VaSyncSurface = unsafe extern "C" fn(VaDisplay, u32) -> c_int;
type VaDeriveImage = unsafe extern "C" fn(VaDisplay, u32, *mut VaImage) -> c_int;
type VaDestroyImage = unsafe extern "C" fn(VaDisplay, u32) -> c_int;
type VaMapBuffer = unsafe extern "C" fn(VaDisplay, u32, *mut *mut c_void) -> c_int;
type VaUnmapBuffer = unsafe extern "C" fn(VaDisplay, u32) -> c_int;

#[repr(C)]
struct VaConfigAttrib {
    type_: c_int,
    value: u32,
}

#[derive(Default)]
#[repr(C)]
struct VaImageFormat {
    fourcc: u32,
    byte_order: u32,
    bits_per_pixel: u32,
    depth: u32,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    alpha_mask: u32,
    reserved: [u32; 4],
}

#[derive(Default)]
#[repr(C)]
struct VaImage {
    image_id: u32,
    format: VaImageFormat,
    buf: u32,
    width: u16,
    height: u16,
    data_size: u32,
    num_planes: u32,
    pitches: [u32; 3],
    offsets: [u32; 3],
    num_palette_entries: i32,
    entry_bytes: i32,
    component_order: [i8; 4],
    reserved: [u32; 4],
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
    mapped_surface_format: Option<&'static str>,
    mapping_failure: Option<String>,
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
            if let Some(format) = codec_device.and_then(|device| device.mapped_surface_format) {
                return format!(
                    "VA-API initialized a {codec:?} VLD context and mapped a {format} surface on {device}{vendor}, but packet decode is not executable in this build"
                );
            }
            if let Some(failure) = codec_device.and_then(|device| device.mapping_failure.as_deref())
            {
                return format!(
                    "VA-API initialized a {codec:?} VLD context on {device}{vendor}, but direct surface mapping failed ({failure}) and packet decode is not executable in this build"
                );
            }
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
                if device_probe.h264.context_ready && h264.is_none() {
                    h264 = Some(VaapiCodecDevice {
                        path: device.clone(),
                        vendor: device_probe.vendor.clone(),
                        mapped_surface_format: device_probe.h264.mapped_surface_format,
                        mapping_failure: device_probe.h264.mapping_failure.clone(),
                    });
                    h264_failure = None;
                } else if h264.is_none()
                    && let Some(failure) = device_probe.h264.failure
                {
                    h264_failure = Some(failure);
                }
                if device_probe.hevc.context_ready && hevc.is_none() {
                    hevc = Some(VaapiCodecDevice {
                        path: device,
                        vendor: device_probe.vendor,
                        mapped_surface_format: device_probe.hevc.mapped_surface_format,
                        mapping_failure: device_probe.hevc.mapping_failure,
                    });
                    hevc_failure = None;
                } else if hevc.is_none()
                    && let Some(failure) = device_probe.hevc.failure
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
    sync_surface: VaSyncSurface,
    derive_image: VaDeriveImage,
    destroy_image: VaDestroyImage,
    map_buffer: VaMapBuffer,
    unmap_buffer: VaUnmapBuffer,
}

struct DeviceProbe {
    vendor: Option<String>,
    h264: CodecProbe,
    hevc: CodecProbe,
}

struct CodecProbe {
    context_ready: bool,
    failure: Option<String>,
    mapped_surface_format: Option<&'static str>,
    mapping_failure: Option<String>,
}

#[derive(Clone, Copy)]
struct SurfaceExpectation {
    rt_format: u32,
    fourcc: u32,
    name: &'static str,
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
                sync_surface: load_symbol(&core, b"vaSyncSurface\0")?,
                derive_image: load_symbol(&core, b"vaDeriveImage\0")?,
                destroy_image: load_symbol(&core, b"vaDestroyImage\0")?,
                map_buffer: load_symbol(&core, b"vaMapBuffer\0")?,
                unmap_buffer: load_symbol(&core, b"vaUnmapBuffer\0")?,
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
            let h264 = self.probe_decode_context(
                display,
                path,
                "H264",
                h264_profile,
                SurfaceExpectation {
                    rt_format: VA_RT_FORMAT_YUV420,
                    fourcc: VA_FOURCC_NV12,
                    name: "NV12",
                },
            );
            let hevc_rt_format = if hevc_profile == Some(VA_PROFILE_HEVC_MAIN10) {
                VA_RT_FORMAT_YUV420_10
            } else {
                VA_RT_FORMAT_YUV420
            };
            let (hevc_fourcc, hevc_format) = if hevc_rt_format == VA_RT_FORMAT_YUV420_10 {
                (VA_FOURCC_P010, "P010")
            } else {
                (VA_FOURCC_NV12, "NV12")
            };
            let hevc = self.probe_decode_context(
                display,
                path,
                "HEVC",
                hevc_profile,
                SurfaceExpectation {
                    rt_format: hevc_rt_format,
                    fourcc: hevc_fourcc,
                    name: hevc_format,
                },
            );
            let vendor_ptr = (self.query_vendor_string)(display);
            let vendor = (!vendor_ptr.is_null())
                .then(|| CStr::from_ptr(vendor_ptr).to_string_lossy().into_owned());
            Ok(DeviceProbe { vendor, h264, hevc })
        }
    }

    unsafe fn probe_decode_context(
        &self,
        display: VaDisplay,
        path: &Path,
        codec: &str,
        profile: Option<c_int>,
        surface_expectation: SurfaceExpectation,
    ) -> CodecProbe {
        let Some(profile) = profile else {
            return CodecProbe {
                context_ready: false,
                failure: Some(format!(
                    "{} did not report a {codec} VLD profile",
                    path.display()
                )),
                mapped_surface_format: None,
                mapping_failure: None,
            };
        };

        // SAFETY: The display is initialized. Each ID is used only after successful creation and
        // every successfully created VA object is destroyed in reverse dependency order.
        unsafe {
            let mut attribute = VaConfigAttrib {
                type_: VA_CONFIG_ATTRIB_RT_FORMAT,
                value: surface_expectation.rt_format,
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
                return failed_codec_probe(format!(
                    "{codec} VA-API config creation failed on {} with status {status}",
                    path.display()
                ));
            }

            let mut surface = 0;
            let surface_status = (self.create_surfaces)(
                display,
                surface_expectation.rt_format,
                PROBE_SURFACE_WIDTH,
                PROBE_SURFACE_HEIGHT,
                &mut surface,
                1,
                std::ptr::null_mut(),
                0,
            );
            if surface_status != VA_STATUS_SUCCESS {
                let _ = (self.destroy_config)(display, config);
                return failed_codec_probe(format!(
                    "{codec} VA-API surface allocation failed on {} with status {surface_status}",
                    path.display()
                ));
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
                return failed_codec_probe(format!(
                    "{codec} VA-API context creation failed on {} with status {context_status}",
                    path.display()
                ));
            }

            let mapping = self.probe_surface_mapping(
                display,
                surface,
                surface_expectation.fourcc,
                surface_expectation.name,
            );

            let destroy_context_status = (self.destroy_context)(display, context);
            let destroy_surface_status = (self.destroy_surfaces)(display, &mut surface, 1);
            let destroy_config_status = (self.destroy_config)(display, config);
            if destroy_context_status != VA_STATUS_SUCCESS
                || destroy_surface_status != VA_STATUS_SUCCESS
                || destroy_config_status != VA_STATUS_SUCCESS
            {
                return failed_codec_probe(format!(
                    "{codec} VA-API probe cleanup failed on {} (context {destroy_context_status}, surface {destroy_surface_status}, config {destroy_config_status})",
                    path.display()
                ));
            }
            match mapping {
                Ok(format) => CodecProbe {
                    context_ready: true,
                    failure: None,
                    mapped_surface_format: Some(format),
                    mapping_failure: None,
                },
                Err(failure) => CodecProbe {
                    context_ready: true,
                    failure: None,
                    mapped_surface_format: None,
                    mapping_failure: Some(failure),
                },
            }
        }
    }

    unsafe fn probe_surface_mapping(
        &self,
        display: VaDisplay,
        surface: u32,
        expected_fourcc: u32,
        expected_format: &'static str,
    ) -> Result<&'static str, String> {
        // SAFETY: The display and surface belong to the active probe. A successfully derived
        // image is always unmapped (when necessary) and destroyed before this function returns.
        unsafe {
            let sync_status = (self.sync_surface)(display, surface);
            if sync_status != VA_STATUS_SUCCESS {
                return Err(format!("vaSyncSurface returned status {sync_status}"));
            }

            let mut image = VaImage::default();
            let derive_status = (self.derive_image)(display, surface, &mut image);
            if derive_status != VA_STATUS_SUCCESS {
                return Err(format!("vaDeriveImage returned status {derive_status}"));
            }

            let layout = validate_image_layout(&image, expected_fourcc, expected_format);
            let layout_valid = layout.is_ok();
            let mut address = std::ptr::null_mut();
            let map_status = if layout_valid {
                (self.map_buffer)(display, image.buf, &mut address)
            } else {
                VA_STATUS_SUCCESS
            };
            let mapping = layout.and_then(|()| {
                if map_status != VA_STATUS_SUCCESS {
                    Err(format!("vaMapBuffer returned status {map_status}"))
                } else if address.is_null() {
                    Err("vaMapBuffer returned a null address".to_string())
                } else {
                    Ok(expected_format)
                }
            });

            let unmap_status = if layout_valid && map_status == VA_STATUS_SUCCESS {
                (self.unmap_buffer)(display, image.buf)
            } else {
                VA_STATUS_SUCCESS
            };
            let destroy_status = (self.destroy_image)(display, image.image_id);
            if unmap_status != VA_STATUS_SUCCESS || destroy_status != VA_STATUS_SUCCESS {
                return Err(format!(
                    "surface image cleanup failed (unmap {unmap_status}, image {destroy_status})"
                ));
            }
            mapping
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

fn failed_codec_probe(failure: String) -> CodecProbe {
    CodecProbe {
        context_ready: false,
        failure: Some(failure),
        mapped_surface_format: None,
        mapping_failure: None,
    }
}

fn validate_image_layout(
    image: &VaImage,
    expected_fourcc: u32,
    expected_format: &str,
) -> Result<(), String> {
    if image.format.fourcc != expected_fourcc {
        return Err(format!(
            "vaDeriveImage returned fourcc 0x{:08x}, expected {expected_format}",
            image.format.fourcc
        ));
    }
    if image.num_planes < 2 || image.num_planes > 3 {
        return Err(format!(
            "vaDeriveImage returned invalid plane count {}",
            image.num_planes
        ));
    }
    if u32::from(image.width) < PROBE_SURFACE_WIDTH
        || u32::from(image.height) < PROBE_SURFACE_HEIGHT
    {
        return Err(format!(
            "vaDeriveImage returned undersized {}x{} storage",
            image.width, image.height
        ));
    }
    if image.data_size == 0 || image.data_size > MAX_MAPPED_IMAGE_BYTES {
        return Err(format!(
            "vaDeriveImage returned invalid data size {}",
            image.data_size
        ));
    }

    let bytes_per_sample = if expected_fourcc == VA_FOURCC_P010 {
        2
    } else {
        1
    };
    let luma_row_bytes = PROBE_SURFACE_WIDTH * bytes_per_sample;
    let chroma_row_bytes = PROBE_SURFACE_WIDTH.div_ceil(2) * 2 * bytes_per_sample;
    validate_image_plane(image, 0, luma_row_bytes, PROBE_SURFACE_HEIGHT)?;
    validate_image_plane(image, 1, chroma_row_bytes, PROBE_SURFACE_HEIGHT.div_ceil(2))
}

fn validate_image_plane(
    image: &VaImage,
    plane: usize,
    row_bytes: u32,
    height: u32,
) -> Result<(), String> {
    let pitch = image.pitches[plane];
    if pitch < row_bytes {
        return Err(format!(
            "vaDeriveImage plane {plane} pitch {pitch} is smaller than its {row_bytes}-byte row"
        ));
    }
    let required = height
        .saturating_sub(1)
        .checked_mul(pitch)
        .and_then(|bytes| bytes.checked_add(row_bytes))
        .and_then(|bytes| bytes.checked_add(image.offsets[plane]))
        .ok_or_else(|| format!("vaDeriveImage plane {plane} size overflowed"))?;
    if required > image.data_size {
        return Err(format!(
            "vaDeriveImage plane {plane} exceeds its {}-byte buffer",
            image.data_size
        ));
    }
    Ok(())
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
                mapped_surface_format: Some("NV12"),
                mapping_failure: None,
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
            "VA-API initialized a H264 VLD context and mapped a NV12 surface on /dev/dri/renderD128 (Test VA driver), but packet decode is not executable in this build"
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

    #[test]
    fn validates_mapped_nv12_layout_bounds() {
        assert_eq!(std::mem::size_of::<VaImageFormat>(), 48);
        assert_eq!(std::mem::size_of::<VaImage>(), 120);

        let image = VaImage {
            format: VaImageFormat {
                fourcc: VA_FOURCC_NV12,
                ..VaImageFormat::default()
            },
            width: 64,
            height: 64,
            data_size: 6_144,
            num_planes: 2,
            pitches: [64, 64, 0],
            offsets: [0, 4_096, 0],
            ..VaImage::default()
        };
        assert!(validate_image_layout(&image, VA_FOURCC_NV12, "NV12").is_ok());

        let truncated = VaImage {
            data_size: 6_143,
            ..image
        };
        assert!(
            validate_image_layout(&truncated, VA_FOURCC_NV12, "NV12")
                .unwrap_err()
                .contains("plane 1")
        );
    }
}
