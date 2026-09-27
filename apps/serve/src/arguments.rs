use rsi_api_http::{HttpConfig, TlsFiles};
use rsi_application::{
    RsiError,
    arguments::{path_value, set_flag, set_option, string_value, utf8},
};
use std::{ffi::OsString, net::SocketAddr, path::PathBuf};

fn invalid(message: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(message.to_string())
}

pub(crate) fn parse(arguments: &[OsString]) -> Result<HttpConfig, RsiError> {
    if arguments.len() > 32
        || arguments
            .iter()
            .map(|arg| arg.as_encoded_bytes().len())
            .sum::<usize>()
            > 64 * 1024
    {
        return Err(invalid("Serve arguments exceed their count or byte bound"));
    }
    let mut bind: Option<SocketAddr> = None;
    let mut origin = None;
    let mut certificate: Option<PathBuf> = None;
    let mut key = None;
    let mut development = false;
    let mut args = arguments.iter().cloned();
    while let Some(arg) = args.next() {
        match utf8(arg)?.as_str() {
            "--bind" => set_option(
                &mut bind,
                string_value(&mut args, "--bind")?
                    .parse()
                    .map_err(invalid)?,
                "--bind",
            )?,
            "--origin" => set_option(
                &mut origin,
                string_value(&mut args, "--origin")?,
                "--origin",
            )?,
            "--tls-certificate" => set_option(
                &mut certificate,
                path_value(&mut args, "--tls-certificate")?,
                "--tls-certificate",
            )?,
            "--tls-key" => set_option(&mut key, path_value(&mut args, "--tls-key")?, "--tls-key")?,
            "--dev-http" => set_flag(&mut development, "--dev-http")?,
            option => return Err(invalid(format!("unknown Serve option: {option}"))),
        }
    }
    let tls = match (certificate, key) {
        (Some(certificate), Some(key)) => Some(TlsFiles { certificate, key }),
        (None, None) => None,
        _ => {
            return Err(invalid(
                "--tls-certificate and --tls-key must be supplied together",
            ));
        }
    };
    let config = HttpConfig {
        bind: bind.ok_or_else(|| invalid("Serve requires --bind ADDRESS"))?,
        public_origin: origin.ok_or_else(|| invalid("Serve requires --origin ORIGIN"))?,
        tls,
        allow_loopback_http: development,
    };
    config.validate().map_err(invalid)?;
    Ok(config)
}
