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

/// Enumerate Linux audio devices: microphones through ALSA, system audio through the
/// sound server.
///
/// The two halves come from different places because they are captured from different
/// places. Microphones are ALSA PCMs that cpal opens directly. System audio is a monitor
/// source, which exists only inside PulseAudio or PipeWire - `snd_device_name_hint` never
/// reports one, so anything looking for monitors among ALSA devices finds nothing, and
/// anything offering ALSA playback devices as a substitute offers something that cannot be
/// opened for capture at all.
pub fn configure_linux_audio(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    let mut devices = Vec::new();

    for device in host.input_devices()? {
        if let Ok(name) = device.name() {
            devices.push(AudioDevice::new(name, DeviceType::Input));
        }
    }

    // Names are the server's own, unadorned: they are what gets stored in the recording
    // preferences and handed straight back to pa_simple_new when a recording starts.
    //
    // A server that is missing or unwell costs the user system audio, not the whole device
    // list - losing the microphones too would turn a degraded setup into an unusable one.
    match crate::audio::capture::list_monitor_sources() {
        Ok(monitors) => {
            for monitor in monitors {
                devices.push(AudioDevice::new(monitor.name, DeviceType::Output));
            }
        }
        Err(e) => log::warn!("System audio unavailable, listing microphones only: {}", e),
    }

    Ok(devices)
}