//! PipeWire audio capture (Linux) with CLOCK_MONOTONIC timestamps.
//!
//! One capture stream per [`CaptureEndpoint`], on its own thread running a PipeWire main
//! loop (all callbacks on that thread, so the shared state is a plain `RefCell`):
//! - `Loopback`: the monitor of an output device (`stream.capture.sink`; the default one
//!   follows the session manager's default).
//! - `Input`: a capture device such as a microphone.
//! - `Process`: apps' playback streams linked to the capture stream by Klip itself: the
//!   streams of one process tree (`include`), or every playback stream except that tree.
//!   PipeWire mixes the links at the input ports. Links follow streams as they come and go.
//!
//! Each packet is stamped with the graph cycle's time minus the stream's reported delay
//! (`pw_stream_get_time_n`), on CLOCK_MONOTONIC like GStreamer's system clock. While
//! nothing arrives (no app playing, a lost device), silence is generated on that timeline
//! ([`crate::timeline`]); a failed connection is retried every two seconds.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::types::ObjectType;

use crate::timeline::{Format, RETRY_NS, deliver, fill_silence};
use crate::{AudioCaptureSink, CaptureEndpoint, PlatformError, qpc_now_ns};

/// How often the loop checks for stop and fills silence.
const TICK: Duration = Duration::from_millis(10);
/// Largest buffer accepted from PipeWire, in frames (1 s at 192 kHz).
const MAX_FRAMES: usize = 192_000;

/// A running capture thread.
pub(crate) struct Handle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    pub(crate) fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) fn start(
    endpoint: CaptureEndpoint,
    rate: u32,
    channels: u16,
    sink: Box<dyn AudioCaptureSink>,
) -> Result<Handle, PlatformError> {
    if rate == 0 || channels == 0 || channels > 8 {
        return Err(PlatformError::Os("bad audio format".into()));
    }
    let fmt = Format { rate, channels };
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("klip-audio-capture".into())
        .spawn(move || {
            let state = Rc::new(RefCell::new(Shared {
                sink,
                next: Some(qpc_now_ns()),
                samples: Vec::new(),
                delivered: false,
            }));
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&endpoint, fmt, &flag, &state);
            }));
            if res.is_err()
                && let Ok(mut s) = state.try_borrow_mut()
            {
                s.sink
                    .notice("internal error in audio capture; it stopped".into());
            }
        })
        .map_err(|e| PlatformError::Os(format!("audio thread: {e}")))?;
    Ok(Handle {
        stop,
        thread: Some(thread),
    })
}

/// State the loop's callbacks share (all on the capture thread).
struct Shared {
    sink: Box<dyn AudioCaptureSink>,
    /// End of the audio delivered so far.
    next: Option<i64>,
    /// Reused conversion buffer.
    samples: Vec<f32>,
    /// Real audio arrived in the current session.
    delivered: bool,
}

impl Shared {
    fn fill_silence(&mut self, fmt: Format) {
        fill_silence(fmt, &mut self.next, self.sink.as_mut());
    }
}

fn run(
    endpoint: &CaptureEndpoint,
    fmt: Format,
    stop: &Arc<AtomicBool>,
    state: &Rc<RefCell<Shared>>,
) {
    pw::init();
    let mut failed = false;
    while !stop.load(Ordering::Relaxed) {
        state.borrow_mut().delivered = false;
        let Err(e) = session(endpoint, fmt, stop, state) else {
            break;
        };
        let delivered = state.borrow().delivered;
        if !failed || delivered {
            state
                .borrow_mut()
                .sink
                .notice(format!("audio capture unavailable, recording silence: {e}"));
        }
        failed = true;
        // Wait before retrying, keeping the stream continuous.
        let retry_at = qpc_now_ns().saturating_add(RETRY_NS);
        while !stop.load(Ordering::Relaxed) && qpc_now_ns() < retry_at {
            state.borrow_mut().fill_silence(fmt);
            std::thread::sleep(TICK);
        }
    }
}

/// Process id of a PipeWire client global: the socket peer's pid (`pipewire.sec.pid`, set
/// by the daemon, so it can't be spoofed), else what the app claims.
fn client_pid(p: &spa::utils::dict::DictRef) -> Option<u32> {
    p.get("pipewire.sec.pid")
        .or_else(|| p.get("application.process.id"))
        .and_then(|s| s.parse().ok())
}

