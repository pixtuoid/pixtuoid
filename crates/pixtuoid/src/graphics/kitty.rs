//! A tile as a kitty image, shown through Unicode placeholders: the image is
//! then ordinary text in cells, which a host like tmux stores and redraws
//! (<https://sw.kovidgoyal.net/kitty/graphics-protocol/>, "Unicode placeholders").
use std::hash::BuildHasher;
use std::ops::RangeInclusive;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use miniz_oxide::deflate::{CompressionLevel, compress_to_vec_zlib};
use ratatui::style::Color;

use super::Medium;
use super::tiles::{Tile, TileImage};
use super::wire::Passthrough;

pub(crate) const PLACEHOLDER: char = '\u{10EEEE}';

/// The numbers 0.. as row/column diacritics: all of kitty's
/// `gen/rowcolumn-diacritics.txt` (kovidgoyal/kitty@24f7369bb638), so a tile
/// an image budget coarsens is still numbered cell by cell.
const DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// One past the largest id a placeholder carries without the most-significant
/// byte's diacritic: the foreground colour's 24 bits.
const ID_LIMIT: u32 = 1 << 24;

/// The ids one process holds: a grid of more tiles shows only these.
const SPAN: u32 = 1 << 16;

/// Base64 bytes per escape ("Remote client").
const CHUNK: usize = 4096;

/// A placeholder cell, in cells from the image's top-left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placeholder {
    /// Its column.
    pub(crate) col: u16,
    /// Its row.
    pub(crate) row: u16,
    /// The placeholder and its row and column diacritics.
    pub(crate) symbol: String,
    /// The image id, as a 24-bit colour.
    pub(crate) fg: Color,
}

/// `tile`'s image id in the block from `base`, the same every frame so a
/// re-send replaces it in place ("Display images on screen": re-sent data
/// replaces the id's image); `None` past the block.
pub(crate) fn image_id(base: u32, tile: Tile) -> Option<u32> {
    (tile.index < SPAN).then(|| base + tile.index)
}

/// This process's id block, picked at random: a fixed one would replace and
/// delete the images of another kitty client in the same window.
pub(crate) fn process_base() -> u32 {
    static BASE: OnceLock<u32> = OnceLock::new();
    *BASE.get_or_init(|| {
        let seed = (std::process::id(), std::time::SystemTime::now());
        base_for(std::collections::hash_map::RandomState::new().hash_one(seed))
    })
}

/// A whole [`SPAN`] under [`ID_LIMIT`], above the first, where other
/// clients' small ids live.
fn base_for(seed: u64) -> u32 {
    let blocks = u64::from(ID_LIMIT / SPAN - 1);
    (1 + (seed % blocks) as u32) * SPAN
}

/// The escapes that (re)transmit `image` as image `id` through `medium` and
/// make its virtual placement over the tile's cells, each wrapped for tmux's
/// passthrough (tmux(1), `allow-passthrough`) when `tmux`. Shared memory that
/// can't be had falls back to the escapes.
///
/// `q=2` on every chunk: a reply would arrive as input mid-frame.
pub(crate) fn transmit(id: u32, image: &TileImage, tmux: bool, medium: Medium) -> Vec<u8> {
    match medium {
        #[cfg(unix)]
        Medium::SharedMemory => match tracing::trace_span!("tile.shm")
            .in_scope(|| super::shm::publish(&image.rgb, std::time::Instant::now()))
        {
            Ok(name) => shared(id, image, &name, tmux),
            Err(e) => {
                tracing::debug!(error = %e, "kitty shared memory failed, tile sent in the escapes");
                direct(id, image, tmux)
            }
        },
        Medium::Direct => direct(id, image, tmux),
    }
}

