//! 正在播放面板：封面、标题 / 艺人 / 规格、频谱。

use gpui_kit::assets::IconName;
use gpui_kit::component::*;
use gpui_kit::*;

use super::super::view::PlayerView;

/// 频谱条区域高度。
pub(super) const SPECTRUM_HEIGHT: f32 = 96.0;

impl PlayerView {
    pub(super) fn render_now_playing(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted;
        let muted_foreground = theme.muted_foreground;
        let border = theme.border;
        let primary = theme.primary;

        let current_track = self.playing.clone();
        let title = current_track
            .as_ref()
            .map(|track| track.title.clone())
            .unwrap_or_else(|| "还没有正在播放的曲目".into());
        let artist = current_track
            .as_ref()
            .map(|track| {
                if track.artist.is_empty() {
                    // 没有艺人标签是常态（纯音乐、试听文件），别在这儿塞引导语，
                    // 那会让人误以为正在播的曲目信息丢了。
                    if track.album.is_empty() {
                        "未知艺人".to_string()
                    } else {
                        track.album.clone()
                    }
                } else {
                    track.artist.clone()
                }
            })
            .unwrap_or_else(|| "把音频文件拖进来，或者从「文件」菜单打开".into());
        let spec_line = current_track
            .as_ref()
            .map(|track| track.spec_line())
            .unwrap_or_else(|| {
                "支持 MP3 / FLAC / WAV / M4A / OGG / Opus / WMA / APE / DSD 等".into()
            });
        let album = current_track
            .as_ref()
            .map(|track| track.album.clone())
            .unwrap_or_default();

        let cover_element: AnyElement = match &self.cover {
            Some(image) => img(image.clone())
                .size(px(300.0))
                .rounded_xl()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size(px(300.0))
                .rounded_xl()
                .bg(muted)
                .border_1()
                .border_color(border)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Music)
                        .size(px(88.0))
                        .text_color(muted_foreground.opacity(0.55)),
                )
                .into_any_element(),
        };

        let spectrum_element = h_flex()
            .h(px(SPECTRUM_HEIGHT))
            .w_full()
            .max_w(px(420.0))
            .items_end()
            .gap(px(2.0))
            .children(self.levels.iter().map(|level| {
                // 静音时留一条 2px 的基线：看起来像「待机」，而不是界面画错了。
                let height = (level * SPECTRUM_HEIGHT).max(2.0);
                div()
                    .flex_1()
                    .h(px(height))
                    .rounded(px(2.0))
                    .bg(primary.opacity(0.30 + 0.70 * level))
            }));

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap_5()
            .px_8()
            .child(cover_element)
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .max_w(px(520.0))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_center()
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted_foreground)
                            .text_center()
                            .child(artist),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .text_center()
                            .child(if album.is_empty() {
                                spec_line
                            } else {
                                format!("{album} · {spec_line}")
                            }),
                    ),
            )
            .child(spectrum_element)
            .into_any_element()
    }
}
