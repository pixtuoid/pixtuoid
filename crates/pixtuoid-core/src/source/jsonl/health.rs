/// Once-per-transition gate for persistent-failure warnings (the watched root
/// going unreadable mid-run, a broken notify backend, the hook socket's accept
/// loop erroring). These failures recur on every pass, so an ungated `warn!`
/// spams the warn-floor log every interval, while total silence leaves the
/// watcher permanently blind — after a successful bind there is no SourceDeath
/// path left to report through.
#[derive(Default, Clone)]
pub(crate) struct FailureLatch {
    failing: bool,
}

impl FailureLatch {
    pub(crate) fn on_failure(&mut self) -> bool {
        !std::mem::replace(&mut self.failing, true)
    }

    pub(crate) fn on_success(&mut self) -> bool {
        std::mem::replace(&mut self.failing, false)
    }
}
