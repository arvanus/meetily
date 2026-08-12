//! System audio capture on Linux, through the PulseAudio protocol.
//!
//! Every sink the server knows about has a companion *monitor source* carrying whatever is
//! being played through it, which is what "system audio" means here. Monitors are objects
//! of the sound server, not of ALSA: `snd_device_name_hint` never reports them, so cpal can
//! neither list nor open one. That is why this path exists alongside the cpal microphone
//! path instead of reusing it.
//!
//! Speaking the Pulse protocol rather than PipeWire's own API is deliberate. PipeWire ships
//! `pipewire-pulse` as its supported application interface, so one client library reaches
//! both servers; linking libpipewire instead would leave the binary unable to even load on a
//! machine still running PulseAudio.
//!
//! Format conversion is the server's job, not ours. Monitors here are s24-32le or s32le; we
//! ask for f32 at the pipeline's rate and the server resamples and converts, so no sample
//! unpacking code lives in this file.

use anyhow::{anyhow, bail, Result};
use log::{debug, info, warn};
use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use libpulse_simple_binding as pulse_simple;

use pulse::callbacks::ListResult;
use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
use pulse::def::BufferAttr;
use pulse::mainloop::standard::Mainloop;
use pulse::operation::State as OperationState;
use pulse::proplist::{properties, Proplist};
use pulse::sample::{Format, Spec};
use pulse::stream::Direction;
use pulse::time::MicroSeconds;
use pulse_simple::Simple;

/// Server-side alias for the default sink's monitor.
///
/// The server resolves it at connection time, so following the user's output device costs
/// no round trip and no bookkeeping of our own.
pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

/// How long to wait for the server before giving up on a request.
const SERVER_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a single mainloop step may wait for the server to say something.
const POLL_TIMEOUT: MicroSeconds = MicroSeconds(50_000);

/// Read size requested from the server, as a slice of a second.
const READ_WINDOW: Duration = Duration::from_millis(20);

/// How long teardown waits for the capture thread before abandoning it.
///
/// Generous next to a 20 ms read, and bounded because the alternative is freezing whoever
/// stops the recording. See [`PulseCapture::drop`].
const STOP_TIMEOUT: Duration = Duration::from_millis(500);

/// A monitor source: system audio for one output device.
#[derive(Clone, Debug)]
pub struct MonitorSource {
    /// Server-side name. Opaque, stable, and what [`open`] expects.
    pub name: String,
    /// Human-readable label, e.g. "Monitor of Built-in Audio".
    pub description: String,
    /// Whether this monitors the sink audio currently plays through.
    pub is_default: bool,
}

/// Connect to the sound server and run `request` against it.
///
/// The connection is torn down before returning; nothing here is worth pooling, since both
/// callers run only on device enumeration.
fn with_context<T>(purpose: &str, request: impl FnOnce(&mut Context, &mut Mainloop) -> Result<T>) -> Result<T> {
    let mut proplist = Proplist::new().ok_or_else(|| anyhow!("Failed to create a PulseAudio proplist"))?;
    let _ = proplist.set_str(properties::APPLICATION_NAME, "Meetily");

    let mut mainloop = Mainloop::new().ok_or_else(|| anyhow!("Failed to create a PulseAudio mainloop"))?;
    let mut context = Context::new_with_proplist(&mainloop, purpose, &proplist)
        .ok_or_else(|| anyhow!("Failed to create a PulseAudio context"))?;

    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| anyhow!("Failed to connect to the sound server: {}", e))?;

    let deadline = Instant::now() + SERVER_TIMEOUT;
    loop {
        iterate(&mut mainloop)?;
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                bail!("The sound server refused the connection")
            }
            _ => {}
        }
        if Instant::now() > deadline {
            context.disconnect();
            bail!("Timed out connecting to the sound server");
        }
    }

    let result = request(&mut context, &mut mainloop);
    context.disconnect();
    result
}

/// Drive the mainloop one step, waiting up to [`POLL_TIMEOUT`] for the server to speak.
///
/// Deliberately not `Mainloop::iterate`: with `block = false` it returns immediately and
/// turns every wait below into a busy loop burning a full core for as long as the request
/// takes, and with `block = true` it waits with no upper bound, so a server that accepts
/// the socket and then goes quiet would hang the caller forever. `prepare` takes the
/// timeout that gives both properties at once.
fn iterate(mainloop: &mut Mainloop) -> Result<()> {
    mainloop
        .prepare(Some(POLL_TIMEOUT))
        .map_err(|e| anyhow!("PulseAudio mainloop error: {}", e))?;
    mainloop
        .poll()
        .map_err(|e| anyhow!("PulseAudio poll error: {}", e))?;
    mainloop
        .dispatch()
        .map_err(|e| anyhow!("PulseAudio dispatch error: {}", e))?;

    Ok(())
}

