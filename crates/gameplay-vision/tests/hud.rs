//! The HUD reader on stored crops: text masks of real HUD corners (400x120,
//! binary PBM, 6 KB each) from our 360p sessions, 720p Discord clips of
//! Splatoon 3 and Splatoon 2, an extra wave and the lobby.

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