/// The client that owns a node global (node globals carry no process id themselves).
fn node_client(p: &spa::utils::dict::DictRef) -> Option<u32> {
    p.get("client.id").and_then(|s| s.parse().ok())
}

/// What the registry has told us, for linking app streams (`Process` endpoints).
#[derive(Default)]
struct Graph {
    /// Clients: id → process id.
    clients: HashMap<u32, u32>,
    /// Playback streams (`Stream/Output/Audio`): node id → client id.
    streams: HashMap<u32, Option<u32>>,
    /// Ports: id → (node id, is output, channel name).
    ports: HashMap<u32, (u32, bool, String)>,
    /// Links Klip created, by (output port, input port). Dropping one removes the link.
    links: HashMap<(u32, u32), pw::link::Link>,
}

/// Links app streams to the capture stream for a `Process` endpoint.
struct Linker {
    core: pw::core::CoreRc,
    graph: RefCell<Graph>,
    /// The capture stream's node id (0 until connected).
    own: Cell<u32>,
    pid: u32,
    include: bool,
    tree: RefCell<HashSet<u32>>,
}

impl Linker {
    fn global(&self, g: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>) {
        let Some(p) = g.props else { return };
        match g.type_ {
            ObjectType::Client => {
                if let Some(pid) = client_pid(p) {
                    // A process may have joined the tree since we looked.
                    if !self.tree.borrow().contains(&pid) {
                        *self.tree.borrow_mut() = process_tree(self.pid);
                    }
                    self.graph.borrow_mut().clients.insert(g.id, pid);
                }
            }
            ObjectType::Node if p.get("media.class") == Some("Stream/Output/Audio") => {
                self.graph.borrow_mut().streams.insert(g.id, node_client(p));
            }
            ObjectType::Port => {
                let node = p.get("node.id").and_then(|s| s.parse::<u32>().ok());
                let output = p.get("port.direction") == Some("out");
                let monitor = p.get("port.monitor") == Some("true");
                let channel = p.get("audio.channel").unwrap_or("MONO").to_string();
                if let Some(node) = node
                    && !monitor
                {
                    self.graph
                        .borrow_mut()
                        .ports
                        .insert(g.id, (node, output, channel));
                }
            }
            _ => return,
        }
        self.link();
    }

    fn removed(&self, id: u32) {
        let mut graph = self.graph.borrow_mut();
        graph.clients.remove(&id);
        graph.streams.remove(&id);
        graph.ports.remove(&id);
        graph.links.retain(|(o, i), _| *o != id && *i != id);
    }

    fn link(&self) {
        let own = self.own.get();
        if own != 0 && own != u32::MAX {
            update_links(
                &self.core,
                &mut self.graph.borrow_mut(),
                own,
                &self.tree.borrow(),
                self.include,
            );
        }
    }
}

fn pw_err(e: pw::Error) -> String {
    format!("PipeWire: {e}")
}

/// Channel positions for `channels` (SPA_AUDIO_CHANNEL_* values): mono, stereo FL FR,
/// else FL FR FC LFE RL RR SL SR in order.
fn positions(channels: u16) -> [u32; 64] {
    const ORDER: [u32; 8] = [
        spa::sys::SPA_AUDIO_CHANNEL_FL,
        spa::sys::SPA_AUDIO_CHANNEL_FR,
        spa::sys::SPA_AUDIO_CHANNEL_FC,
        spa::sys::SPA_AUDIO_CHANNEL_LFE,
        spa::sys::SPA_AUDIO_CHANNEL_RL,
        spa::sys::SPA_AUDIO_CHANNEL_RR,
        spa::sys::SPA_AUDIO_CHANNEL_SL,
        spa::sys::SPA_AUDIO_CHANNEL_SR,
    ];
    let mut p = [0u32; 64];
    if channels == 1 {
        if let Some(first) = p.first_mut() {
            *first = spa::sys::SPA_AUDIO_CHANNEL_MONO;
        }
        return p;
    }
    for (slot, pos) in p.iter_mut().zip(ORDER).take(usize::from(channels)) {
        *slot = pos;
    }
    p
}

