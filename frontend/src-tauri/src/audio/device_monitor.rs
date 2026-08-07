// Audio device monitoring for disconnect/reconnect detection
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use anyhow::Result;
use log::{debug, info, warn, error};

use super::devices::{default_input_device, default_output_device, AudioDevice, list_audio_devices};

/// Device monitoring events
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    /// A device that was in use has disconnected
    DeviceDisconnected {
        device_name: String,
        device_type: DeviceMonitorType,
    },
    /// A previously disconnected device has reconnected
    DeviceReconnected {
        device_name: String,
        device_type: DeviceMonitorType,
    },
    /// Device list has changed (new device added or removed)
    DeviceListChanged,
}

/// Type of device being monitored
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceMonitorType {
    Microphone,
    SystemAudio,
}

/// Monitor state for a single device
#[derive(Debug, Clone)]
struct MonitoredDevice {
    name: String,
    device_type: DeviceMonitorType,
    consecutive_missing: u32,
    is_bluetooth: bool,
}

impl MonitoredDevice {
    fn new(name: String, device_type: DeviceMonitorType) -> Self {
        // Heuristic: check if device name contains bluetooth-related keywords
        let is_bluetooth = name.to_lowercase().contains("airpods")
            || name.to_lowercase().contains("bluetooth")
            || name.to_lowercase().contains("wireless");

        Self {
            name,
            device_type,
            consecutive_missing: 0,
            is_bluetooth,
        }
    }

    /// Get appropriate disconnect threshold based on device type
    fn disconnect_threshold(&self) -> u32 {
        // Bluetooth devices get more grace period (they can briefly disconnect)
        if self.is_bluetooth {
            3 // 3 polling cycles (6-15 seconds)
        } else {
            2 // 2 polling cycles (4-10 seconds)
        }
    }

    /// Get appropriate reconnect check interval
    #[allow(dead_code)]
    fn reconnect_interval(&self) -> Duration {
        if self.is_bluetooth {
            Duration::from_secs(5) // Check every 5s for Bluetooth
        } else {
            Duration::from_secs(3) // Check every 3s for wired devices
        }
    }
}

/// Audio device monitor that detects disconnects and reconnects
pub struct AudioDeviceMonitor {
    monitor_handle: Option<JoinHandle<()>>,
    event_sender: mpsc::UnboundedSender<DeviceEvent>,
    stop_signal: Arc<tokio::sync::Notify>,
}

impl AudioDeviceMonitor {
    /// Create a new device monitor
    pub fn new() -> (Self, mpsc::UnboundedReceiver<DeviceEvent>) {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let stop_signal = Arc::new(tokio::sync::Notify::new());

        (
            Self {
                monitor_handle: None,
                event_sender,
                stop_signal,
            },
            event_receiver,
        )
    }

    /// Start monitoring specified devices
    pub fn start_monitoring(
        &mut self,
        microphone: Option<Arc<AudioDevice>>,
        system_audio: Option<Arc<AudioDevice>>,
    ) -> Result<()> {
        if self.monitor_handle.is_some() {
            warn!("Device monitor already running");
            return Ok(());
        }

        let mut monitored_devices = Vec::new();

        if let Some(mic) = microphone {
            monitored_devices.push(MonitoredDevice::new(
                mic.name.clone(),
                DeviceMonitorType::Microphone,
            ));
            info!("🔍 Monitoring microphone: '{}' (Bluetooth: {})",
                  mic.name, monitored_devices.last().unwrap().is_bluetooth);
        }

        if let Some(sys) = system_audio {
            monitored_devices.push(MonitoredDevice::new(
                sys.name.clone(),
                DeviceMonitorType::SystemAudio,
            ));
            info!("🔍 Monitoring system audio: '{}' (Bluetooth: {})",
                  sys.name, monitored_devices.last().unwrap().is_bluetooth);
        }

        if monitored_devices.is_empty() {
            return Err(anyhow::anyhow!("No devices to monitor"));
        }

        let event_sender = self.event_sender.clone();
        let stop_signal = self.stop_signal.clone();

        let handle = tokio::spawn(async move {
            Self::monitor_loop(monitored_devices, event_sender, stop_signal).await;
        });

        self.monitor_handle = Some(handle);
        info!("✅ Device monitor started");
        Ok(())
    }

