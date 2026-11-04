//! Throwaway: f32 libm shims that record where std differs from `libm`.

#[cfg(not(test))]
mod imp {
    #[inline]
    pub(crate) fn exp(x: f32) -> f32 {
        x.exp()
    }
    #[inline]
    pub(crate) fn ln(x: f32) -> f32 {
        x.ln()
    }
    #[inline]
    pub(crate) fn sin(x: f32) -> f32 {
        x.sin()
    }
    #[inline]
    pub(crate) fn cos(x: f32) -> f32 {
        x.cos()
    }
    #[inline]
    pub(crate) fn hypot(x: f32, y: f32) -> f32 {
        x.hypot(y)
    }
}

pub(crate) use imp::*;

#[cfg(test)]
mod imp {
    use std::cell::{Cell, RefCell};

    pub(crate) struct Record {
        pub(crate) name: &'static str,
        pub(crate) input: u64,
        pub(crate) input2: Option<u64>,
        pub(crate) wide: bool,
        pub(crate) std: u64,
        pub(crate) libm: u64,
    }

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
        static RECORDS: RefCell<Vec<Record>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn start() {
        ON.with(|o| o.set(true));
    }

    pub(crate) fn take() -> Vec<Record> {
        ON.with(|o| o.set(false));
        RECORDS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    fn note(name: &'static str, input: (u32, Option<u32>), std: f32, libm: f32) -> f32 {
        if ON.with(Cell::get) && std.to_bits() != libm.to_bits() {
            RECORDS.with(|r| {
                r.borrow_mut().push(Record {
                    name,
                    input: input.0.into(),
                    input2: input.1.map(Into::into),
                    wide: false,
                    std: std.to_bits().into(),
                    libm: libm.to_bits().into(),
                })
            });
        }
        std
    }

    pub(crate) fn exp(x: f32) -> f32 {
        note("exp", (x.to_bits(), None), x.exp(), libm::expf(x))
    }
    pub(crate) fn ln(x: f32) -> f32 {
        note("ln", (x.to_bits(), None), x.ln(), libm::logf(x))
    }
    pub(crate) fn sin(x: f32) -> f32 {
        note("sin", (x.to_bits(), None), x.sin(), libm::sinf(x))
    }
    pub(crate) fn cos(x: f32) -> f32 {
        note("cos", (x.to_bits(), None), x.cos(), libm::cosf(x))
    }
    pub(crate) fn hypot(x: f32, y: f32) -> f32 {
        note(
            "hypot",
            (x.to_bits(), Some(y.to_bits())),
            x.hypot(y),
            libm::hypotf(x, y),
        )
    }
    impl Record {
        pub(crate) fn show(&self, bits: u64) -> String {
            if self.wide {
                f64::from_bits(bits).to_string()
            } else {
                f32::from_bits(bits as u32).to_string()
            }
        }
    }
}