/// The `EnumFormat` param: F32LE interleaved at `fmt` (PipeWire converts to it).
fn format_param(fmt: Format) -> Result<Vec<u8>, String> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(fmt.rate);
    info.set_channels(u32::from(fmt.channels));
    info.set_position(positions(fmt.channels));
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map(|(c, _)| c.into_inner())
    .map_err(|e| format!("audio format: {e:?}"))
}

/// `pid` and its descendants, from a `/proc` snapshot.
fn process_tree(pid: u32) -> HashSet<u32> {
    let procs = crate::linux_impl::list_processes().unwrap_or_default();
    let mut tree = HashSet::from([pid]);
    // Parents can come after children in the listing; repeat until nothing changes.
    loop {
        let before = tree.len();
        for p in &procs {
            if tree.contains(&p.parent_pid) {
                tree.insert(p.pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// Which input port an app's output channel goes to: same channel name; mono to every
/// input; anything else unlinked.
fn input_for<'p>(out_channel: &str, inputs: &'p [(u32, String)]) -> impl Iterator<Item = u32> + 'p {
    let mono = out_channel == "MONO";
    let wanted = out_channel.to_string();
    inputs
        .iter()
        .filter(move |(_, ch)| mono || *ch == wanted)
        .map(|(id, _)| *id)
}

/// Create the links that are missing for `Process` endpoints.
fn update_links(
    core: &pw::core::Core,
    graph: &mut Graph,
    own_node: u32,
    tree: &HashSet<u32>,
    include: bool,
) {
    let inputs: Vec<(u32, String)> = graph
        .ports
        .iter()
        .filter(|(_, (node, output, _))| *node == own_node && !*output)
        .map(|(id, (_, _, ch))| (*id, ch.clone()))
        .collect();
    if inputs.is_empty() {
        return;
    }
    let wanted: Vec<(u32, u32, u32)> = graph
        .ports
        .iter()
        .filter(|(_, (node, output, _))| {
            *output
                && *node != own_node
                && graph.streams.get(node).is_some_and(|client| {
                    let pid = client.and_then(|c| graph.clients.get(&c));
                    let ours = pid.is_some_and(|p| tree.contains(p));
                    ours == include
                })
        })
        .flat_map(|(out_id, (node, _, ch))| {
            input_for(ch, &inputs)
                .map(|in_id| (*node, *out_id, in_id))
                .collect::<Vec<_>>()
        })
        .collect();
    for (node, out_port, in_port) in wanted {
        if graph.links.contains_key(&(out_port, in_port)) {
            continue;
        }
        let props = properties! {
            "link.output.node" => node.to_string(),
            "link.output.port" => out_port.to_string(),
            "link.input.node" => own_node.to_string(),
            "link.input.port" => in_port.to_string(),
            "object.linger" => "false",
        };
        match core.create_object::<pw::link::Link>("link-factory", &props) {
            Ok(link) => {
                graph.links.insert((out_port, in_port), link);
            }
            Err(e) => tracing::debug!(error = %e, "could not link an app stream"),
        }
    }
}

/// One connection to PipeWire, until `stop` (`Ok`) or a failure (`Err`).
fn session(
    endpoint: &CaptureEndpoint,
    fmt: Format,
    stop: &Arc<AtomicBool>,
    state: &Rc<RefCell<Shared>>,
) -> Result<(), String> {
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(pw_err)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(pw_err)?;
    let core = context.connect_rc(None).map_err(pw_err)?;

    let failure: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let _core_listener = core
        .add_listener_local()
        .error({
            let (ml, failure) = (mainloop.clone(), failure.clone());
            move |id, _seq, res, message| {
                // Errors on our own link proxies (an app stream that went away) are routine.
                if id == pw::core::PW_ID_CORE {
                    *failure.borrow_mut() = Some(format!("PipeWire error {res}: {message}"));
                    ml.quit();
                }
            }
        })
        .register();

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Production",
        *pw::keys::NODE_NAME => "klip-audio-capture",
        *pw::keys::APP_NAME => "Klip",
    };
    let linker = match endpoint {
        CaptureEndpoint::Loopback { device } => {
            props.insert("stream.capture.sink", "true");
            if let Some(d) = device {
                props.insert("target.object", d.as_str());
            }
            None
        }
        CaptureEndpoint::Input { device } => {
            if let Some(d) = device {
                props.insert("target.object", d.as_str());
            }
            None
        }
        CaptureEndpoint::Process { pid, include } => {
            // Klip links the app streams itself; the session manager must not.
            props.insert("node.autoconnect", "false");
            Some(Rc::new(Linker {
                core: core.clone(),
                graph: RefCell::new(Graph::default()),
                own: Cell::new(0),
                pid: *pid,
                include: *include,
                tree: RefCell::new(process_tree(*pid)),
            }))
        }
    };
    let stream = pw::stream::StreamBox::new(&core, "Klip audio capture", props).map_err(pw_err)?;
    let _stream_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let (ml, failure, linker) = (mainloop.clone(), failure.clone(), linker.clone());
            move |stream, _, _, new| match new {
                pw::stream::StreamState::Error(e) => {
                    *failure.borrow_mut() = Some(format!("PipeWire stream: {e}"));
                    ml.quit();
                }
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming => {
                    if let Some(l) = &linker {
                        l.own.set(stream.node_id());
                        l.link();
                    }
                }
                _ => {}
            }
        })
        .process({
            let state = state.clone();
            move |stream, _| on_process(stream, fmt, &state)
        })
        .register()
        .map_err(pw_err)?;
    let values = format_param(fmt)?;
    let pod = spa::pod::Pod::from_bytes(&values).ok_or("audio format pod")?;
    let mut params = [pod];
    let mut flags = pw::stream::StreamFlags::MAP_BUFFERS;
    if linker.is_none() {
        flags |= pw::stream::StreamFlags::AUTOCONNECT;
    }
    stream
        .connect(spa::utils::Direction::Input, None, flags, &mut params)
        .map_err(pw_err)?;

    let registry = core.get_registry().map_err(pw_err)?;
    let _registry_listener = linker.as_ref().map(|l| {
        let (added, removed) = (l.clone(), l.clone());
        registry
            .add_listener_local()
            .global(move |g| added.global(g))
            .global_remove(move |id| removed.removed(id))
            .register()
    });

    let timer = mainloop.loop_().add_timer({
        let (ml, state, stop) = (mainloop.clone(), state.clone(), stop.clone());
        move |_| {
            state.borrow_mut().fill_silence(fmt);
            if stop.load(Ordering::Relaxed) {
                ml.quit();
            }
        }
    });
    timer
        .update_timer(Some(TICK), Some(TICK))
        .into_result()
        .map_err(|e| format!("PipeWire timer: {e}"))?;

    mainloop.run();
    let _ = stream.disconnect();
    match failure.borrow_mut().take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// One buffer from PipeWire: stamp it and hand it on.
fn on_process(stream: &pw::stream::Stream, fmt: Format, state: &RefCell<Shared>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    let Some(d) = datas.first_mut() else { return };
    let offset = usize::try_from(d.chunk().offset()).unwrap_or(usize::MAX);
    let size = usize::try_from(d.chunk().size()).unwrap_or(0);
    let Some(bytes) = d.data() else { return };
    let Some(bytes) = offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
    else {
        return;
    };
    let frame_bytes = usize::from(fmt.channels).saturating_mul(4);
    let frames = bytes
        .len()
        .checked_div(frame_bytes)
        .unwrap_or(0)
        .min(MAX_FRAMES);
    if frames == 0 {
        return;
    }
    let start = capture_time(stream);
    let mut s = state.borrow_mut();
    let s = &mut *s;
    s.samples.clear();
    s.samples.extend(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .take(frames.saturating_mul(usize::from(fmt.channels)))
            .map(|b| f32::from_le_bytes(*b)),
    );
    let frames = u32::try_from(frames).unwrap_or(0);
    deliver(
        fmt,
        &mut s.next,
        start,
        frames,
        Some(&s.samples),
        false,
        s.sink.as_mut(),
    );
    s.delivered = true;
}

/// When the first frame of the buffer being processed was captured: the graph cycle's
/// time minus the stream's delay, on CLOCK_MONOTONIC.
fn capture_time(stream: &pw::stream::Stream) -> i64 {
    let Ok(t) = stream.time() else {
        return qpc_now_ns();
    };
    let rate = t.rate();
    let delay_ns = i128::from(t.delay())
        .saturating_mul(1_000_000_000)
        .saturating_mul(i128::from(rate.num))
        .checked_div(i128::from(rate.denom))
        .unwrap_or(0);
    let now = if t.now() > 0 { t.now() } else { qpc_now_ns() };
    i64::try_from(i128::from(now).saturating_sub(delay_ns)).unwrap_or(now)
}

/// How long [`audio_sessions`] waits for PipeWire's answers.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// One playback stream seen by [`audio_sessions`]: its proxy and info listener (kept
/// alive until the answers are in), process id and whether it is running.
struct SeenStream {
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
    client: Option<u32>,
    running: Rc<Cell<bool>>,
}

/// Processes with a playback stream (`Stream/Output/Audio` nodes), one entry each;
/// `active` when one of its streams is running (playing).
pub(crate) fn audio_sessions() -> Result<Vec<crate::AudioSession>, PlatformError> {
    let os = |e: pw::Error| PlatformError::Os(pw_err(e));
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(os)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(os)?;
    let core = context.connect_rc(None).map_err(os)?;
    let registry = core.get_registry_rc().map_err(os)?;
    let seen: Rc<RefCell<Vec<SeenStream>>> = Rc::default();
    let clients: Rc<RefCell<HashMap<u32, u32>>> = Rc::default();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (seen, clients, registry) = (seen.clone(), clients.clone(), registry.downgrade());
            move |g| {
                let Some(p) = g.props else { return };
                if g.type_ == ObjectType::Client
                    && let Some(pid) = client_pid(p)
                {
                    clients.borrow_mut().insert(g.id, pid);
                    return;
                }
                if g.type_ != ObjectType::Node
                    || p.get("media.class") != Some("Stream/Output/Audio")
                {
                    return;
                }
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                let Ok(node) = registry.bind::<pw::node::Node, _>(g) else {
                    return;
                };
                let running = Rc::new(Cell::new(false));
                let listener = node
                    .add_listener_local()
                    .info({
                        let running = running.clone();
                        move |info| {
                            running.set(matches!(info.state(), pw::node::NodeState::Running));
                        }
                    })
                    .register();
                seen.borrow_mut().push(SeenStream {
                    _node: node,
                    _listener: listener,
                    client: node_client(p),
                    running,
                });
            }
        })
        .register();
    // Round trip 1 lists the globals (and binds the streams); round trip 2 delivers the
    // bound streams' info.
    let first = core.sync(0).map_err(os)?;
    let second: Rc<Cell<Option<spa::utils::result::AsyncSeq>>> = Rc::default();
    let done = Rc::new(Cell::new(false));
    let _core_listener = core
        .add_listener_local()
        .done({
            let (ml, core, second, done) =
                (mainloop.clone(), core.clone(), second.clone(), done.clone());
            move |id, seq| {
                if id != pw::core::PW_ID_CORE {
                    return;
                }
                if seq == first {
                    match core.sync(0) {
                        Ok(s) => second.set(Some(s)),
                        Err(_) => ml.quit(),
                    }
                } else if second.get() == Some(seq) {
                    done.set(true);
                    ml.quit();
                }
            }
        })
        .register();
    let timer = mainloop.loop_().add_timer({
        let ml = mainloop.clone();
        move |_| ml.quit()
    });
    timer
        .update_timer(Some(QUERY_TIMEOUT), None)
        .into_result()
        .map_err(|e| PlatformError::Os(format!("PipeWire timer: {e}")))?;
    mainloop.run();
    if !done.get() {
        return Err(PlatformError::Os("PipeWire did not answer".into()));
    }
    let mut out: Vec<crate::AudioSession> = Vec::new();
    let clients = clients.borrow();
    for s in seen.borrow().iter() {
        let Some(&pid) = s.client.and_then(|c| clients.get(&c)) else {
            continue;
        };
        match out.iter_mut().find(|a| a.pid == pid) {
            Some(a) => a.active |= s.running.get(),
            None => out.push(crate::AudioSession {
                pid,
                active: s.running.get(),
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioCapture, AudioPacket};
    use std::sync::Mutex;

    /// Collects samples of the first channel.
    struct Collect(Arc<Mutex<Vec<f32>>>);
    impl AudioCaptureSink for Collect {
        fn packet(&mut self, p: AudioPacket<'_>) {
            let mut v = self.0.lock().unwrap();
            match p.data {
                Some(d) => v.extend(d.iter().step_by(2)),
                None => v.extend(std::iter::repeat_n(0.0, p.frames as usize)),
            }
        }
        fn notice(&mut self, message: String) {
            println!("notice: {message}");
        }
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt()
    }

    fn capture(endpoint: CaptureEndpoint, ms: u64) -> Vec<f32> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let cap = AudioCapture::start(endpoint, 48_000, 2, Box::new(Collect(log.clone()))).unwrap();
        std::thread::sleep(Duration::from_millis(ms));
        cap.stop();
        Arc::try_unwrap(log).unwrap().into_inner().unwrap()
    }

    /// A null sink only this test plays to (nothing audible), kept while the loop thread
    /// lives.
    fn null_sink(name: &'static str) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            pw::init();
            let ml = pw::main_loop::MainLoopRc::new(None).unwrap();
            let ctx = pw::context::ContextRc::new(&ml, None).unwrap();
            let core = ctx.connect_rc(None).unwrap();
            let _node = core
                .create_object::<pw::node::Node>(
                    "adapter",
                    &properties! {
                        "factory.name" => "support.null-audio-sink",
                        "node.name" => name,
                        "media.class" => "Audio/Sink",
                        "audio.position" => "[ FL FR ]",
                        "object.linger" => "false",
                    },
                )
                .unwrap();
            let timer = ml.loop_().add_timer({
                let ml = ml.clone();
                move |_| {
                    if rx.try_recv().is_ok() {
                        ml.quit();
                    }
                }
            });
            timer
                .update_timer(
                    Some(Duration::from_millis(10)),
                    Some(Duration::from_millis(10)),
                )
                .into_result()
                .unwrap();
            ready_tx.send(()).unwrap();
            ml.run();
        });
        ready_rx.recv().unwrap();
        (tx, t)
    }

    #[test]
    #[ignore = "needs a PipeWire session and gst-launch-1.0; run via `cargo xtask e2e`"]
    fn app_streams_are_linked_by_process() {
        let sink = "klip-test-null-sink";
        let (stop_sink, sink_thread) = null_sink(sink);
        std::thread::sleep(Duration::from_millis(300));
        let mut tone = std::process::Command::new("gst-launch-1.0")
            .args([
                "-q",
                "audiotestsrc",
                "freq=1000",
                "volume=0.5",
                "!",
                "pipewiresink",
                &format!("target-object={sink}"),
            ])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
        let pid = tone.id();
        let included = capture(CaptureEndpoint::Process { pid, include: true }, 1500);
        let excluded = capture(
            CaptureEndpoint::Process {
                pid,
                include: false,
            },
            1500,
        );
        let monitor = capture(
            CaptureEndpoint::Loopback {
                device: Some(sink.into()),
            },
            1500,
        );
        let sessions = audio_sessions().unwrap();
        let _ = tone.kill();
        let _ = tone.wait();
        let _ = stop_sink.send(());
        let _ = sink_thread.join();
        // Skip the first 200 ms (linking).
        let tail = |v: &[f32]| v.get(9_600..).map(rms).unwrap_or(0.0);
        let (inc, exc, mon) = (tail(&included), tail(&excluded), tail(&monitor));
        println!("rms: included {inc:.3}, excluded {exc:.3}, sink monitor {mon:.3}");
        println!("sessions: {sessions:?}");
        // A 0.5 sine has RMS 0.354.
        assert!(inc > 0.3, "the tone's own stream was not captured: {inc}");
        assert!(
            mon > 0.3,
            "the sink's monitor did not carry the tone: {mon}"
        );
        assert!(
            exc < 0.3 * inc,
            "excluding the tone's process still captured it: {exc}"
        );
        assert!(
            sessions.iter().any(|s| s.pid == pid && s.active),
            "{sessions:?}"
        );
    }
}