    /// Stop monitoring
    pub async fn stop_monitoring(&mut self) {
        info!("Stopping device monitor");
        self.stop_signal.notify_one();

        if let Some(handle) = self.monitor_handle.take() {
            let _ = handle.await;
        }

        info!("Device monitor stopped");
    }

    /// Main monitoring loop
    async fn monitor_loop(
        mut monitored_devices: Vec<MonitoredDevice>,
        event_sender: mpsc::UnboundedSender<DeviceEvent>,
        stop_signal: Arc<tokio::sync::Notify>,
    ) {
        let mut last_device_list = Vec::new();
        let check_interval = Duration::from_secs(2); // Poll every 2 seconds

        loop {
            // Check for stop signal with timeout
            tokio::select! {
                _ = stop_signal.notified() => {
                    info!("Device monitor received stop signal");
                    break;
                }
                _ = tokio::time::sleep(check_interval) => {
                    // Continue with monitoring check
                }
            }

            // Get current device list
            let current_devices = match list_audio_devices().await {
                Ok(devices) => devices,
                Err(e) => {
                    error!("Failed to list audio devices: {}", e);
                    continue;
                }
            };

            // Check if device list changed
            if current_devices.len() != last_device_list.len() {
                debug!("Device list changed: {} -> {} devices",
                       last_device_list.len(), current_devices.len());
                let _ = event_sender.send(DeviceEvent::DeviceListChanged);
            }
            last_device_list = current_devices.clone();

            // Check each monitored device
            for monitored in &mut monitored_devices {
                let device_found = current_devices.iter().any(|d| d.name == monitored.name);

                if device_found {
                    // Device is present
                    if monitored.consecutive_missing > 0 {
                        // Device has reconnected!
                        info!("✅ Device '{}' reconnected after {} missing checks",
                              monitored.name, monitored.consecutive_missing);

                        let _ = event_sender.send(DeviceEvent::DeviceReconnected {
                            device_name: monitored.name.clone(),
                            device_type: monitored.device_type.clone(),
                        });

                        monitored.consecutive_missing = 0;
                    }
                } else {
                    // Device is missing
                    monitored.consecutive_missing += 1;

                    debug!("⚠️ Device '{}' missing for {} checks (threshold: {})",
                          monitored.name, monitored.consecutive_missing,
                          monitored.disconnect_threshold());

                    // Only emit disconnect event once when threshold is reached
                    if monitored.consecutive_missing == monitored.disconnect_threshold() {
                        warn!("❌ Device '{}' ({:?}) disconnected!",
                              monitored.name, monitored.device_type);

                        let _ = event_sender.send(DeviceEvent::DeviceDisconnected {
                            device_name: monitored.name.clone(),
                            device_type: monitored.device_type.clone(),
                        });
                    }
                }
            }

            // Adjust check interval based on device states
            // If any device is missing, check more frequently
            let has_missing = monitored_devices.iter().any(|d| d.consecutive_missing > 0);
            let next_interval = if has_missing {
                Duration::from_secs(2) // Fast polling when device missing
            } else {
                Duration::from_secs(5) // Slower polling when all devices present
            };

            if next_interval != check_interval {
                debug!("Adjusting monitor interval to {:?}", next_interval);
            }
        }
    }
}

impl Default for AudioDeviceMonitor {
    fn default() -> Self {
        Self::new().0
    }
}

impl Drop for AudioDeviceMonitor {
    fn drop(&mut self) {
        // Signal stop
        self.stop_signal.notify_one();
    }
}

/// How often the default-device watcher samples the system defaults
const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// How many consecutive samples a new default must survive before it is acted on.
/// Bluetooth devices flap while connecting, and every migration costs a short
/// audio gap, so a single sample is not enough evidence.
const DEFAULT_CHANGE_DEBOUNCE_CYCLES: u32 = 2;

/// The system default device for one role changed while recording was following it
#[derive(Debug, Clone)]
pub struct DefaultDeviceChange {
    pub device_type: DeviceMonitorType,
    pub old_name: String,
    pub new_name: String,
}

