//! Narrow subscriptions. The match tables keep every match ever played, so
//! nothing here subscribes to a whole match table.

/// Lives for the whole connection once our bot id is known.
pub fn always(me: u64) -> Vec<String> {
    vec![
        "SELECT * FROM bot".into(),
        "SELECT * FROM bot_credential".into(),
        "SELECT * FROM matchmaker_config".into(),
        "SELECT * FROM open_lobbies".into(),
        "SELECT * FROM open_lobby_members".into(),
        "SELECT * FROM tournament".into(),
        "SELECT * FROM tournament_entry".into(),
        "SELECT * FROM tournament_match".into(),
        "SELECT * FROM my_rack".into(),
        format!("SELECT * FROM match_participant WHERE bot_id = {me}"),
    ]
}

/// Before we know our bot id (fresh nonce identity): just enough to find it.
pub fn bootstrap() -> Vec<String> {
    vec![
        "SELECT * FROM bot_credential".into(),
        "SELECT * FROM bot".into(),
    ]
}

/// Everyone's live state in one match.
pub fn per_match(m: u64) -> Vec<String> {
    vec![
        format!("SELECT * FROM match_state WHERE id = {m}"),
        format!("SELECT * FROM auction WHERE match_id = {m}"),
        format!("SELECT * FROM match_participant WHERE match_id = {m}"),
        format!("SELECT * FROM auction_result WHERE match_id = {m}"),
        format!("SELECT * FROM word_play WHERE match_id = {m}"),
    ]
}

/// Filled when the match ends: everyone's winning bids.
pub fn archive(m: u64) -> Vec<String> {
    vec![format!(
        "SELECT * FROM auction_top_bid_archive WHERE match_id = {m}"
    )]
}
