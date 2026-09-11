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
            format!(
                "rsi-device={bearer}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=2592000{}",
                if policy.secure { "; Secure" } else { "" }
            )
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