/// Tracks the system default for a single role (microphone or system audio)
struct TrackedDefault {
    device_type: DeviceMonitorType,
    /// Device the recording is currently capturing from
    active_name: String,
    /// New default seen but not yet stable enough to act on
    candidate: Option<String>,
    candidate_hits: u32,
}

impl TrackedDefault {
    fn new(device_type: DeviceMonitorType, active_name: String) -> Self {
        Self {
            device_type,
            active_name,
            candidate: None,
            candidate_hits: 0,
        }
    }

    /// Feed one sample of the current system default.
    /// Returns a change only once the same new default survived the debounce window.
    fn observe(&mut self, current: String) -> Option<DefaultDeviceChange> {
        if current == self.active_name {
            // Default flipped back before we acted on it
            self.candidate = None;
            self.candidate_hits = 0;
            return None;
        }

        match self.candidate {
            Some(ref candidate) if *candidate == current => self.candidate_hits += 1,
            _ => {
                self.candidate = Some(current.clone());
                self.candidate_hits = 1;
            }
        }

        if self.candidate_hits < DEFAULT_CHANGE_DEBOUNCE_CYCLES {
            return None;
        }

        let old_name = std::mem::replace(&mut self.active_name, current.clone());
        self.candidate = None;
        self.candidate_hits = 0;

        Some(DefaultDeviceChange {
            device_type: self.device_type.clone(),
            old_name,
            new_name: current,
        })
    }
}

/// Watches the system default input/output devices and reports when they change.
///
/// This is deliberately separate from [`AudioDeviceMonitor`]: plugging in a headset
/// flips the default while the old device stays present, so neither
/// `DeviceDisconnected` (device still listed) nor `DeviceListChanged` (device count
/// unchanged on a swap) fires. It also avoids `list_audio_devices()` - full device
/// enumeration on a loop is what made shutdown take 90+ seconds on Windows - and
/// asks only for the two default handles.
///
/// The watcher reports what the system default is, not what capture succeeded on: a
/// migration that fails is not retried until the default changes again.
pub struct DefaultDeviceWatcher {
    handle: Option<JoinHandle<()>>,
    stop_signal: Arc<tokio::sync::Notify>,
}

impl DefaultDeviceWatcher {
    /// Start watching. Pass the currently active device name for each role that
    /// should follow the system default, `None` for roles that are pinned.
    pub fn start(
        microphone: Option<String>,
        system_audio: Option<String>,
    ) -> (Self, mpsc::UnboundedReceiver<DefaultDeviceChange>) {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let stop_signal = Arc::new(tokio::sync::Notify::new());

        let mut tracked = Vec::new();
        if let Some(name) = microphone {
            info!("🔁 Following system default microphone (currently '{}')", name);
            tracked.push(TrackedDefault::new(DeviceMonitorType::Microphone, name));
        }
        if let Some(name) = system_audio {
            info!("🔁 Following system default audio output (currently '{}')", name);
            tracked.push(TrackedDefault::new(DeviceMonitorType::SystemAudio, name));
        }

        let handle = if tracked.is_empty() {
            None
        } else {
            let sender = event_sender;
            let stop = stop_signal.clone();
            Some(tokio::spawn(async move {
                Self::watch_loop(tracked, sender, stop).await;
            }))
        };

        (
            Self {
                handle,
                stop_signal,
            },
            event_receiver,
        )
    }

    /// Stop watching and wait for the loop to finish
    pub async fn stop(&mut self) {
        self.stop_signal.notify_one();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
        debug!("Default device watcher stopped");
    }

