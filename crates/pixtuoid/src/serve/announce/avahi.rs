//! Avahi, the mDNS responder on Linux and the BSDs, over its D-Bus API
//! ([avahi-daemon's introspection](https://github.com/avahi/avahi/tree/009b4e01dbc7b405fc18e23d3b9cac83b0a129d3/avahi-daemon)).

use anyhow::{Context, Result};
use futures_util::StreamExt;
use pixtuoid_core::remote::SERVICE_TYPE;
use zbus::{Connection, OwnedObjectPath};

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

fn step(state: i32) -> Step {
    match state {
        ESTABLISHED => Step::Announced,
        COLLISION => Step::Rename,
        FAILURE => Step::Dropped,
        _ => Step::Wait,
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Avahi.Server",
    default_service = "org.freedesktop.Avahi",
    default_path = "/"
)]
trait Server {
    fn entry_group_new(&self) -> zbus::Result<OwnedObjectPath>;
    fn get_alternative_service_name(&self, name: &str) -> zbus::Result<String>;
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

/// Announce `instance` on `port`, pinned by `spki`, until Avahi lets the service
/// go or leaves the bus, which frees a client's groups with it. A taken name is
/// renamed as avahi-common/defs.h asks: "add your services back to the entry
/// group (but with new names …) and commit again".
pub(super) async fn announce(instance: &str, port: u16, spki: &str) -> Result<()> {
    let connection = Connection::system()
        .await
        .context("no system D-Bus to reach Avahi through")?;
    let bus = zbus::fdo::DBusProxy::new(&connection).await?;
    let mut owners = bus
        .receive_name_owner_changed_with_args(&[(0, AVAHI)])
        .await?;
    let server = ServerProxy::new(&connection).await?;
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
    add(&server, &group, &mut name, port, &txt).await?;
    loop {
        tokio::select! {
            change = states.next() => {
                let Some(change) = change else { return Ok(()) };
                let Ok(change) = change.args() else { continue };
                match step(change.state) {
                    Step::Announced => tracing::info!(name = ?name, "announced on the local network"),
                    Step::Rename => {
                        name = server.get_alternative_service_name(&name).await?;
                        group.reset().await?;
                        add(&server, &group, &mut name, port, &txt).await?;
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
            assert_eq!(step(state), want, "state {state}");
        }
    }
}
