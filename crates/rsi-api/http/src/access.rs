use crate::{
    policy::{Policy, one},
    server::{Body, full},
};
use bytes::Bytes;
use http::{Request, Response};
use hyper::body::{Body as _, Incoming};
#[cfg(unix)]
use rsi_api_protocol::LocalCompatibilityKey;
use rsi_api_protocol::{
    ApiError, AuthenticatedDevice, CallOrigin, DeviceAuthentication, DeviceId, Result,
};
use rsi_credentials_protocol::SecretValue;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) enum Access {
    Remote {
        policy: Policy,
        authentication: Arc<dyn DeviceAuthentication>,
    },
    #[cfg(unix)]
    Local(LocalCompatibilityKey),
}
pub(crate) struct Caller {
    pub origin: CallOrigin,
    pub revoked: CancellationToken,
}
impl Access {
    fn remote(&self) -> Result<(&Policy, &Arc<dyn DeviceAuthentication>)> {
        match self {
            Self::Remote {
                policy,
                authentication,
            } => Ok((policy, authentication)),
            #[cfg(unix)]
            Self::Local(_) => Err(ApiError::Unauthorized),
        }
    }
    pub fn fence(&self, request: &Request<Incoming>) -> Result<()> {
        let headers = request.headers();
        match self {
            Self::Remote { policy, .. } => {
                if one(headers, "x-rsi-local-key")?.is_some() {
                    return Err(ApiError::Unauthorized);
                }
                policy.fence(headers, request.uri(), request.version())
            }
            #[cfg(unix)]
            Self::Local(key) => {
                if request.version() != http::Version::HTTP_11
                    || request.uri().authority().is_some()
                    || one(headers, "host")? != Some("rsi.local")
                    || one(headers, "x-rsi-local-key")? != Some(key.as_str())
                    || ["authorization", "cookie", "origin"]
                        .iter()
                        .any(|name| headers.contains_key(*name))
                {
                    return Err(ApiError::Unauthorized);
                }
                Ok(())
            }
        }
    }
    pub fn authenticate(&self, headers: &http::HeaderMap) -> Result<Caller> {
        let caller = match self {
            Self::Remote { .. } => {
                let device = self.authenticate_device(headers)?;
                Caller {
                    revoked: device.revoked.clone(),
                    origin: CallOrigin::Device(device),
                }
            }
            #[cfg(unix)]
            Self::Local(_) => Caller {
                origin: CallOrigin::Local,
                revoked: CancellationToken::new(),
            },
        };
        if let Some(expected) = expected_device(headers)?
            && !matches!(&caller.origin, CallOrigin::Device(device) if device.id == expected)
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(caller)
    }
    fn authenticate_device(&self, headers: &http::HeaderMap) -> Result<AuthenticatedDevice> {
        let (policy, authentication) = self.remote()?;
        let bearer = one(headers, "authorization")?;
        let cookie = device_cookie(headers)?;
        let token = match (bearer, cookie) {
            (Some(bearer), None) => bearer
                .strip_prefix("Bearer ")
                .ok_or(ApiError::Unauthorized)?,
            (None, Some(cookie)) => {
                policy.browser(headers)?;
                cookie
            }
            _ => return Err(ApiError::Unauthorized),
        };
        if token.len() != 64 {
            return Err(ApiError::Unauthorized);
        }
        let secret = SecretValue::new(token).map_err(|_| ApiError::Unauthorized)?;
        let device = authentication.authenticate(&secret)?;
        if device.revoked.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        Ok(device)
    }