/// `image` zlib-compressed ("Compression") in the escapes.
fn direct(id: u32, image: &TileImage, tmux: bool) -> Vec<u8> {
    let zlib = tracing::trace_span!("tile.zlib")
        .in_scope(|| compress_to_vec_zlib(&image.rgb, CompressionLevel::BestSpeed as u8));
    let keys = placement_keys(id, image, "o=z,");
    let b64 = tracing::trace_span!("tile.base64")
        .in_scope(|| base64_simd::STANDARD.encode_to_string(zlib));
    chunked(&keys, b64.as_bytes(), tmux)
}

/// `image` read from the shared-memory object `name` holding exactly its
/// bytes (`S`: a mapping's size is a whole number of pages).
#[cfg(unix)]
fn shared(id: u32, image: &TileImage, name: &str, tmux: bool) -> Vec<u8> {
    let keys = placement_keys(id, image, &format!("t=s,S={},", image.rgb.len()));
    let b64 = base64_simd::STANDARD.encode_to_string(name);
    chunked(&keys, b64.as_bytes(), tmux)
}

/// Transmit-and-place `image` as `id`, raw RGB carried as `transport_keys`
/// say, over the tile's cells.
fn placement_keys(id: u32, image: &TileImage, transport_keys: &str) -> String {
    let TileImage {
        tile,
        width,
        height,
        ..
    } = image;
    format!(
        "a=T,U=1,i={id},f=24,{transport_keys}s={width},v={height},c={},r={},",
        tile.cols, tile.rows
    )
}

