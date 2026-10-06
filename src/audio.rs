use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::stream::{
    backoff_for_attempt, open_stream, IcyDemuxer, StreamError, StreamSource, MAX_RECONNECT_ATTEMPTS,
};

/// Visualizer geometry shared with the app/UI layer.
pub const N_BANDS: usize = 32;
pub const N_WAVE: usize = 48;
const FFT_SIZE: usize = 1024;

/// Simple control API for the rest of the application (spec §8).
/// The TUI/app layer only talks to this trait + the command channel.
pub trait AudioPlayer: Send {
    fn play(&mut self, stream: StreamSource);
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn set_volume(&mut self, volume: f32);
}

/// Events flowing from the audio worker back to the app (reconnection, metadata).
#[derive(Debug, Clone)]
pub enum AudioEvent {
    Connected { icy_name: Option<String> },
    Playing,
    Disconnected { reason: String, attempt: u32 },
    Failed { reason: String },
    Metadata { title: String },
    /// Live analysis at ~10Hz: 32 normalized spectrum bands + 48 waveform points.
    Levels { bands: Vec<f32>, wave: Vec<f32> },
}

#[derive(Debug, Clone)]
pub enum AudioCommand {
    Play(StreamSource),
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
}

/// Shared playback state owned by the worker task.
#[derive(Debug, Default)]
pub struct PlaybackStatus {
    pub playing: bool,
    pub paused: bool,
    pub volume: f32,
    pub current_url: Option<String>,
}

/// The worker: owns networking + decoding + output, runs off the TUI thread.
/// Reconnection uses the spec backoff (2/5/10/20/30s) without blocking the UI:
/// progress is reported via `AudioEvent`s.
pub struct AudioWorker {
    pub tx_cmd: mpsc::UnboundedSender<AudioCommand>,
    pub rx_event: mpsc::UnboundedReceiver<AudioEvent>,
    pub status: Arc<Mutex<PlaybackStatus>>,
}

impl AudioWorker {
    pub fn spawn() -> Self {
        let (tx_cmd, rx_cmd) = mpsc::unbounded_channel::<AudioCommand>();
        let (tx_event, rx_event) = mpsc::unbounded_channel::<AudioEvent>();
        let status = Arc::new(Mutex::new(PlaybackStatus {
            volume: crate::config::DEFAULT_VOLUME,
            ..Default::default()
        }));
        tokio::spawn(run_worker(rx_cmd, tx_event, status.clone()));
        Self {
            tx_cmd,
            rx_event,
            status,
        }
    }

    pub fn send(&self, cmd: AudioCommand) {
        let _ = self.tx_cmd.send(cmd);
    }

    /// Non-blocking drain of pending worker events; the TUI calls this each frame.
    pub fn drain_events(&mut self) -> Vec<AudioEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx_event.try_recv() {
            out.push(ev);
        }
        out
    }
}

async fn run_worker(
    mut rx_cmd: mpsc::UnboundedReceiver<AudioCommand>,
    tx_event: mpsc::UnboundedSender<AudioEvent>,
    status: Arc<Mutex<PlaybackStatus>>,
) {
    let mut stream_task: Option<JoinHandle<()>> = None;
    let output = StreamOutput::new();

    loop {
        let cmd = rx_cmd.recv().await;
        let Some(cmd) = cmd else { break };
        match cmd {
            AudioCommand::Play(src) => {
                // Stop any previous stream.
                if let Some(h) = stream_task.take() {
                    h.abort();
                }
                {
                    let mut st = status.lock().unwrap();
                    st.playing = true;
                    st.paused = false;
                    st.current_url = Some(src.url.clone());
                }
                // A fresh Play always resumes audible output (Enter while
                // paused starts the newly selected station, not silence).
                output.set_paused(false);
                let tx = tx_event.clone();
                let st = status.clone();
                let out = output.clone();
                stream_task = Some(tokio::spawn(play_stream_loop(src, tx, st, out)));
            }
            AudioCommand::Pause => {
                if let Ok(mut st) = status.lock() {
                    st.paused = true;
                }
                output.set_paused(true);
            }
            AudioCommand::Resume => {
                if let Ok(mut st) = status.lock() {
                    st.paused = false;
                }
                output.set_paused(false);
            }
            AudioCommand::Stop => {
                if let Some(h) = stream_task.take() {
                    h.abort();
                }
                if let Ok(mut st) = status.lock() {
                    st.playing = false;
                    st.paused = false;
                    st.current_url = None;
                }
                output.stop();
            }
            AudioCommand::SetVolume(v) => {
                let v = v.clamp(0.0, 1.0);
                if let Ok(mut st) = status.lock() {
                    st.volume = v;
                }
                output.set_volume(v);
            }
        }
    }
}