    pub fn bootstrap(
        &self,
        headers: &http::HeaderMap,
        body: &Incoming,
        bootstrap: &dyn crate::BrowserBootstrap,
        endpoint: &rsi_api_protocol::EndpointId,
    ) -> Result<Response<Body>> {
        let (policy, authentication) = self.remote()?;
        policy.browser(headers)?;
        if body.size_hint().upper() != Some(0) || one(headers, "authorization")?.is_some() {
            return Err(ApiError::Unauthorized);
        }
        // Classify all credential headers before consuming one-use authority.
        let _cookie = device_cookie(headers)?;
        let expected = expected_device(headers)?;
        let (device, token) = if let Some(ticket) = one(headers, "x-rsi-launch-ticket")? {
            if expected.is_some()
                || ticket.len() != 64
                || !ticket.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(ApiError::Unauthorized);
            }
            let ticket = SecretValue::new(ticket).map_err(|_| ApiError::Unauthorized)?;
            let token = bootstrap.redeem(&ticket)?;
            let device = authentication.authenticate(&token)?;
            if device.revoked.is_cancelled() {
                return Err(ApiError::Unauthorized);
            }
            (device, Some(token))
        } else {
            let device = self.authenticate_device(headers)?;
            if expected.is_some_and(|expected| expected != device.id) {
                return Err(ApiError::Unauthorized);
            }
            (device, None)
        };
        let bytes = serde_json::to_vec(
            &serde_json::json!({"endpoint_id": endpoint, "device_id": device.id}),
        )
        .map_err(|_| ApiError::Backend("bootstrap encoding failed".into()))?;
        let mut response = Response::new(full(Bytes::from(bytes)));
        response.headers_mut().insert(
            "content-type",
            http::HeaderValue::from_static("application/json"),
        );
        if let Some(token) = token {
            response.headers_mut().insert(
                "set-cookie",
                device_cookie_value(token.expose_secret(), policy.secure)?,
            );
        }
        Ok(response)
    }

    pub fn cookie(
        &self,
        path: &str,
        headers: &http::HeaderMap,
        body: &Incoming,
    ) -> Result<Response<Body>> {
        let (policy, _) = self.remote().map_err(|_| ApiError::Unavailable)?;
        policy.browser(headers)?;
        if body.size_hint().upper() != Some(0) {
            return Err(ApiError::Invalid("cookie operations have no body".into()));
        }
        let expected = expected_device(headers)?;
        let cookie_present = device_cookie(headers)?.is_some();
        if path.ends_with("/logout") && cookie_present && expected.is_none() {
            return Err(ApiError::Unauthorized);
        }
        if let Some(expected) = expected
            && (cookie_present || one(headers, "authorization")?.is_some())
            && self.authenticate_device(headers)?.id != expected
        {
            return Err(ApiError::Unauthorized);
        }
        let value = if path.ends_with("/login") {
            self.authenticate_device(headers)?;
            let bearer = one(headers, "authorization")?
                .and_then(|value| value.strip_prefix("Bearer "))
                .ok_or(ApiError::Unauthorized)?;
            device_cookie_value(bearer, policy.secure)?
                .to_str()
                .map_err(|_| ApiError::Unauthorized)?
                .to_owned()
        } else {
            "rsi-device=; Path=/api; HttpOnly; SameSite=Strict; Max-Age=0".into()
        };
        let mut response = Response::new(full(Bytes::from_static(b"{}")));
        response.headers_mut().insert(
            "set-cookie",
            value.parse().map_err(|_| ApiError::Unauthorized)?,
        );
        Ok(response)
    }
}

fn expected_device(headers: &http::HeaderMap) -> Result<Option<DeviceId>> {
    one(headers, "x-rsi-expected-device")?
        .map(DeviceId::parse)
        .transpose()
}
fn device_cookie(headers: &http::HeaderMap) -> Result<Option<&str>> {
    let mut cookie = None;
    if let Some(cookies) = one(headers, "cookie")? {
        for part in cookies.split(';') {
            if let Some(value) = part.trim().strip_prefix("rsi-device=")
                && cookie.replace(value).is_some()
            {
                return Err(ApiError::Unauthorized);
            }
        }
    }
    Ok(cookie)
}

fn device_cookie_value(token: &str, secure: bool) -> Result<http::HeaderValue> {
    format!(
        "rsi-device={token}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=2592000{}",
        if secure { "; Secure" } else { "" }
    )
    .parse()
    .map_err(|_| ApiError::Unauthorized)
}
