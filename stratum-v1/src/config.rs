//! V1-specific session config; the firmware passes a `&'static`
//! reference into `StratumV1Client::new`.

/// Pool credentials for a Stratum V1 session.
pub struct Sv1Config {
    pub user: &'static str,
    pub password: &'static str,
}