/// Continuous streaming loop with reconnect backoff. Emits AudioEvents.
/// Cancelled via task abort on Play/Stop.
async fn play_stream_loop(
    src: StreamSource,
    tx_event: mpsc::UnboundedSender<AudioEvent>,
    status: Arc<Mutex<PlaybackStatus>>,
    output: StreamOutput,
) {
    let mut attempt: u32 = 0;
    loop {
        if !is_alive(&status, &src) {
            break;
        }
        match open_stream(&src).await {
            Ok((stream, metaint, icy_name)) => {
                attempt = 0;
                let _ = tx_event.send(AudioEvent::Connected { icy_name });
                match consume_stream(stream, metaint, &tx_event, &status, &output).await {
                    Ok(()) => {
                        // Server closed the stream; reconnect.
                        attempt += 1;
                        if attempt > MAX_RECONNECT_ATTEMPTS {
                            let _ = tx_event.send(AudioEvent::Failed {
                                reason: "Stream ended.".to_string(),
                            });
                            break;
                        }
                        let _ = tx_event.send(AudioEvent::Disconnected {
                            reason: "Stream ended.".to_string(),
                            attempt,
                        });
                        tokio::time::sleep(backoff_for_attempt(attempt)).await;
                    }
                    Err(e) => {
                        let reason = stream_error_message(&e);
                        attempt += 1;
                        if attempt > MAX_RECONNECT_ATTEMPTS {
                            let _ = tx_event.send(AudioEvent::Failed { reason });
                            break;
                        }
                        let _ = tx_event.send(AudioEvent::Disconnected { reason, attempt });
                        tokio::time::sleep(backoff_for_attempt(attempt)).await;
                    }
                }
            }
            Err(e) => {
                let reason = stream_error_message(&e);
                attempt += 1;
                if attempt > MAX_RECONNECT_ATTEMPTS {
                    let _ = tx_event.send(AudioEvent::Failed { reason });
                    break;
                }
                let _ = tx_event.send(AudioEvent::Disconnected { reason, attempt });
                tokio::time::sleep(backoff_for_attempt(attempt)).await;
            }
        }
    }
}

fn is_alive(status: &Arc<Mutex<PlaybackStatus>>, src: &StreamSource) -> bool {
    status
        .lock()
        .map(|st| st.playing && st.current_url.as_deref() == Some(src.url.as_str()))
        .unwrap_or(false)
}

/// Consume the byte stream: demux ICY metadata, forward titles as events,
/// and push audio bytes into a blocking pipe decoded once, continuously,
/// by Symphonia. The decode thread taps PCM samples for the visualizer
/// (FFT → `AudioEvent::Levels`) and forwards them to the output backend.
/// Resolves when the stream ends or fails. While paused, bytes are
/// discarded (live radio rejoins the live edge on resume).
async fn consume_stream<S>(
    stream: S,
    metaint: usize,
    tx_event: &mpsc::UnboundedSender<AudioEvent>,
    status: &Arc<Mutex<PlaybackStatus>>,
    output: &StreamOutput,
) -> Result<(), StreamError>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    use futures_util::StreamExt;
    let mut stream = stream;
    let mut demux = IcyDemuxer::new(metaint);
    let mut first_audio = true;

    // One continuous decode pipeline per connection: network bytes in,
    // PCM samples + spectrum levels out. Decoding always runs (even on
    // silent builds / no-device machines) so the visualizer has signal.
    let (writer, reader) = byte_pipe();
    let sound = output.sound_enabled();
    let pcm = sound.then(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PcmChunk>(16);
        output.play(rx);
        tx
    });
    let _decode = spawn_decode_thread(reader, pcm, tx_event.clone());

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            StreamError::Lost(if e.is_timeout() {
                "Connection timed out.".to_string()
            } else {
                format!("Connection lost: {e}")
            })
        })?;
        let (audio, titles) = demux.push(&chunk);
        for t in titles {
            let _ = tx_event.send(AudioEvent::Metadata { title: t });
        }
        let (paused, alive) = status
            .lock()
            .map(|s| (s.paused, s.playing))
            .unwrap_or((false, false));
        if !alive {
            break;
        }
        if paused {
            continue;
        }
        if !audio.is_empty() {
            if first_audio {
                first_audio = false;
                let _ = tx_event.send(AudioEvent::Playing);
            }
            // Never blocks the async worker: the pipe drops oldest bytes
            // when full, staying near the live edge.
            writer.push(&audio);
        }
    }
    // EOF to the decoder (its thread exits on its own), silence the sink.
    writer.close();
    if sound {
        output.stop();
    }
    Ok(())
}