fn chunked(keys: &str, payload: &[u8], tmux: bool) -> Vec<u8> {
    let Passthrough { open, esc, close } = Passthrough::new(tmux);
    let chunks = payload.chunks(CHUNK);
    let last = chunks.len().saturating_sub(1);
    let mut out = Vec::with_capacity(payload.len() + 128);
    for (i, chunk) in chunks.enumerate() {
        out.extend_from_slice(format!("{open}{esc}_G").as_bytes());
        if i == 0 {
            out.extend_from_slice(keys.as_bytes());
        }
        out.extend_from_slice(format!("q=2,m={};", u8::from(i < last)).as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(format!("{esc}\\{close}").as_bytes());
    }
    out
}

/// Whether this process has put kitty images on the terminal, inside tmux,
/// and the highest id among them: read by an unwind that may run from the
/// panic hook.
static ON_SCREEN: OnceLock<bool> = OnceLock::new();
static LAST_SENT: AtomicU32 = AtomicU32::new(0);

/// Record that ids up to `last` are about to reach the terminal, before
/// their transmits are written.
pub(crate) fn on_screen(tmux: bool, last: u32) {
    let _ = ON_SCREEN.set(tmux);
    LAST_SENT.fetch_max(last, Ordering::Relaxed);
}

/// What the terminal unwind writes first: nothing until [`on_screen`], then
/// [`unwind_for`] this process's ids.
pub(crate) fn unwind() -> Vec<u8> {
    ON_SCREEN.get().map_or_else(Vec::new, |&tmux| {
        unwind_for(process_base()..=LAST_SENT.load(Ordering::Relaxed), tmux)
    })
}

/// An ST that ends an escape a failed write cut short, then [`delete`].
pub(crate) fn unwind_for(ids: RangeInclusive<u32>, tmux: bool) -> Vec<u8> {
    [super::ST, &delete(ids, tmux)].concat()
}

/// Deletes the images `ids` names and frees their data ("Deleting images":
/// `d=R` takes ids from `x` to `y`, both included).
fn delete(ids: RangeInclusive<u32>, tmux: bool) -> Vec<u8> {
    let Passthrough { open, esc, close } = Passthrough::new(tmux);
    format!(
        "{open}{esc}_Ga=d,d=R,x={},y={},q=2{esc}\\{close}",
        ids.start(),
        ids.end()
    )
    .into_bytes()
}

/// The cells that show image `id` over `tile`; `None` for a tile wider or
/// taller than [`DIACRITICS`] can number. Every cell names its row and column,
/// so none leans on the cell to its left being there.
pub(crate) fn placeholders(id: u32, tile: Tile) -> Option<impl Iterator<Item = Placeholder>> {
    let numbered = |n: u16| usize::from(n) <= DIACRITICS.len();
    if !(numbered(tile.cols) && numbered(tile.rows)) {
        return None;
    }
    let [_, r, g, b] = id.to_be_bytes();
    Some((0..tile.rows).flat_map(move |y| {
        (0..tile.cols).map(move |x| Placeholder {
            col: tile.col + x,
            row: tile.row + y,
            symbol: [
                PLACEHOLDER,
                DIACRITICS[usize::from(y)],
                DIACRITICS[usize::from(x)],
            ]
            .iter()
            .collect(),
            fg: Color::Rgb(r, g, b),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::tiles::Tiles;
    use crate::graphics::{CellSize, ImageProtocol, cutaway_fit};
    use pixtuoid_scene::cutaway::canvas::Dirty;
    use pixtuoid_scene::sprite::format::Density;
    use pixtuoid_scene::sprite::{Rgb, RgbBuffer};
    use ratatui::layout::Size as TermSize;

    fn tile(index: u32, cols: u16, rows: u16) -> Tile {
        Tile {
            index,
            col: 0,
            row: 0,
            cols,
            rows,
        }
    }

    fn image(rgb: Vec<u8>) -> TileImage {
        TileImage {
            tile: tile(0, 1, 1),
            width: 2,
            height: 1,
            rgb,
        }
    }

    fn escapes(bytes: &[u8]) -> Vec<String> {
        String::from_utf8(bytes.to_vec())
            .expect("ascii")
            .split_inclusive("\x1b\\")
            .map(str::to_string)
            .collect()
    }

    fn payloads(bytes: &[u8]) -> (Vec<String>, Vec<u8>) {
        let all: Vec<_> = escapes(bytes)
            .iter()
            .map(|e| e[e.find(';').expect("payload") + 1..e.len() - 2].to_string())
            .collect();
        let zlib = base64_simd::STANDARD
            .decode_to_vec(all.concat())
            .expect("base64");
        let rgb = miniz_oxide::inflate::decompress_to_vec_zlib(&zlib).expect("zlib");
        (all, rgb)
    }

    fn incompressible(len: usize) -> Vec<u8> {
        let mut x: u32 = 0x9E37_79B9;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn a_tile_is_one_quiet_compressed_rgb_transmit_with_a_virtual_placement() {
        let rgb = vec![1, 2, 3, 4, 5, 6];
        let out = transmit(1, &image(rgb.clone()), false, Medium::Direct);
        let [escape] = escapes(&out).try_into().expect("one escape");
        assert!(
            escape.starts_with("\x1b_Ga=T,U=1,i=1,f=24,o=z,s=2,v=1,c=1,r=1,q=2,m=0;"),
            "{escape:?}"
        );
        assert!(escape.ends_with("\x1b\\"));
        assert_eq!(payloads(&out).1, rgb);
    }

    /// Through shared memory a tile is one escape naming an object that holds
    /// exactly its raw RGB, uncompressed, with the same placement.
    #[cfg(unix)]
    #[test]
    fn a_shared_memory_tile_names_an_object_holding_its_rgb() {
        let rgb = incompressible(CHUNK * 3);
        let out = transmit(1, &image(rgb.clone()), false, Medium::SharedMemory);
        let [escape] = escapes(&out).try_into().expect("one escape");
        let keys = format!(
            "\x1b_Ga=T,U=1,i=1,f=24,t=s,S={},s=2,v=1,c=1,r=1,q=2,m=0;",
            rgb.len()
        );
        assert!(escape.starts_with(&keys), "{escape:?}");
        let name = base64_simd::STANDARD
            .decode_to_vec(&escape[keys.len()..escape.len() - 2])
            .expect("base64");
        let name = String::from_utf8(name).expect("a name");
        assert_eq!(
            super::super::shm::read_and_unlink(&name, rgb.len()),
            Some(rgb)
        );
    }

    /// Every escape but the last carries a whole chunk and says more follow;
    /// the rest name nothing but `m` and `q`.
    #[test]
    fn the_payload_splits_at_the_chunk_size() {
        let rgb = incompressible(CHUNK * 2);
        let out = transmit(1, &image(rgb.clone()), false, Medium::Direct);
        let all = escapes(&out);
        let (chunks, decoded) = payloads(&out);
        assert_eq!(decoded, rgb);
        let (last, full) = all.split_last().expect("escapes");
        assert!(full.len() >= 2);
        for (escape, chunk) in full.iter().zip(&chunks) {
            assert!(escape.contains("q=2,m=1;"));
            assert_eq!(chunk.len(), CHUNK);
        }
        for escape in &all[1..] {
            assert!(escape.starts_with("\x1b_Gq=2,m="));
        }
        assert!(last.contains("m=0;"));
        assert!((1..=CHUNK).contains(&chunks.last().expect("chunk").len()));
    }

    #[test]
    fn the_last_chunk_says_none_follow() {
        let flags = |len: usize| -> Vec<bool> {
            escapes(&chunked("", &vec![b'A'; len], false))
                .iter()
                .map(|e| e.contains("m=1;"))
                .collect()
        };
        assert_eq!(flags(CHUNK), [false]);
        assert_eq!(flags(CHUNK + 1), [true, false]);
        assert_eq!(flags(2 * CHUNK), [true, false]);
    }

    #[test]
    fn a_two_colour_tile_compresses_to_a_fraction_of_its_pixels() {
        let (w, h) = (40_u32, 40_u32);
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if (x / 4 + y / 8) % 2 == 0 {
                    [0x3a, 0x2f, 0x4c]
                } else {
                    [0xe8, 0xc1, 0x70]
                }
            })
            .collect();
        let raw = rgb.len();
        let tile = TileImage {
            tile: tile(0, 4, 2),
            width: w,
            height: h,
            rgb,
        };
        let sent = transmit(1, &tile, false, Medium::Direct);
        assert!(sent.len() < raw / 4, "{} of {raw}", sent.len());
        assert_eq!(payloads(&sent).1, tile.rgb);
    }

    /// Inside tmux each escape rides passthrough, its own ESCs doubled.
    #[test]
    fn inside_tmux_each_escape_is_wrapped() {
        let tile = image(incompressible(CHUNK * 2));
        let plain = escapes(&transmit(1, &tile, false, Medium::Direct));
        let wrapped: Vec<u8> = plain
            .iter()
            .flat_map(|e| format!("\x1bPtmux;{}\x1b\\", e.replace('\x1b', "\x1b\x1b")).into_bytes())
            .collect();
        assert_eq!(transmit(1, &tile, true, Medium::Direct), wrapped);
    }

    /// The spec's own 2x2 example, for image 42.
    #[test]
    fn placeholders_carry_the_id_and_each_cells_row_and_column() {
        let at = Tile {
            col: 4,
            row: 2,
            ..tile(41, 2, 2)
        };
        let cells: Vec<_> = placeholders(42, at)
            .expect("a kitty tile")
            .map(|p| (p.col, p.row, p.symbol, p.fg))
            .collect();
        let fg = Color::Rgb(0, 0, 42);
        assert_eq!(
            cells,
            [
                (4, 2, "\u{10EEEE}\u{305}\u{305}".to_string(), fg),
                (5, 2, "\u{10EEEE}\u{305}\u{30D}".to_string(), fg),
                (4, 3, "\u{10EEEE}\u{30D}\u{305}".to_string(), fg),
                (5, 3, "\u{10EEEE}\u{30D}\u{30D}".to_string(), fg),
            ]
        );
    }

    /// Against `gen/rowcolumn-diacritics.txt` (kitty@24f7369bb638): its 297
    /// rows, first and last.
    #[test]
    fn the_diacritics_number_every_cell_of_a_kitty_tile() {
        assert_eq!(DIACRITICS.len(), 297);
        assert_eq!(
            DIACRITICS[..4],
            ['\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}']
        );
        assert_eq!(DIACRITICS[295..], ['\u{1D243}', '\u{1D244}']);
        let shape = ImageProtocol::Kitty.tile();
        assert!(usize::from(shape.cols.max(shape.rows)) <= DIACRITICS.len());
    }

    #[test]
    fn the_unwind_deletes_exactly_the_ids_sent() {
        assert_eq!(
            unwind_for(65536..=65541, false),
            b"\x1b\\\x1b_Ga=d,d=R,x=65536,y=65541,q=2\x1b\\"
        );
        assert_eq!(
            delete(65536..=65541, true),
            b"\x1bPtmux;\x1b\x1b_Ga=d,d=R,x=65536,y=65541,q=2\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn a_tile_past_the_diacritics_has_no_placeholders() {
        let past = u16::try_from(DIACRITICS.len() + 1).expect("fits");
        assert!(placeholders(1, tile(0, past, 1)).is_none());
        assert!(placeholders(1, tile(0, 1, past)).is_none());
    }

    /// A tile the image budget coarsens still numbers every cell: its last
    /// row and column carry their own diacritics.
    #[test]
    fn a_coarsened_tile_numbers_every_cell() {
        let cells: Vec<_> = placeholders(1, tile(0, 16, 8)).expect("numbered").collect();
        assert_eq!(cells.len(), 16 * 8);
        let last = cells.last().expect("cells");
        assert_eq!((last.col, last.row), (15, 7));
        let symbol: Vec<char> = last.symbol.chars().collect();
        assert_eq!(symbol, [PLACEHOLDER, DIACRITICS[7], DIACRITICS[15]]);
    }

    #[test]
    fn ids_live_in_the_block_from_the_base() {
        let base = base_for(7);
        assert_eq!(image_id(base, tile(0, 1, 1)), Some(base));
        assert_eq!(image_id(base, tile(SPAN - 1, 1, 1)), Some(base + SPAN - 1));
        assert_eq!(image_id(base, tile(SPAN, 1, 1)), None);
        assert_eq!(image_id(base, tile(u32::MAX, 1, 1)), None);
    }

    /// Every seed lands on a whole block between the first and [`ID_LIMIT`],
    /// so two different bases never share an id.
    #[test]
    fn every_base_is_a_block_of_its_own() {
        let blocks = ID_LIMIT / SPAN - 1;
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..u64::from(2 * blocks) {
            let base = base_for(seed);
            assert!(base.is_multiple_of(SPAN), "seed {seed}");
            assert!(base >= SPAN && base + SPAN <= ID_LIMIT, "seed {seed}");
            seen.insert(base);
        }
        assert_eq!(seen.len(), blocks as usize);
        assert!(process_base().is_multiple_of(SPAN) && process_base() >= SPAN);
    }

    /// An idle office sends the terminal nothing.
    #[test]
    fn an_idle_office_sends_zero_bytes() {
        let cell = CellSize { w: 8, h: 16 };
        let area = TermSize {
            width: 80,
            height: 24,
        };
        let fit = cutaway_fit(cell, area, Density::new(4).expect("nonzero")).expect("fits");
        let mut tiles = Tiles::new(ImageProtocol::Kitty, cell, fit);
        let buf = RgbBuffer::filled(64, 40, Rgb { r: 1, g: 2, b: 3 });
        let mut sent = |dirty: &Dirty| -> usize {
            let changed = tiles.changed(&buf, dirty);
            let bytes = changed
                .iter()
                .map(|c| {
                    let id = image_id(SPAN, c.tile).expect("id");
                    transmit(id, &tiles.image(&buf, c.tile), false, Medium::Direct).len()
                })
                .sum();
            tiles.sent(&changed);
            bytes
        };
        assert!(sent(&Dirty::All) > 0);
        assert_eq!(sent(&Dirty::Unchanged), 0);
        assert_eq!(sent(&Dirty::All), 0);
    }
}