/// Pump the mainloop until `operation` finishes or the server stops answering.
fn wait_for<T: ?Sized>(mainloop: &mut Mainloop, operation: &pulse::operation::Operation<T>) -> Result<()> {
    let deadline = Instant::now() + SERVER_TIMEOUT;

    while operation.get_state() == OperationState::Running {
        iterate(mainloop)?;
        if Instant::now() > deadline {
            bail!("The sound server stopped responding");
        }
    }

    Ok(())
}

/// List every monitor source the server exposes, one per output device.
///
/// Fails rather than reporting an empty list when the server cannot be reached or does not
/// answer: "there is no system audio on this machine" and "the server did not respond in
/// time" call for different handling, and collapsing them makes a transient stall look like
/// a device that ceased to exist. Enumeration callers are expected to degrade to
/// microphones only; whoever is about to record wants the real reason.
pub fn list_monitor_sources() -> Result<Vec<MonitorSource>> {
    with_context("Meetily device enumeration", |context, mainloop| {
        // Resolve the default sink first: its monitor is the one to preselect.
        let default_sink: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let sink_slot = default_sink.clone();
        let server_op = context.introspect().get_server_info(move |info| {
            *sink_slot.borrow_mut() = info.default_sink_name.as_ref().map(|n| n.to_string());
        });
        wait_for(mainloop, &server_op)?;

        // Compared against monitor_of_sink_name below rather than against a
        // "<sink>.monitor" string built here: the server states which sink a monitor
        // belongs to, and the naming convention is only a convention.
        let default_sink_name = default_sink.borrow().clone();

        let sources: Rc<RefCell<Vec<MonitorSource>>> = Rc::new(RefCell::new(Vec::new()));
        let sources_slot = sources.clone();

        let list_op = context.introspect().get_source_info_list(move |result| {
            let ListResult::Item(info) = result else {
                return;
            };

            // monitor_of_sink is set only on monitors, which is exactly the distinction
            // between "records what is playing" and "records a microphone".
            if info.monitor_of_sink.is_none() {
                return;
            }

            let Some(name) = info.name.as_ref().map(|n| n.to_string()) else {
                return;
            };

            let description = info
                .description
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| name.clone());

            let is_default = match (&default_sink_name, &info.monitor_of_sink_name) {
                (Some(default_sink), Some(owning_sink)) => default_sink == owning_sink.as_ref(),
                _ => false,
            };
            sources_slot.borrow_mut().push(MonitorSource { name, description, is_default });
        });
        wait_for(mainloop, &list_op)?;

        let found = sources.borrow().clone();
        debug!("Found {} monitor source(s) for system audio", found.len());
        Ok(found)
    })
}

/// Name of the default sink's monitor, for display.
///
/// [`DEFAULT_MONITOR`] would capture the same audio without a round trip, but it shows up
/// in the UI and in saved preferences, where a literal `@DEFAULT_MONITOR@` would be opaque.
pub fn default_monitor_source() -> Result<MonitorSource> {
    let sources = list_monitor_sources()?;

    if let Some(default) = sources.iter().find(|s| s.is_default) {
        return Ok(default.clone());
    }

    // No sink is marked default (or the server is unreachable); any monitor still beats
    // reporting that system audio does not exist.
    sources
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No system audio source available"))
}

/// A running capture, stopped by dropping it.
pub struct PulseCapture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Signalled when the capture loop exits; also disconnects if the thread unwinds.
    finished: std::sync::mpsc::Receiver<()>,
    source: String,
}

impl PulseCapture {
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl Drop for PulseCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        let Some(thread) = self.thread.take() else {
            return;
        };

