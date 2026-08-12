use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use std::os::raw::{c_char, c_int};
use std::sync::Once;

use crate::audio::devices::configuration::{AudioDevice, DeviceType};

/// Stop libasound from writing its own diagnostics to stderr.
///
/// Enumerating devices makes ALSA probe every plugin definition, including `dmix` for
/// capture and `dsnoop` for playback, which those plugins reject by design. Its default
/// error handler prints one line per rejection - 57 per `list_audio_devices()` call - even
/// though the enumeration itself succeeds and the audio path is unaffected. The device
/// monitor polls that list every couple of seconds while recording, so the CLI status
/// panel gets overwritten several times a second by messages that carry no information.
///
/// A no-op handler drops them at the source. Only the library's own logging is silenced;
/// return codes still reach the caller, so real failures surface as before.
pub fn silence_alsa_logging() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        unsafe extern "C" fn ignore(
            _file: *const c_char,
            _line: c_int,
            _function: *const c_char,
            _err: c_int,
            _fmt: *const c_char,
        ) {
        }

        // SAFETY: ALSA types the handler as variadic, which stable Rust cannot define, so
        // a non-variadic no-op is transmuted into that signature. The two agree on the
        // named parameters, and the body reads no argument at all before returning, so
        // whatever ALSA passes beyond them is never touched.
        let installed = unsafe {
            let handler = std::mem::transmute::<
                unsafe extern "C" fn(*const c_char, c_int, *const c_char, c_int, *const c_char),
                unsafe extern "C" fn(
                    *const c_char,
                    c_int,
                    *const c_char,
                    c_int,
                    *const c_char,
                    ...
                ),
            >(ignore);
            alsa_sys::snd_lib_error_set_handler(Some(handler))
        };

        if installed < 0 {
            log::warn!(
                "Could not install the ALSA error handler ({}); device enumeration will keep writing to stderr",
                installed
            );
        }
    });
}

/// Configure Linux audio devices using ALSA/PulseAudio
pub fn configure_linux_audio(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    let mut devices = Vec::new();

    // Add input devices
    for device in host.input_devices()? {
        if let Ok(name) = device.name() {
            devices.push(AudioDevice::new(name, DeviceType::Input));
        }
    }

    // Add PulseAudio monitor sources for system audio
    if let Ok(pulse_host) = cpal::host_from_id(cpal::HostId::Alsa) {
        for device in pulse_host.input_devices()? {
            if let Ok(name) = device.name() {
                // Check if it's a monitor source
                if name.contains("monitor") {
                    devices.push(AudioDevice::new(
                        format!("{} (System Audio)", name),
                        DeviceType::Output
                    ));
                }
            }
        }
    }

    Ok(devices)
}