use serde::{Deserialize, Serialize};

/// A single internet-radio station.
/// Only the fields needed for V1. Future fields (genre, bitrate, website)
/// can be added without breaking config parsing (serde defaults).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub url: String,
}

impl Station {
    pub fn new(name: &str, description: &str, url: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            url: url.to_string(),
        }
    }
}

/// Curated, verified direct audio streams.
///
/// Verification (Oct 2026, via `curl -I`):
/// - All endpoints return 200 with `Content-Type: audio/mpeg` (or aac) and
///   `icy-name` headers from public Icecast/Shoutcast servers.
/// - We link to the streams; we do not host or redistribute content.
/// - NOTE: "Lofi Girl" has no official direct audio stream (YouTube only),
///   so it is intentionally NOT listed. See README for details.
pub fn builtin_stations() -> Vec<Station> {
    vec![
        Station::new(
            "Groove Salad",
            "SomaFM — ambient beats and grooves",
            "https://ice1.somafm.com/groovesalad-128-mp3",
        ),
        Station::new(
            "Deep Space One",
            "SomaFM — deep ambient space music",
            "https://ice1.somafm.com/deepspaceone-128-mp3",
        ),
        Station::new(
            "Drone Zone",
            "SomaFM — atmospheric ambient",
            "https://ice1.somafm.com/dronezone-128-mp3",
        ),
        Station::new(
            "Lush",
            "SomaFM — female vocals, electronic",
            "https://ice1.somafm.com/lush-128-mp3",
        ),
        Station::new(
            "Radio Paradise",
            "Radio Paradise — eclectic mix (128k mp3)",
            "https://stream.radioparadise.com/mp3-128",
        ),
    ]
}

/// Case-insensitive lookup by name. Trims surrounding whitespace.
pub fn find_station<'a>(stations: &'a [Station], name: &str) -> Option<&'a Station> {
    let needle = name.trim().to_lowercase();
    stations
        .iter()
        .find(|s| s.name.to_lowercase() == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_list_is_nonempty_with_https_urls() {
        let stations = builtin_stations();
        assert!(!stations.is_empty());
        for s in &stations {
            assert!(!s.name.is_empty());
            assert!(
                s.url.starts_with("https://"),
                "station {} must use https, got {}",
                s.name,
                s.url
            );
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let stations = builtin_stations();
        assert!(find_station(&stations, "groove salad").is_some());
        assert!(find_station(&stations, "GROOVE SALAD").is_some());
        assert!(find_station(&stations, "  Drone Zone ").is_some());
    }

    #[test]
    fn lookup_invalid_returns_none() {
        let stations = builtin_stations();
        assert!(find_station(&stations, "no such station").is_none());
        assert!(find_station(&stations, "").is_none());
    }
}