        // Not a bare join(). The stop flag is only read between reads, and pa_simple_read
        // returns when it has filled the buffer - fragsize is a request, not a promise, so
        // a source that stops producing (a suspended sink, a server that went quiet without
        // dropping the connection) leaves the thread parked inside it with no deadline.
        // This runs on the recording teardown path, so blocking there freezes stopping a
        // recording. Wait a bounded while, then leave the thread behind: it holds nothing
        // but its own PulseAudio handle and exits on its own if the read ever returns.
        match self.finished.recv_timeout(STOP_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if thread.join().is_err() {
                    warn!("The system audio capture thread panicked");
                }
                info!("System audio capture stopped for '{}'", self.source);
            }
            Err(RecvTimeoutError::Timeout) => {
                warn!(
                    "System audio capture on '{}' did not stop within {:?}; abandoning its thread",
                    self.source, STOP_TIMEOUT
                );
            }
        }
    }
}

/// Start capturing `source`, delivering f32 samples to `on_samples` until the returned
/// handle is dropped.
///
/// `source` is a server-side source name, or [`DEFAULT_MONITOR`]. Errors opening the stream
/// are reported synchronously, so a caller can fall back to microphone-only recording
/// instead of discovering the failure through silence.
pub fn open<F>(source: &str, sample_rate: u32, channels: u8, mut on_samples: F) -> Result<PulseCapture>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let spec = Spec { format: Format::F32le, channels, rate: sample_rate };
    if !spec.is_valid() {
        bail!("Invalid capture format: {} channel(s) at {} Hz", channels, sample_rate);
    }

    let bytes_per_frame = spec.frame_size();
    let frames_per_read = (sample_rate as u64 * READ_WINDOW.as_millis() as u64 / 1000) as usize;
    let read_bytes = frames_per_read * bytes_per_frame;

    // Only fragsize matters for recording; the rest keep the server's own sizing.
    let attr = BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: read_bytes as u32,
    };

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let source_name = source.to_string();
    let thread_source = source_name.clone();

    // pa_simple's handle is tied to the thread that reads from it, so it is built there.
    // This channel carries the outcome back so that open() can still fail synchronously.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    // Lets teardown tell "the loop ended" from "still stuck in a read" without blocking on
    // a join. The sender travels into the thread, so an unwind disconnects it just as an
    // orderly exit signals it.
    let (finished_tx, finished_rx) = std::sync::mpsc::channel::<()>();

    let thread = std::thread::Builder::new()
        .name("meetily-system-audio".into())
        .spawn(move || {
            let _finished_tx = finished_tx;
            let simple = match Simple::new(
                None,
                "Meetily",
                Direction::Record,
                Some(&thread_source),
                "Meeting system audio",
                &spec,
                None,
                Some(&attr),
            ) {
                Ok(simple) => {
                    let _ = ready_tx.send(Ok(()));
                    simple
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{}", e)));
                    return;
                }
            };

            let mut raw = vec![0u8; read_bytes];
            let mut samples = vec![0f32; read_bytes / 4];

            while !thread_stop.load(Ordering::Relaxed) {
                if let Err(e) = simple.read(&mut raw) {
                    // A dropped connection (the server restarted, the sink disappeared)
                    // ends this capture; the device monitor is what notices and restarts.
                    warn!("System audio read failed, ending capture: {}", e);
                    break;
                }

                for (sample, bytes) in samples.iter_mut().zip(raw.chunks_exact(4)) {
                    *sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                }

                // The consumer is the mixing pipeline, and a panic there would otherwise
                // unwind this thread and leave the recording with silent system audio
                // until someone read the logs. End the capture deliberately instead, on
                // the same path a read failure takes.
                if std::panic::catch_unwind(AssertUnwindSafe(|| on_samples(&samples))).is_err() {
                    warn!("The system audio consumer panicked, ending capture");
                    break;
                }
            }
        })
        .map_err(|e| anyhow!("Failed to start the system audio thread: {}", e))?;

    match ready_rx.recv_timeout(SERVER_TIMEOUT) {
        Ok(Ok(())) => {
            info!(
                "System audio capture started on '{}' ({} Hz, {} channel(s))",
                source_name, sample_rate, channels
            );
            Ok(PulseCapture { stop, thread: Some(thread), finished: finished_rx, source: source_name })
        }
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(anyhow!("Could not open system audio source '{}': {}", source_name, e))
        }
        Err(_) => {
            // Still inside Simple::new, which does a synchronous handshake and reads no
            // flag. Setting it means the thread stops the moment the handshake returns
            // rather than capturing into a callback nobody is listening to any more.
            stop.store(true, Ordering::Relaxed);
            Err(anyhow!("Timed out opening system audio source '{}'", source_name))
        }
    }
}