fn stream_error_message(e: &StreamError) -> String {
    match e {
        StreamError::Connect(s) | StreamError::Lost(s) | StreamError::Decode(s) => s.clone(),
    }
}

// ---------------------------------------------------------------------------
// Audio output. The rest of the app never depends on a specific audio crate:
// network bytes flow into a blocking pipe, one Symphonia decode thread per
// connection turns them into PCM (tapping spectrum levels for the UI), and
// PCM flows to the backend below.
//
// - With `--features audio`: real output via rodio (streaming Source).
// - Without: decode + levels still run (pure Rust), samples are dropped —
//   builds anywhere and the visualizer still has honest signal.
// ---------------------------------------------------------------------------

/// One decoded PCM chunk: interleaved f32 samples plus the stream spec.
/// The spec rides along so the output can configure itself from the first
/// chunk instead of assuming 44.1kHz stereo.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
struct PcmChunk {
    samples: Vec<f32>,
    rate: u32,
    channels: u16,
}

type PcmReceiver = std::sync::mpsc::Receiver<PcmChunk>;

/// Handle to the output backend actor (a dedicated thread owning rodio).
/// Cloned into each stream task; all methods are fire-and-forget.
#[derive(Clone)]
struct StreamOutput {
    #[cfg(feature = "audio")]
    feeder: std::sync::mpsc::Sender<FeederCtl>,
}

impl StreamOutput {
    fn new() -> Self {
        #[cfg(feature = "audio")]
        {
            let (tx, rx) = std::sync::mpsc::channel::<FeederCtl>();
            std::thread::spawn(move || rodio_backend::feeder_main(rx));
            Self { feeder: tx }
        }
        #[cfg(not(feature = "audio"))]
        {
            Self {}
        }
    }

    /// True when decoded samples should go somewhere audible.
    fn sound_enabled(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            rodio_backend::probe()
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = self;
            false
        }
    }

    fn play(&self, rx: PcmReceiver) {
        #[cfg(feature = "audio")]
        {
            let _ = self.feeder.send(FeederCtl::Play { rx });
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = (self, rx);
        }
    }

    fn stop(&self) {
        #[cfg(feature = "audio")]
        {
            let _ = self.feeder.send(FeederCtl::Stop);
        }
    }

    fn set_volume(&self, volume: f32) {
        #[cfg(feature = "audio")]
        {
            let _ = self.feeder.send(FeederCtl::Volume(volume));
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = (self, volume);
        }
    }

    fn set_paused(&self, paused: bool) {
        #[cfg(feature = "audio")]
        {
            let _ = self.feeder.send(FeederCtl::Paused(paused));
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = (self, paused);
        }
    }
}

// --- blocking byte pipe: async writer, blocking Symphonia reader ---

const PIPE_CAP: usize = 512 * 1024;

struct PipeShared {
    buf: VecDeque<u8>,
    closed: bool,
}

struct PipeWriter {
    shared: Arc<(Mutex<PipeShared>, std::sync::Condvar)>,
}

struct PipeReader {
    shared: Arc<(Mutex<PipeShared>, std::sync::Condvar)>,
}

fn byte_pipe() -> (PipeWriter, PipeReader) {
    let shared = Arc::new((
        Mutex::new(PipeShared {
            buf: VecDeque::new(),
            closed: false,
        }),
        std::sync::Condvar::new(),
    ));
    (
        PipeWriter {
            shared: shared.clone(),
        },
        PipeReader { shared },
    )
}

