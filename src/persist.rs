//! Files the bot keeps between runs: its token, learned opponent models and
//! per-match JSONL logs. Tokens are written owner-only and never logged.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::game::price::PriceModel;

pub fn load_token(path: &Path) -> Option<String> {
    let t = fs::read_to_string(path).ok()?;
    let t = t.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

pub fn save_token(path: &Path, token: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("writing token file {}", path.display()))?;
    f.write_all(token.as_bytes())?;
    Ok(())
}

pub fn load_prices(path: &Path) -> PriceModel {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_prices(path: &Path, prices: &PriceModel) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(prices)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// One JSON object per line, one file per match.
pub struct MatchLog {
    file: Option<File>,
    pub path: PathBuf,
}

impl MatchLog {
    pub fn open(dir: &Path, match_id: u64) -> MatchLog {
        let _ = fs::create_dir_all(dir);
        let path = dir.join(format!("match-{match_id}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        MatchLog { file, path }
    }

    pub fn write<T: Serialize>(&mut self, record: &T) {
        if let Some(f) = self.file.as_mut()
            && let Ok(line) = serde_json::to_string(record)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}
