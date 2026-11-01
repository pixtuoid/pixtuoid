//! A layer of a frame drawn once per input set and stamped into each frame
//! after: what lies under everything that moves, which every painter
//! repaints whole or in damage boxes.

use crate::sprite::{Rgb, RgbBuffer};

/// The layer `K` names, and the key it was drawn for. `K` holds every input
/// the drawing reads: a stale hit is invisible to every other gate, so the
/// key is the correctness boundary, and a new input to the drawing joins it.
#[derive(Debug)]
pub(crate) struct CachedLayer<K> {
    key: Option<K>,
    layer: RgbBuffer,
}

impl<K> Default for CachedLayer<K> {
    fn default() -> Self {
        Self {
            key: None,
            layer: RgbBuffer::filled(0, 0, Rgb { r: 0, g: 0, b: 0 }),
        }
    }
}

impl<K> CachedLayer<K> {
    /// Stamp the layer for `key` into `buf` wherever `buf`'s clip lets a write
    /// through, drawing it first with `draw` over a buffer `buf`'s size on any
    /// change of key or size. `draw` is a plain `fn`, which can capture
    /// nothing: all it reads is the key, so every input is one. `key` may
    /// borrow what `K` owns: it becomes a `K` only on a miss, so a hit
    /// allocates nothing.
    pub(crate) fn stamp<Q>(&mut self, key: Q, draw: fn(&K, &mut RgbBuffer), buf: &mut RgbBuffer)
    where
        K: PartialEq<Q>,
        Q: Into<K>,
    {
        let size = (buf.width(), buf.height());
        let hit = self.key.as_ref().is_some_and(|held| *held == key);
        if !hit || (self.layer.width(), self.layer.height()) != size {
            self.layer
                .resize_fill(size.0, size.1, Rgb { r: 0, g: 0, b: 0 });
            let key = self.key.insert(key.into());
            draw(key, &mut self.layer);
        }
        let width = usize::from(size.0);
        let layer = self.layer.as_slice();
        for (y, first, row) in buf.writable_rows_mut() {
            let start = usize::from(y) * width + usize::from(first);
            row.copy_from_slice(&layer[start..start + row.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rgb = Rgb { r: 1, g: 2, b: 3 };
    const B: Rgb = Rgb { r: 9, g: 8, b: 7 };

    std::thread_local! {
        static DRAWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// Fill the layer with the colour its key is, counting the draw.
    fn fill(colour: &Rgb, layer: &mut RgbBuffer) {
        DRAWS.set(DRAWS.get() + 1);
        for y in 0..layer.height() {
            for x in 0..layer.width() {
                layer.put(x, y, *colour);
            }
        }
    }

    /// The layer draws once a key, stamps only inside the clip, and redraws
    /// on a new key or size.
    #[test]
    fn a_layer_draws_once_a_key_and_stamps_inside_the_clip() {
        let mut layer = CachedLayer::default();
        let mut buf = RgbBuffer::filled(4, 3, B);
        layer.stamp(A, fill, &mut buf);
        assert!(buf.as_slice().iter().all(|&p| p == A));
        let mut buf = RgbBuffer::filled(4, 3, B);
        buf.with_clip((1..3, 1..2), |buf| layer.stamp(A, fill, buf));
        assert_eq!(DRAWS.get(), 1, "the same key drew again");
        let painted: Vec<bool> = buf.as_slice().iter().map(|&p| p == A).collect();
        let want: Vec<bool> = (0..3u16)
            .flat_map(|y| (0..4u16).map(move |x| y == 1 && (1..3).contains(&x)))
            .collect();
        assert_eq!(painted, want, "stamped outside the clip");
        let mut buf = RgbBuffer::filled(4, 3, A);
        layer.stamp(B, fill, &mut buf);
        assert!(
            buf.as_slice().iter().all(|&p| p == B),
            "a new key kept the old layer"
        );
        let mut buf = RgbBuffer::filled(5, 3, A);
        layer.stamp(B, fill, &mut buf);
        assert!(
            buf.as_slice().iter().all(|&p| p == B),
            "a new size kept the old layer"
        );
        assert_eq!(DRAWS.get(), 3);
    }
}
