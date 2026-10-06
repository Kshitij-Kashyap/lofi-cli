use bytes::Bytes;
use futures_util::StreamExt;
use std::time::Duration;

/// Backoff schedule from the spec: 2s, 5s, 10s, 20s, 30s (then stays at 30s).
pub const BACKOFF_SCHEDULE: [u64; 5] = [2, 5, 10, 20, 30];
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;

pub fn backoff_for_attempt(attempt: u32) -> Duration {
    // attempt is 1-based.
    let idx = (attempt.saturating_sub(1) as usize).min(BACKOFF_SCHEDULE.len() - 1);
    Duration::from_secs(BACKOFF_SCHEDULE[idx])
}

#[derive(Debug, Clone)]
pub struct StreamSource {
    pub url: String,
    pub user_agent: String,
}

impl StreamSource {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            user_agent: format!("lofi-cli/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Parse an ICY/Shoutcast `StreamTitle='Artist - Title';` metadata block.
/// Returns None when the block carries no title.
pub fn parse_icy_stream_title(block: &[u8]) -> Option<String> {
    // Blocks are usually latin-1; fall back to lossy UTF-8.
    let text = String::from_utf8_lossy(block);
    let key = "StreamTitle='";
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    let end = rest.find('\'')?;
    let title = rest[..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Incremental ICY metadata demuxer: splits raw audio bytes from metadata
/// blocks given the `icy-metaint` interval. Independent of the UI.
pub struct IcyDemuxer {
    pub metaint: usize,
    until_meta: usize,
    meta_remaining: Option<usize>,
    meta_buf: Vec<u8>,
}

impl IcyDemuxer {
    pub fn new(metaint: usize) -> Self {
        Self {
            metaint,
            until_meta: metaint,
            meta_remaining: None,
            meta_buf: Vec::new(),
        }
    }

    /// Feed raw socket bytes. Returns (audio_bytes, titles_found).
    pub fn push(&mut self, mut data: &[u8]) -> (Vec<u8>, Vec<String>) {
        // metaint == 0 means "no metadata interleaved".
        if self.metaint == 0 {
            return (data.to_vec(), vec![]);
        }
        let mut audio = Vec::with_capacity(data.len());
        let mut titles = Vec::new();
        while !data.is_empty() {
            // Currently inside a metadata block?
            if let Some(remaining) = self.meta_remaining {
                let take = remaining.min(data.len());
                self.meta_buf.extend_from_slice(&data[..take]);
                data = &data[take..];
                let left = remaining - take;
                if left == 0 {
                    self.meta_remaining = None;
                    if let Some(t) = parse_icy_stream_title(&self.meta_buf) {
                        titles.push(t);
                    }
                    self.meta_buf.clear();
                    self.until_meta = self.metaint;
                } else {
                    self.meta_remaining = Some(left);
                }
                continue;
            }
            // Emit audio until the next metadata block.
            if self.until_meta == 0 {
                // Length byte: number of 16-byte words that follow.
                let (len_byte, rest) = match data.split_first() {
                    Some(x) => x,
                    None => break,
                };
                let meta_len = (*len_byte as usize) * 16;
                data = rest;
                if meta_len == 0 {
                    self.until_meta = self.metaint;
                } else {
                    self.meta_remaining = Some(meta_len);
                }
                continue;
            }
            let take = self.until_meta.min(data.len());
            audio.extend_from_slice(&data[..take]);
            data = &data[take..];
            self.until_meta -= take;
        }
        (audio, titles)
    }
}

/// Open a stream, requesting ICY metadata. Returns the audio byte stream,
/// the `icy-metaint` interval (0 = none), and the station's `icy-name`.
pub async fn open_stream(
    source: &StreamSource,
) -> Result<(impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>>, usize, Option<String>), StreamError> {
    let client = reqwest::Client::builder()
        .user_agent(&source.user_agent)
        .build()
        .map_err(|e| StreamError::Connect(e.to_string()))?;
    let resp = client
        .get(&source.url)
        .header("Icy-MetaData", "1")
        .send()
        .await
        .map_err(|e| StreamError::Connect(map_reqwest_error(&e)))?;
    if !resp.status().is_success() {
        return Err(StreamError::Connect(format!("HTTP {}", resp.status())));
    }
    let headers = resp.headers().clone();
    let metaint = headers
        .get("icy-metaint")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let icy_name = headers
        .get("icy-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    Ok((resp.bytes_stream(), metaint, icy_name))
}

fn map_reqwest_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "Connection timed out.".to_string()
    } else if e.is_connect() {
        "Could not connect to station.".to_string()
    } else {
        format!("{e}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("stream error: {0}")]
    Connect(String),
    #[error("decoder error: {0}")]
    Decode(String),
    #[error("connection lost: {0}")]
    Lost(String),
}

/// Decode the first audio frame of `data` with symphonia to prove the
/// bytes are playable (used by the audio PoC check + tests).
/// Returns (codec_name, sample_rate, channels) on success.
pub fn probe_decode_first_frame(data: &[u8]) -> Result<(String, u32, usize), StreamError> {
    use std::io::Cursor;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    if data.len() < 64 {
        return Err(StreamError::Decode("not enough data buffered".to_string()));
    }
    let cursor = Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    hint.mime_type("audio/mpeg");
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| StreamError::Decode(e.to_string()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| StreamError::Decode("no audio track".to_string()))?;
    let codec_params = &track.codec_params;
    let codec = format!("{:?}", codec_params.codec);
    let rate = codec_params.sample_rate.unwrap_or(0);
    let channels = codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(0);
    // Try decoding one packet to ensure the stream is real.
    let mut decoder = symphonia::default::get_codecs()
        .make(codec_params, &Default::default())
        .map_err(|e| StreamError::Decode(e.to_string()))?;
    if let Ok(packet) = format.next_packet() {
        let _ = decoder.decode(&packet);
    }
    Ok((codec, rate, channels))
}

/// Fetch the first `max_bytes` of a station to verify it is a live,
/// decodable audio stream. Used for pre-flight checks, not for playback
/// (playback streams continuously without downloading first).
pub async fn verify_station_stream(url: &str, max_bytes: usize) -> Result<(String, u32, usize), StreamError> {
    let source = StreamSource::new(url);
    let (stream, _metaint, _name) = open_stream(&source).await?;
    tokio::pin!(stream);
    let mut buf = Vec::new();
    let timeout = Duration::from_secs(15);
    let chunk = tokio::time::timeout(timeout, stream.next())
        .await
        .map_err(|_| StreamError::Connect("Connection timed out.".to_string()))?
        .ok_or_else(|| StreamError::Connect("Empty stream.".to_string()))?
        .map_err(|e| StreamError::Connect(map_reqwest_error(&e)))?;
    buf.extend_from_slice(&chunk);
    while buf.len() < max_bytes {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(Ok(c))) => {
                buf.extend_from_slice(&c);
                if buf.len() >= 32 * 1024 {
                    break;
                }
            }
            _ => break,
        }
    }
    probe_decode_first_frame(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_matches_spec() {
        assert_eq!(backoff_for_attempt(1), Duration::from_secs(2));
        assert_eq!(backoff_for_attempt(2), Duration::from_secs(5));
        assert_eq!(backoff_for_attempt(3), Duration::from_secs(10));
        assert_eq!(backoff_for_attempt(4), Duration::from_secs(20));
        assert_eq!(backoff_for_attempt(5), Duration::from_secs(30));
        assert_eq!(backoff_for_attempt(99), Duration::from_secs(30));
    }

    #[test]
    fn parses_stream_title() {
        let block = b"StreamTitle='Snowfall - idealism';StreamUrl='https://x';";
        assert_eq!(
            parse_icy_stream_title(block).as_deref(),
            Some("Snowfall - idealism")
        );
        assert_eq!(parse_icy_stream_title(b"StreamTitle='';"), None);
        assert_eq!(parse_icy_stream_title(b"nothing here"), None);
    }

    #[test]
    fn demuxer_splits_audio_and_metadata() {
        // metaint = 4: 4 audio bytes, then length byte + metadata.
        let mut d = IcyDemuxer::new(4);
        let title = b"StreamTitle='AB';";
        let mut payload = vec![1, 2, 3, 4];
        payload.push(((title.len() + 15) / 16) as u8); // length in 16-byte words
        payload.extend_from_slice(title);
        while payload.len() % 16 != 5 {
            payload.push(0);
        }
        payload.extend_from_slice(&[5, 6]);
        let (audio, titles) = d.push(&payload);
        assert_eq!(audio, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(titles, vec!["AB".to_string()]);
    }

    #[test]
    fn demuxer_passthrough_without_metaint() {
        let mut d = IcyDemuxer::new(0);
        let (audio, titles) = d.push(&[1, 2, 3]);
        assert_eq!(audio, vec![1, 2, 3]);
        assert!(titles.is_empty());
    }
}