    async fn watch_loop(
        mut tracked: Vec<TrackedDefault>,
        event_sender: mpsc::UnboundedSender<DefaultDeviceChange>,
        stop_signal: Arc<tokio::sync::Notify>,
    ) {
        let watch_microphone = tracked
            .iter()
            .any(|t| t.device_type == DeviceMonitorType::Microphone);
        let watch_system = tracked
            .iter()
            .any(|t| t.device_type == DeviceMonitorType::SystemAudio);

        loop {
            tokio::select! {
                _ = stop_signal.notified() => {
                    debug!("Default device watcher received stop signal");
                    break;
                }
                _ = tokio::time::sleep(DEFAULT_WATCH_INTERVAL) => {}
            }

            // cpal device queries block; keep them off the async worker threads
            let defaults = tokio::task::spawn_blocking(move || {
                let microphone = if watch_microphone {
                    default_input_device().ok().map(|d| d.name)
                } else {
                    None
                };
                let system_audio = if watch_system {
                    default_output_device().ok().map(|d| d.name)
                } else {
                    None
                };
                (microphone, system_audio)
            })
            .await;

            let (default_microphone, default_system) = match defaults {
                Ok(values) => values,
                Err(e) => {
                    warn!("Failed to read system default devices: {}", e);
                    continue;
                }
            };

            for entry in &mut tracked {
                // A transient read failure is not a device change - keep the last known default
                let current = match entry.device_type {
                    DeviceMonitorType::Microphone => default_microphone.clone(),
                    DeviceMonitorType::SystemAudio => default_system.clone(),
                };
                let Some(current) = current else { continue };

                if let Some(change) = entry.observe(current) {
                    info!(
                        "🔁 System default {:?} changed: '{}' -> '{}'",
                        change.device_type, change.old_name, change.new_name
                    );
                    if event_sender.send(change).is_err() {
                        debug!("Default device change receiver dropped, stopping watcher");
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for DefaultDeviceWatcher {
    fn drop(&mut self) {
        self.stop_signal.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bluetooth_detection() {
        let airpods = MonitoredDevice::new(
            "John's AirPods Pro".to_string(),
            DeviceMonitorType::Microphone,
        );
        assert!(airpods.is_bluetooth);
        assert_eq!(airpods.disconnect_threshold(), 3);

        let builtin = MonitoredDevice::new(
            "Built-in Microphone".to_string(),
            DeviceMonitorType::Microphone,
        );
        assert!(!builtin.is_bluetooth);
        assert_eq!(builtin.disconnect_threshold(), 2);
    }

    #[tokio::test]
    async fn test_monitor_creation() {
        let (mut monitor, _receiver) = AudioDeviceMonitor::new();
        assert!(monitor.monitor_handle.is_none());

        // Stop should be safe even if not started
        monitor.stop_monitoring().await;
    }

    #[test]
    fn test_default_change_needs_debounce() {
        let mut tracked = TrackedDefault::new(
            DeviceMonitorType::Microphone,
            "Built-in Microphone".to_string(),
        );

        // Same default: nothing to do
        assert!(tracked.observe("Built-in Microphone".to_string()).is_none());

        // First sighting of the new default is not enough
        assert!(tracked.observe("Headset Microphone".to_string()).is_none());

        // Second consecutive sighting triggers the migration
        let change = tracked
            .observe("Headset Microphone".to_string())
            .expect("change after debounce window");
        assert_eq!(change.old_name, "Built-in Microphone");
        assert_eq!(change.new_name, "Headset Microphone");

        // The new device is now the active one, so it stops being a change
        assert!(tracked.observe("Headset Microphone".to_string()).is_none());
    }

    #[test]
    fn test_default_change_flap_is_ignored() {
        let mut tracked = TrackedDefault::new(
            DeviceMonitorType::SystemAudio,
            "Speakers".to_string(),
        );

        // Bluetooth device appears for one cycle, then the default flips back
        assert!(tracked.observe("Headphones".to_string()).is_none());
        assert!(tracked.observe("Speakers".to_string()).is_none());

        // The earlier sighting must not count towards the next candidate
        assert!(tracked.observe("Headphones".to_string()).is_none());
    }

    #[test]
    fn test_default_change_candidate_switch_restarts_debounce() {
        let mut tracked =
            TrackedDefault::new(DeviceMonitorType::Microphone, "Mic A".to_string());

        assert!(tracked.observe("Mic B".to_string()).is_none());
        // Different candidate: counter restarts instead of carrying over
        assert!(tracked.observe("Mic C".to_string()).is_none());
        let change = tracked
            .observe("Mic C".to_string())
            .expect("change after debounce window");
        assert_eq!(change.new_name, "Mic C");
    }
}