impl PipeWriter {
    /// Non-blocking push; drops oldest bytes when full to stay live.
    /// Never panics on a poisoned mutex: worst case the chunk is dropped
    /// (the MP3 demuxer resyncs) and the stream continues.
    fn push(&self, bytes: &[u8]) {
        let Ok(mut g) = self.shared.0.lock() else { return };
        if g.closed {
            return;
        }
        g.buf.extend(bytes);
        let excess = g.buf.len().saturating_sub(PIPE_CAP);
        if excess > 0 {
            g.buf.drain(..excess);
        }
        self.shared.1.notify_one();
    }

    fn close(&self) {
        if let Ok(mut g) = self.shared.0.lock() {
            g.closed = true;
        }
        self.shared.1.notify_all();
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        // Guarantees the decode thread sees EOF even on task abort.
        self.close();
    }
}

impl std::io::Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut g = self.shared.0.lock().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe lock poisoned")
        })?;
        while g.buf.is_empty() && !g.closed {
            g = self.shared.1.wait(g).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe lock poisoned")
            })?;
        }
        if g.buf.is_empty() {
            return Ok(0); // closed + drained = EOF
        }
        let n = out.len().min(g.buf.len());
        for (slot, byte) in out[..n].iter_mut().zip(g.buf.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl symphonia::core::io::MediaSource for PipeReader {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl std::io::Seek for PipeReader {
    fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
        // Non-seekable live stream; readers check `is_seekable()` first.
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "stream is not seekable",
        ))
    }
}

#[cfg(feature = "audio")]
enum FeederCtl {
    /// New live PCM stream. The feeder waits briefly for the first chunk
    /// (which carries the stream spec), clears stale audio, and appends.
    Play { rx: PcmReceiver },
    Stop,
    Volume(f32),
    Paused(bool),
}

#[cfg(feature = "audio")]
mod rodio_backend {
    use super::{FeederCtl, PcmChunk, PcmReceiver};
    use std::sync::OnceLock;
    use std::time::Duration;

    static AVAILABLE: OnceLock<bool> = OnceLock::new();

    /// Probe once per process whether a real output device exists.
    /// Must be called before entering the alternate screen: probing a
    /// missing device makes ALSA print errors to stderr, which corrupts
    /// TUI escape sequences if interleaved with drawing.
    pub fn probe() -> bool {
        *AVAILABLE.get_or_init(|| rodio::OutputStream::try_default().is_ok())
    }

    /// Owns the rodio output stream + sink. Never touches the network;
    /// it only turns PCM chunks into sound, on command.
    pub fn feeder_main(rx: std::sync::mpsc::Receiver<FeederCtl>) {
        if !probe() {
            // No sound card (CI / WSL): drain commands, decode still
            // feeds the visualizer from the worker thread.
            while rx.recv().is_ok() {}
            return;
        }
        let (_stream, handle) = match rodio::OutputStream::try_default() {
            Ok(x) => x,
            Err(_) => {
                while rx.recv().is_ok() {}
                return;
            }
        };
        let sink = match rodio::Sink::try_new(&handle) {
            Ok(s) => s,
            Err(_) => {
                while rx.recv().is_ok() {}
                return;
            }
        };
        let mut volume = 0.7f32;
        sink.set_volume(volume);
        while let Ok(ctl) = rx.recv() {
            match ctl {
                FeederCtl::Play { rx: pcm_rx } => {
                    // The first chunk tells us rate/channels; without it
                    // there is nothing to play (stream died early).
                    match pcm_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(first) => {
                            sink.clear();
                            sink.append(PcmSource::new_with_first(pcm_rx, first));
                            sink.set_volume(volume);
                            sink.play();
                        }
                        Err(_) => continue,
                    }
                }
                FeederCtl::Stop => {
                    sink.clear();
                }
                FeederCtl::Volume(v) => {
                    volume = v;
                    sink.set_volume(v);
                }
                FeederCtl::Paused(true) => sink.pause(),
                FeederCtl::Paused(false) => sink.play(),
            }
        }
    }

    /// A rodio `Source` pulling live PCM chunks off a channel.
    /// Starvation yields silence (never blocks the audio thread);
    /// a dropped sender ends the source so the sink goes idle.
    struct PcmSource {
        rx: PcmReceiver,
        cur: std::vec::IntoIter<f32>,
        channels: u16,
        rate: u32,
    }

    impl PcmSource {
        fn new_with_first(rx: PcmReceiver, first: PcmChunk) -> Self {
            let channels = first.channels;
            let rate = first.rate;
            Self {
                rx,
                cur: first.samples.into_iter(),
                channels,
                rate,
            }
        }
    }

    impl Iterator for PcmSource {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            loop {
                if let Some(s) = self.cur.next() {
                    return Some(s);
                }
                match self.rx.try_recv() {
                    Ok(chunk) => {
                        if chunk.samples.is_empty() {
                            continue;
                        }
                        self.cur = chunk.samples.into_iter();
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Some(0.0),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
                }
            }
        }
    }

    impl rodio::Source for PcmSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> u16 {
            self.channels
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }
}

