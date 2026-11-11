//! Avahi, the mDNS responder on Linux and the BSDs, over its D-Bus API
//! ([avahi-daemon's introspection](https://github.com/avahi/avahi/tree/009b4e01dbc7b405fc18e23d3b9cac83b0a129d3/avahi-daemon)).

use anyhow::{Context, Result};
use futures_util::StreamExt;
use pixtuoid_core::remote::SERVICE_TYPE;
use server::ServerProxy;
use zbus::Connection;

/// Avahi's bus name (avahi-common/dbus.h), which the proxies below spell again:
/// a macro attribute takes only a literal.
const AVAHI: &str = "org.freedesktop.Avahi";

/// `AVAHI_DBUS_ERR_COLLISION` (avahi-common/dbus.h): `AddService` met a name
/// already held, by this host or another.
const COLLISION_ERROR: &str = "org.freedesktop.Avahi.CollisionError";

/// `AVAHI_IF_UNSPEC` and `AVAHI_PROTO_UNSPEC` (avahi-common/address.h): every
/// interface, both IP versions.
const UNSPEC: i32 = -1;

/// `AvahiEntryGroupState` (avahi-common/defs.h), numbered from 0.
const ESTABLISHED: i32 = 2;
const COLLISION: i32 = 3;
const FAILURE: i32 = 4;

/// What an entry group's new state asks of `announce`.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Announced,
    Rename,
    Dropped,
    Wait,
}

/// The office's service, as far as the entry group holds it: what each server
/// and group state asks, taken as done.
#[derive(Debug, Default)]
struct Service {
    holds: bool,
}

impl Service {
    fn on_server(&mut self, state: i32) -> server::Step {
        match (state, self.holds) {
            // The SRV names the host as it was at `AddService` (our "" host
            // turns NULL in avahi-daemon/dbus-entry-group.c, and
            // avahi-core/entry.c fills it in), so defs.h registers only while
            // RUNNING.
            (server::RUNNING, false) => {
                self.holds = true;
                server::Step::Publish
            }
            // defs.h: remove the services on COLLISION or REGISTERING.
            (server::REGISTERING | server::COLLISION, true) => {
                self.holds = false;
                server::Step::Withdraw
            }
            // avahi-daemon never signals INVALID or FAILURE (avahi-daemon/main.c's
            // `server_callback`), and FAILURE quits it, which the owner watch ends.
            _ => server::Step::Wait,
        }
    }

    fn on_group(&mut self, state: i32) -> Step {
        match state {
            ESTABLISHED => Step::Announced,
            // defs.h: on COLLISION "the entries have been withdrawn".
            COLLISION => {
                self.holds = false;
                Step::Rename
            }
            FAILURE => Step::Dropped,
            _ => Step::Wait,
        }
    }
}

/// Avahi's server: its states, what each asks of the office's service, and its
/// D-Bus proxy.
mod server {
    use zbus::OwnedObjectPath;

    /// `AvahiServerState` (avahi-common/defs.h), numbered from 0.
    pub(super) const REGISTERING: i32 = 1;
    pub(super) const RUNNING: i32 = 2;
    pub(super) const COLLISION: i32 = 3;

    /// What the server's new state asks of the entry group.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Step {
        Publish,
        Withdraw,
        Wait,
    }

    #[zbus::proxy(
        interface = "org.freedesktop.Avahi.Server",
        default_service = "org.freedesktop.Avahi",
        default_path = "/"
    )]
    pub(super) trait Server {
        fn entry_group_new(&self) -> zbus::Result<OwnedObjectPath>;
        fn get_alternative_service_name(&self, name: &str) -> zbus::Result<String>;
        fn get_state(&self) -> zbus::Result<i32>;
        #[zbus(signal)]
        fn state_changed(&self, state: i32, error: String) -> zbus::Result<()>;
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Avahi.EntryGroup",
    default_service = "org.freedesktop.Avahi"
)]
trait EntryGroup {
    #[expect(
        clippy::too_many_arguments,
        reason = "Avahi's AddService signature, argument for argument"
    )]
    fn add_service(
        &self,
        interface: i32,
        protocol: i32,
        flags: u32,
        name: &str,
        kind: &str,
        domain: &str,
        host: &str,
        port: u16,
        txt: Vec<Vec<u8>>,
    ) -> zbus::Result<()>;
    fn commit(&self) -> zbus::Result<()>;
    fn reset(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn state_changed(&self, state: i32, error: String) -> zbus::Result<()>;
}

