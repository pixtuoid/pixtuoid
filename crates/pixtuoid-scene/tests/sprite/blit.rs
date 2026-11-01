use pixtuoid_scene::sprite::blit::blit_frame;
use pixtuoid_scene::sprite::{Frame, Pixel, Rgb, RgbBuffer};

fn px(r: u8, g: u8, b: u8) -> Pixel {
    Some(Rgb { r, g, b })
}
fn t() -> Pixel {
    None
}

#[test]
fn blit_writes_opaque_pixels_and_skips_transparent() {
    let frame = Frame::from_pixels(2, 2, vec![px(10, 0, 0), t(), t(), px(0, 0, 30)]);
    let mut buf = RgbBuffer::filled(
        4,
        4,
        Rgb {
            r: 99,
            g: 99,
            b: 99,
        },
    );
    blit_frame(&frame, 1, 1, &mut buf);

    assert_eq!(buf.get(1, 1), Rgb { r: 10, g: 0, b: 0 });
    assert_eq!(
        buf.get(2, 1),
        Rgb {
            r: 99,
            g: 99,
            b: 99
        }
    );
    assert_eq!(
        buf.get(1, 2),
        Rgb {
            r: 99,
            g: 99,
            b: 99
        }
    );
    assert_eq!(buf.get(2, 2), Rgb { r: 0, g: 0, b: 30 });
    assert_eq!(
        buf.get(0, 0),
        Rgb {
            r: 99,
            g: 99,
            b: 99
        }
    );
}

#[test]
fn blit_ignores_out_of_bounds() {
    let frame = Frame::from_pixels(3, 3, vec![px(1, 1, 1); 9]);
    let mut buf = RgbBuffer::filled(2, 2, Rgb { r: 0, g: 0, b: 0 });
    blit_frame(&frame, 1, 1, &mut buf);
    assert_eq!(buf.get(1, 1), Rgb { r: 1, g: 1, b: 1 });
}