// --- spectrum analysis: Hann-windowed FFT, log bands, waveform tap ---
// Hand-rolled radix-2 FFT (no new dependencies): 1024-point at ~10Hz is
// trivial load, and it keeps the V1 dependency list unchanged.

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && n == im.len());
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j &= !bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Iterative butterflies.
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wpr, wpi) = (ang.cos(), ang.sin());
        let half = len >> 1;
        let mut i = 0;
        while i < n {
            let (mut wr, mut wi) = (1.0f32, 0.0f32);
            for k in 0..half {
                let a = i + k;
                let b = a + half;
                let tr = wr * re[b] - wi * im[b];
                let ti = wr * im[b] + wi * re[b];
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let t = wr;
                wr = wr * wpr - wi * wpi;
                wi = t * wpi + wi * wpr;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Rolling level state: slow-release AGC plus per-bar smoothing so the
/// bars feel alive instead of jittery.
struct LevelState {
    agc: f32,
    bars: [f32; N_BANDS],
}

impl LevelState {
    fn new() -> Self {
        Self {
            agc: 1e-3,
            bars: [0.0; N_BANDS],
        }
    }

    /// `mono` is the recent mono samples (any length); `rate` the stream
    /// sample rate. Returns normalized bands + waveform points, all 0..1
    /// (waveform is -1..1).
    fn update(&mut self, mono: &[f32], rate: u32) -> ([f32; N_BANDS], [f32; N_WAVE]) {
        let rate = rate.max(8000) as f32;

        // Last FFT_SIZE samples, Hann-windowed, zero-padded when short.
        let mut re = [0.0f32; FFT_SIZE];
        let mut im = [0.0f32; FFT_SIZE];
        let start = mono.len().saturating_sub(FFT_SIZE);
        let got = mono.len() - start;
        for (i, s) in mono[start..].iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos();
            re[i] = s * w;
        }
        let _ = got;
        fft(&mut re, &mut im);

        // Log-spaced bands, 60Hz .. 16kHz (capped below Nyquist).
        let f_min = 60.0f32;
        let f_max = (16_000.0f32).min(rate * 0.45);
        let ratio = f_max / f_min;
        let mut mags = [0.0f32; N_BANDS];
        let mut counts = [0usize; N_BANDS];
        let bins = FFT_SIZE / 2;
        for j in 1..bins {
            let hz = j as f32 * rate / FFT_SIZE as f32;
            if hz < f_min || hz > f_max {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let b = ((hz / f_min).ln() / ratio.ln() * N_BANDS as f32) as usize;
            let b = b.min(N_BANDS - 1);
            let mag = (re[j] * re[j] + im[j] * im[j]).sqrt() / FFT_SIZE as f32;
            mags[b] += mag;
            counts[b] += 1;
        }
        for b in 0..N_BANDS {
            if counts[b] > 0 {
                mags[b] /= counts[b] as f32;
            }
        }

        // AGC: instant attack, slow release. Silent input → bars relax to 0.
        let frame_max = mags.iter().cloned().fold(0.0f32, f32::max);
        if frame_max > self.agc {
            self.agc = frame_max;
        } else {
            self.agc = (self.agc * 0.985).max(1e-4);
        }
        let mut out = [0.0f32; N_BANDS];
        for b in 0..N_BANDS {
            let target = (mags[b] / self.agc).clamp(0.0, 1.0);
            let cur = self.bars[b];
            // Fast attack, lazy release: punchy but stable.
            self.bars[b] = cur + (target - cur) * if target > cur { 0.55 } else { 0.12 };
            out[b] = self.bars[b];
        }

        // Waveform: last 96 mono samples decimated to N_WAVE, peak-normalized.
        let mut wave = [0.0f32; N_WAVE];
        let wstart = mono.len().saturating_sub(N_WAVE * 2);
        let slice = &mono[wstart..];
        let peak = slice.iter().cloned().fold(0.0f32, |a, s| a.max(s.abs())).max(1e-4);
        for (i, w) in wave.iter_mut().enumerate() {
            let idx = (i * 2).min(slice.len().saturating_sub(1));
            *w = slice.get(idx).copied().unwrap_or(0.0) / peak;
        }
        (out, wave)
    }
}

// --- continuous decode thread: pipe bytes → PCM + levels ---

/// Decode one connection end-to-end on a plain thread (Symphonia reads
/// block on the pipe). Ends when the pipe hits EOF or the PCM consumer
/// goes away. Levels flow at ~10Hz regardless of output backend.
fn spawn_decode_thread(
    reader: PipeReader,
    pcm_tx: Option<std::sync::mpsc::SyncSender<PcmChunk>>,
    levels_tx: mpsc::UnboundedSender<AudioEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || decode_loop(reader, pcm_tx, levels_tx))
}

fn decode_loop(
    reader: PipeReader,
    mut pcm_tx: Option<std::sync::mpsc::SyncSender<PcmChunk>>,
    levels_tx: mpsc::UnboundedSender<AudioEvent>,
) {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use symphonia::core::audio::SampleBuffer;

    let mss = MediaSourceStream::new(Box::new(reader), Default::default());
    // Empty hint: sniff the container (mp3 and AAC streams both occur).
    let hint = Hint::new();
    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut format = probed.format;
    let track = match format.default_track() {
        Some(t) => t.clone(),
        None => return,
    };
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let mut decoder = match symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
    {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut buf_key: Option<(u32, usize)> = None;
    let mut sample_capacity: usize = 0;
    let mut mono_win: VecDeque<f32> = VecDeque::with_capacity(4096);
    let mut levels = LevelState::new();
    let mut since_levels = 0u32;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // EOF (pipe closed + drained) or unrecoverable read error.
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue, // bad frame: skip, demuxer resyncs
            Err(Error::ResetRequired) => {
                // Mid-stream format change: rebuild the decoder.
                match symphonia::default::get_codecs()
                    .make(&codec_params, &DecoderOptions::default())
                {
                    Ok(d) => decoder = d,
                    Err(_) => break,
                }
                continue;
            }
            Err(_) => break,
        };

        let spec = *decoded.spec();
        let frames = decoded.frames();
        let channels = spec.channels.count();
        let key = (spec.rate, channels);
        let changed = buf_key != Some(key);
        if changed || sample_capacity < frames {
            sample_buf = Some(SampleBuffer::<f32>::new(frames as u64, spec));
            buf_key = Some(key);
            sample_capacity = frames;
        }
        let buf = sample_buf.as_mut().expect("sample buffer allocated above");
        buf.copy_interleaved_ref(decoded);
        let interleaved = buf.samples();

        // Downmix to mono for analysis; keep interleaved for output.
        for f in 0..frames {
            let mut acc = 0.0f32;
            for c in 0..channels {
                acc += interleaved.get(f * channels + c).copied().unwrap_or(0.0);
            }
            mono_win.push_back(acc / channels.max(1) as f32);
        }
        while mono_win.len() > 4096 {
            mono_win.pop_front();
        }

        // Forward PCM. Blocking send = backpressure to the pipe (which
        // drops oldest bytes when full). A dead receiver means the
        // connection moved on: stop sending, keep analyzing briefly.
        if let Some(tx) = pcm_tx.as_ref() {
            let chunk = PcmChunk {
                samples: interleaved.to_vec(),
                rate: spec.rate,
                channels: channels.min(u16::MAX as usize) as u16,
            };
            if tx.send(chunk).is_err() {
                pcm_tx = None;
            }
        }

        // ~10Hz analysis (a packet is ~25ms of audio).
        since_levels += 1;
        if since_levels >= 4 {
            since_levels = 0;
            let mono: Vec<f32> = mono_win.iter().copied().collect();
            let (bands, wave) = levels.update(&mono, spec.rate);
            let _ = levels_tx.send(AudioEvent::Levels {
                bands: bands.to_vec(),
                wave: wave.to_vec(),
            });
        }
    }
}

