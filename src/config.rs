//! Configuration : fichier `blackshark-battery.ini` à côté de l'exécutable (optionnel).
//!
//! ```ini
//! poll_ms = 1000        # période de lecture / re-détection du dongle (ms)
//! low_battery = 15      # seuil d'alerte batterie faible (%), 0 = désactivé
//! stale_s = 900         # au-delà, le dernier niveau est affiché comme « ancien »
//! product_ids = 057A    # PID USB acceptés, séparés par des virgules (hex)
//! query =               # rapport hex envoyé à chaque cycle (optionnel) ; le dongle
//!                       # ne sait pas répondre au niveau, voir le README
//! ```

#[derive(Debug, Clone)]
pub struct Config {
    pub poll_ms: u32,
    pub low_battery: u8,
    pub stale_s: u64,
    pub product_ids: Vec<u16>,
    pub query: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        Self { poll_ms: 1000, low_battery: 15, stale_s: 900, product_ids: vec![0x057A], query: vec![] }
    }
}

impl Config {
    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let line = line.split(['#', ';']).next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "poll_ms" => if let Ok(x) = v.parse::<u32>() { c.poll_ms = x.clamp(100, 600_000) },
                "low_battery" => if let Ok(x) = v.parse::<u8>() { c.low_battery = x.min(100) },
                "stale_s" => if let Ok(x) = v.parse() { c.stale_s = x },
                "product_ids" => {
                    let ids: Vec<u16> = v.split(',').filter_map(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()).collect();
                    if !ids.is_empty() { c.product_ids = ids }
                }
                "query" => c.query = v.split_whitespace().filter_map(|s| u8::from_str_radix(s, 16).ok()).collect(),
                _ => {}
            }
        }
        c
    }

    pub fn load() -> Self {
        std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::read_to_string(p.with_file_name("blackshark-battery.ini")).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values_and_ignores_junk() {
        let c = Config::parse("poll_ms = 250 # rapide\nlow_battery=20\nproduct_ids = 057A, 0x0580\nquery = 02 00 ff\nfoo\n");
        assert_eq!(c.poll_ms, 250);
        assert_eq!(c.low_battery, 20);
        assert_eq!(c.product_ids, vec![0x057A, 0x0580]);
        assert_eq!(c.query, vec![2, 0, 0xFF]);
        assert_eq!(Config::parse("poll_ms=1").poll_ms, 100);
    }
}
