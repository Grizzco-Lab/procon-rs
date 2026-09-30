//! The HUD reader on stored crops: text masks of real HUD corners (400x120,
//! binary PBM, 6 KB each) from our 360p sessions, 720p Discord clips of
//! Splatoon 3 and Splatoon 2, an extra wave and the lobby, and 720p streams
//! of top players (counts of 100 and more, sparkles once the quota is met,
//! Eggstra Work's waves 4 and 5).

use gameplay_vision::hud::{self, CROP_H, CROP_W, Hud, HudCrop};

/// A crop that is white where the mask is set and black elsewhere
fn crop_from_pbm(bytes: &[u8]) -> HudCrop {
    let header = format!("P4\n{CROP_W} {CROP_H}\n");
    assert!(
        bytes.starts_with(header.as_bytes()),
        "not a {CROP_W}x{CROP_H} PBM"
    );
    let bits = &bytes[header.len()..];
    let row = CROP_W.div_ceil(8);
    let mut rgb = vec![0u8; CROP_W * CROP_H * 3];
    for y in 0..CROP_H {
        for x in 0..CROP_W {
            if bits[y * row + x / 8] & (0x80 >> (x % 8)) != 0 {
                rgb[(y * CROP_W + x) * 3..][..3].fill(255);
            }
        }
    }
    HudCrop { rgb }
}

/// Wave, timer and eggs read
type Read = (Option<u8>, Option<u8>, Option<(u16, u16)>);

fn read(bytes: &[u8]) -> Option<Read> {
    hud::read(&crop_from_pbm(bytes)).map(|h: Hud| (h.wave, h.timer_s, h.eggs))
}

#[test]
fn reads_our_360p_sessions() {
    let white = include_bytes!("hud/s3_360p_white.pbm");
    assert_eq!(read(white), Some((Some(1), Some(91), Some((0, 27)))));
    // The last seconds: yellow digits, a sparkle on the egg icon
    let yellow = include_bytes!("hud/s3_360p_yellow.pbm");
    assert_eq!(read(yellow), Some((Some(3), Some(7), Some((34, 31)))));
}

#[test]
fn reads_720p_clips_of_both_games() {
    // Pulsing digits on the orange band of the last 30 seconds
    let orange = include_bytes!("hud/s3_720p_orange.pbm");
    assert_eq!(read(orange), Some((Some(1), Some(22), Some((25, 27)))));
    let s2 = include_bytes!("hud/s2_720p.pbm");
    assert_eq!(read(s2), Some((Some(2), Some(14), Some((19, 20)))));
}

#[test]
fn reads_counts_of_100_and_more_in_their_narrow_digits() {
    // Read as 108 before: a narrow 0 or 6 taken for an 8
    let c106 = include_bytes!("hud/s3_720p_narrow_106.pbm");
    assert_eq!(read(c106), Some((Some(3), Some(2), Some((106, 26)))));
    let c100 = include_bytes!("hud/s3_720p_narrow_100.pbm");
    assert_eq!(read(c100), Some((Some(2), Some(10), Some((100, 18)))));
}

#[test]
fn sparkles_around_a_met_quota_are_not_digits() {
    // A sparkle right of the quota was read as a third digit (41/291)
    let apart = include_bytes!("hud/s3_720p_sparkles.pbm");
    assert_eq!(read(apart), Some((Some(1), Some(28), Some((41, 29)))));
    // One stuck to the quota's 2 made it an 8 (33/38): unread now
    let stuck = include_bytes!("hud/s3_720p_sparkle_stuck.pbm");
    assert_eq!(read(stuck), Some((Some(2), Some(15), None)));
    // A burst over both numbers left a digit of each (38/32 read as 3/3)
    let over = include_bytes!("hud/s3_720p_sparkles_over.pbm");
    assert_eq!(read(over), Some((Some(2), Some(47), None)));
}

#[test]
fn reads_eggstra_works_waves_4_and_5() {
    // Not read, and read as 3, before their digits were learned
    let w4 = include_bytes!("hud/s3_720p_wave4.pbm");
    assert_eq!(read(w4), Some((Some(4), Some(50), Some((18, 30)))));
    let w5 = include_bytes!("hud/s3_720p_wave5.pbm");
    assert_eq!(read(w5), Some((Some(5), Some(46), Some((23, 31)))));
}

#[test]
fn an_extra_wave_has_a_timer_but_no_wave_number() {
    let extra = include_bytes!("hud/s3_xtrawave.pbm");
    assert_eq!(read(extra), Some((None, Some(4), None)));
}

#[test]
fn no_hud_in_the_lobby() {
    assert_eq!(read(include_bytes!("hud/lobby.pbm")), None);
}

#[test]
fn a_1080p_frame_reads_like_its_crop() {
    // The crop scaled into the corner of a 1920x1080 frame (1.5 times the
    // crop's 1280x720 scale), then taken back out
    let crop = crop_from_pbm(include_bytes!("hud/s3_360p_white.pbm"));
    let (w, h) = (1920, 1080);
    let mut frame = vec![40u8; w * h * 3];
    for y in 0..CROP_H * 3 / 2 {
        for x in 0..CROP_W * 3 / 2 {
            let src = ((y * 2 / 3) * CROP_W + x * 2 / 3) * 3;
            frame[(y * w + x) * 3..][..3].copy_from_slice(&crop.rgb[src..src + 3]);
        }
    }
    let back = HudCrop::from_frame(&frame, w, h);
    let hud = hud::read(&back).expect("a HUD");
    assert_eq!(
        (hud.wave, hud.timer_s, hud.eggs),
        (Some(1), Some(91), Some((0, 27)))
    );
}
