//! Avahi, the mDNS responder on Linux and the BSDs, over its D-Bus API
//! ([avahi-daemon's introspection](https://github.com/avahi/avahi/tree/009b4e01dbc7b405fc18e23d3b9cac83b0a129d3/avahi-daemon)).
//! The daemon frees a client's entry groups when the client leaves the bus, so
//! the connection held here is what keeps the service announced.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use zbus::{Connection, OwnedObjectPath};

use super::SERVICE;

/// `AVAHI_IF_UNSPEC` and `AVAHI_PROTO_UNSPEC` (avahi-common/address.h): every
/// interface, both IP versions.
const UNSPEC: i32 = -1;

/// `AvahiEntryGroupState` (avahi-common/defs.h), numbered from 0.
const ESTABLISHED: i32 = 2;
const COLLISION: i32 = 3;
const FAILURE: i32 = 4;

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

/// The service, until dropped: the connection goes with it, and the daemon
/// frees the group.
#[derive(Debug)]
pub(super) struct Registration {
    _connection: Connection,
    watch: JoinHandle<()>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

/// Register `instance` on `port` with the one TXT string `txt`, renaming it on a
/// collision as avahi-common/defs.h asks: "add your services back to the entry
/// group (but with new names …) and commit again".
pub(super) async fn register(instance: &str, port: u16, txt: &str) -> Result<Registration> {
    let connection = Connection::system()
        .await
        .context("no system D-Bus to reach Avahi through")?;
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
    let txt = vec![txt.as_bytes().to_vec()];
    add(&group, instance, port, &txt).await?;
    let mut name = instance.to_owned();
    let watch = tokio::spawn(async move {
        while let Some(change) = states.next().await {
            let Ok(change) = change.args() else { continue };
            match change.state {
                ESTABLISHED => tracing::info!(name = ?name, "announced on the local network"),
                COLLISION => match server.get_alternative_service_name(&name).await {
                    Ok(renamed) => {
                        name = renamed;
                        let readded = match group.reset().await {
                            Ok(()) => add(&group, &name, port, &txt).await,
                            Err(error) => Err(error),
                        };
                        if let Err(error) = readded {
                            tracing::warn!(error = ?error, "Avahi refused the renamed service");
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = ?error, "Avahi gave no other name for the service");
                        return;
                    }
                },
                FAILURE => {
                    tracing::warn!(error = ?change.error, "Avahi dropped the service");
                    return;
                }
                _ => {}
            }
        }
    });
    Ok(Registration {
        _connection: connection,
        watch,
    })
}

async fn add(
    group: &EntryGroupProxy<'_>,
    name: &str,
    port: u16,
    txt: &[Vec<u8>],
) -> zbus::Result<()> {
    group
        .add_service(UNSPEC, UNSPEC, 0, name, SERVICE, "", "", port, txt.to_vec())
        .await?;
    group.commit().await
}