/// Probe for a real audio output device.
/// - `None`: silent build (no `audio` feature) — no device needed.
/// - `Some(true/false)`: rodio probe result. Call before entering the
///   alternate screen so ALSA errors land on the normal terminal.
pub fn probe_audio_output() -> Option<bool> {
    #[cfg(feature = "audio")]
    {
        Some(rodio_backend::probe())
    }
    #[cfg(not(feature = "audio"))]
    {
        None
    }
}

/// In-process player used by tests and headless runs.
pub struct NullPlayer {
    pub status: PlaybackStatus,
}

impl NullPlayer {
    pub fn new() -> Self {
        Self {
            status: PlaybackStatus::default(),
        }
    }
}

impl Default for NullPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayer for NullPlayer {
    fn play(&mut self, stream: StreamSource) {
        self.status.playing = true;
        self.status.paused = false;
        self.status.current_url = Some(stream.url);
    }
    fn pause(&mut self) {
        self.status.paused = true;
    }
    fn resume(&mut self) {
        self.status.paused = false;
    }
    fn stop(&mut self) {
        self.status.playing = false;
        self.status.paused = false;
        self.status.current_url = None;
    }
    fn set_volume(&mut self, volume: f32) {
        self.status.volume = volume.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_player_state_machine() {
        let mut p = NullPlayer::new();
        p.play(StreamSource::new("https://example.com/s"));
        assert!(p.status.playing);
        p.pause();
        assert!(p.status.paused);
        p.resume();
        assert!(!p.status.paused);
        p.set_volume(2.0);
        assert!((p.status.volume - 1.0).abs() < f32::EPSILON);
        p.stop();
        assert!(!p.status.playing);
    }

    #[test]
    fn fft_finds_sine_bin() {
        // 440Hz at 44.1kHz, N=1024 → peak near bin 10.
        let mut re = [0.0f32; 64];
        let mut im = [0.0f32; 64];
        for (i, s) in re.iter_mut().enumerate() {
            // 4 cycles over 64 samples → bin 4.
            *s = (2.0 * std::f32::consts::PI * 4.0 * i as f32 / 64.0).sin();
        }
        fft(&mut re, &mut im);
        let mut best = 0;
        let mut best_mag = 0.0;
        for i in 0..32 {
            let mag = re[i] * re[i] + im[i] * im[i];
            if mag > best_mag {
                best_mag = mag;
                best = i;
            }
        }
        assert_eq!(best, 4, "sine peak should land on bin 4");
    }

    #[test]
    fn levels_are_normalized_bands() {
        // Loud 440Hz mono tone → bands in 0..1 with energy somewhere.
        let rate = 44_100u32;
        let mono: Vec<f32> = (0..2048)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin())
            .collect();
        let mut st = LevelState::new();
        let (bands, wave) = st.update(&mono, rate);
        assert_eq!(bands.len(), N_BANDS);
        assert_eq!(wave.len(), N_WAVE);
        assert!(bands.iter().all(|b| (0.0..=1.0).contains(b)));
        assert!(wave.iter().all(|w| (-1.0..=1.0).contains(w)));
        assert!(
            bands.iter().cloned().fold(0.0f32, f32::max) > 0.3,
            "tone should light up some bands: {bands:?}"
        );
    }

    #[test]
    fn silence_relaxes_to_zero() {
        let mut st = LevelState::new();
        let tone: Vec<f32> = (0..2048)
            .map(|i| (i as f32 * 0.11).sin())
            .collect();
        let _ = st.update(&tone, 44_100);
        // Long silence: AGC releases and bars decay to ~0.
        let mut last = [1.0f32; N_BANDS];
        for _ in 0..400 {
            let (bands, _) = st.update(&[0.0; 2048], 44_100);
            last = bands;
        }
        assert!(
            last.iter().all(|b| *b < 0.05),
            "bars should relax, got {last:?}"
        );
    }
}
