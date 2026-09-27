//! Streaming and AI availability probes adapted from clash-verge-rev (GPL-3.0-or-later).
//! See the README relationship section for provenance.

// Keep upstream probe branches easy to compare when service endpoints change.
#![allow(
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::redundant_pub_crate
)]

use std::time::Duration;

use reqwest::Client;

mod bahamut;
mod bilibili;
mod chatgpt;
mod claude;
mod disney_plus;
mod gemini;
mod netflix;
mod prime_video;
mod spotify;
mod tiktok;
mod types;
mod utils;
mod youtube;

pub use types::UnlockItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnlockCheck {
    BilibiliChinaMainland,
    BilibiliHkMcTw,
    ChatGptWeb,
    Claude,
    Gemini,
    YouTubePremium,
    BahamutAnime,
    Netflix,
    DisneyPlus,
    PrimeVideo,
    Spotify,
    TikTok,
}

impl UnlockCheck {
    const ALL: &[Self] = &[
        Self::BilibiliChinaMainland,
        Self::BilibiliHkMcTw,
        Self::ChatGptWeb,
        Self::Claude,
        Self::Gemini,
        Self::YouTubePremium,
        Self::BahamutAnime,
        Self::Netflix,
        Self::DisneyPlus,
        Self::PrimeVideo,
        Self::Spotify,
        Self::TikTok,
    ];

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|check| check.name() == name)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::BilibiliChinaMainland => bilibili::BILIBILI_CHINA_MAINLAND_NAME,
            Self::BilibiliHkMcTw => bilibili::BILIBILI_HK_MC_TW_NAME,
            Self::ChatGptWeb => chatgpt::CHATGPT_WEB_NAME,
            Self::Claude => claude::CLAUDE_NAME,
            Self::Gemini => gemini::GEMINI_NAME,
            Self::YouTubePremium => youtube::YOUTUBE_PREMIUM_NAME,
            Self::BahamutAnime => bahamut::BAHAMUT_ANIME_NAME,
            Self::Netflix => netflix::NETFLIX_NAME,
            Self::DisneyPlus => disney_plus::DISNEY_NAME,
            Self::PrimeVideo => prime_video::PRIME_VIDEO_NAME,
            Self::Spotify => spotify::SPOTIFY_NAME,
            Self::TikTok => tiktok::TIKTOK_NAME,
        }
    }

    async fn check(self, client: &Client) -> UnlockItem {
        match self {
            Self::BilibiliChinaMainland => bilibili::check_bilibili_china_mainland(client).await,
            Self::BilibiliHkMcTw => bilibili::check_bilibili_hk_mc_tw(client).await,
            Self::ChatGptWeb => chatgpt::check_chatgpt(client).await,
            Self::Claude => claude::check_claude(client).await,
            Self::Gemini => gemini::check_gemini(client).await,
            Self::YouTubePremium => youtube::check_youtube_premium(client).await,
            Self::BahamutAnime => bahamut::check_bahamut_anime(client).await,
            Self::Netflix => netflix::check_netflix(client).await,
            Self::DisneyPlus => disney_plus::check_disney_plus(client).await,
            Self::PrimeVideo => prime_video::check_prime_video(client).await,
            Self::Spotify => spotify::check_spotify(client).await,
            Self::TikTok => tiktok::check_tiktok(client).await,
        }
    }

    async fn check_with_timeout(self, client: &Client) -> UnlockItem {
        tokio::time::timeout(Duration::from_secs(15), self.check(client))
            .await
            .unwrap_or_else(|_| UnlockItem::checked(self.name(), "Failed", None))
    }
}

pub async fn check_media_unlock_item(client: &Client, name: &str) -> Result<UnlockItem, String> {
    let check =
        UnlockCheck::from_name(name).ok_or_else(|| format!("未知的流媒体检测项目: {name}"))?;

    Ok(check.check_with_timeout(client).await)
}

#[cfg(test)]
mod tests {
    use super::UnlockCheck;

    #[test]
    fn every_tui_service_resolves_to_one_probe() {
        for kind in cvt_tui::row::TestKind::all() {
            let cvt_tui::row::TestKind::Unlock(name) = kind else {
                panic!("test page contains a non-unlock entry");
            };
            assert!(
                UnlockCheck::from_name(name).is_some(),
                "missing probe: {name}"
            );
        }
        assert_eq!(cvt_tui::row::TestKind::all().len(), UnlockCheck::ALL.len());
    }
}
