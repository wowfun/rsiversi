/// Ownership of the service reached by an application's connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionLifetime {
    /// The enclosing application owns and shuts down its embedded service.
    Embedded,
    /// The service has an independent owner; application exit detaches.
    Remote,
}

/// Connection metadata for presentation; it carries no service shutdown authority.
#[derive(Debug)]
pub struct ConnectionLifetimeContract;
impl rsi_meta::LocalContract for ConnectionLifetimeContract {
    const KEY: &'static str = "rsi.client.connection-lifetime";
    type Service = ConnectionLifetime;
}