/// Announce `instance` on `port`, with the pin `spki`, whenever Avahi's host
/// name is established, until Avahi lets the service go or leaves the bus,
/// which frees a client's groups with it. A taken name is renamed as
/// avahi-common/defs.h asks: "add your services back to the entry group (but
/// with new names …) and commit again".
pub(super) async fn announce(instance: &str, port: u16, spki: &str) -> Result<()> {
    let connection = Connection::system()
        .await
        .context("no system D-Bus to reach Avahi through")?;
    let bus = zbus::fdo::DBusProxy::new(&connection).await?;
    let mut owners = bus
        .receive_name_owner_changed_with_args(&[(0, AVAHI)])
        .await?;
    let server = ServerProxy::new(&connection).await?;
    // Subscribed before `get_state` below, so no change falls between them.
    let mut server_states = server.receive_state_changed().await?;
    let path = server
        .entry_group_new()
        .await
        .context("no Avahi daemon on the system bus")?;
    let group = EntryGroupProxy::builder(&connection)
        .path(path)
        .build()
        .await?;
    let mut states = group.receive_state_changed().await?;
    let txt = vec![super::txt(spki).into_bytes()];
    let mut name = instance.to_owned();
    let mut server_state = server.get_state().await?;
    let mut service = Service::default();
    loop {
        match service.on_server(server_state) {
            server::Step::Publish => add(&server, &group, &mut name, port, &txt).await?,
            server::Step::Withdraw => group.reset().await?,
            server::Step::Wait => {}
        }
        tokio::select! {
            change = server_states.next() => {
                let Some(change) = change else { return Ok(()) };
                if let Ok(change) = change.args() {
                    server_state = change.state;
                }
            }
            change = states.next() => {
                let Some(change) = change else { return Ok(()) };
                let Ok(change) = change.args() else { continue };
                match service.on_group(change.state) {
                    Step::Announced => tracing::info!(name = ?name, "announced on the local network"),
                    Step::Rename => {
                        name = server.get_alternative_service_name(&name).await?;
                        group.reset().await?;
                    }
                    Step::Dropped => {
                        tracing::debug!(error = ?change.error, "Avahi dropped the service");
                        return Ok(());
                    }
                    Step::Wait => {}
                }
            }
            owner = owners.next() => {
                let Some(owner) = owner else { return Ok(()) };
                if owner.args().is_ok_and(|args| args.new_owner().is_none()) {
                    return Ok(());
                }
            }
        }
    }
}

/// Add the service under `name`, or under the next name Avahi offers while the
/// one tried is taken, then commit.
async fn add(
    server: &ServerProxy<'_>,
    group: &EntryGroupProxy<'_>,
    name: &mut String,
    port: u16,
    txt: &[Vec<u8>],
) -> Result<()> {
    loop {
        let added = group
            .add_service(
                UNSPEC,
                UNSPEC,
                0,
                name,
                SERVICE_TYPE,
                "",
                "",
                port,
                txt.to_vec(),
            )
            .await;
        match added {
            Err(zbus::Error::MethodError(error, ..)) if error.as_str() == COLLISION_ERROR => {
                *name = server.get_alternative_service_name(name).await?;
            }
            added => {
                added?;
                return Ok(group.commit().await?);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a run of server states asks of a group that starts without the
    /// service.
    fn steps(states: &[i32]) -> Vec<server::Step> {
        let mut service = Service::default();
        states
            .iter()
            .map(|&state| service.on_server(state))
            .collect()
    }

    /// Each `AvahiServerState` by its defs.h number, `INVALID` first, against
    /// a group without the service and one that holds it.
    #[test]
    fn each_server_state_is_its_step() {
        use server::Step::{Publish, Wait, Withdraw};
        let want = [
            (Wait, Wait),
            (Wait, Withdraw),
            (Publish, Wait),
            (Wait, Withdraw),
            (Wait, Wait),
        ];
        for (state, (without, holds)) in (0..).zip(want) {
            assert_eq!(steps(&[state]), [without], "state {state}, without");
            assert_eq!(
                steps(&[server::RUNNING, state])[1],
                holds,
                "state {state}, holds"
            );
        }
    }

    /// A host rename withdraws the service until the server runs again, and a
    /// RUNNING seen twice, by `GetState` and by a signal, publishes once.
    #[test]
    fn the_service_follows_a_host_rename_and_publishes_once() {
        use server::Step::{Publish, Wait, Withdraw};
        use server::{COLLISION, REGISTERING, RUNNING};
        let rename = steps(&[RUNNING, COLLISION, REGISTERING, RUNNING]);
        assert_eq!(rename, [Publish, Withdraw, Wait, Publish]);
        assert_eq!(steps(&[RUNNING, RUNNING]), [Publish, Wait]);
    }

    /// The group's own collision withdraws the service, which comes back under
    /// its new name only while the server runs.
    #[test]
    fn a_group_collision_republishes_only_while_the_server_runs() {
        use server::Step::{Publish, Wait, Withdraw};
        use server::{REGISTERING, RUNNING};
        let mut service = Service::default();
        assert_eq!(service.on_server(RUNNING), Publish);
        assert_eq!(service.on_group(COLLISION), Step::Rename);
        assert_eq!(service.on_server(RUNNING), Publish, "renamed while running");
        assert_eq!(service.on_server(REGISTERING), Withdraw);
        assert_eq!(service.on_group(COLLISION), Step::Rename);
        assert_eq!(
            service.on_server(REGISTERING),
            Wait,
            "no publish under REGISTERING"
        );
        assert_eq!(service.on_server(RUNNING), Publish);
    }

    /// Each `AvahiEntryGroupState` by its defs.h number, `UNCOMMITED` first.
    #[test]
    fn each_group_state_is_its_step() {
        let steps = [
            Step::Wait,
            Step::Wait,
            Step::Announced,
            Step::Rename,
            Step::Dropped,
        ];
        for (state, want) in (0..).zip(steps) {
            assert_eq!(Service::default().on_group(state), want, "state {state}");
        }
    }
}
