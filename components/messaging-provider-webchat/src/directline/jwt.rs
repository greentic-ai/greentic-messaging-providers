use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const TTL_SECONDS: i64 = 1800;
const ISS: &str = "greentic.webchat";
const AUD: &str = "directline";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DirectLineContext {
    pub env: String,
    pub tenant: String,
    pub team: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub ctx: DirectLineContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conv: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum JwtError {
    InvalidFormat,
    InvalidSignature,
    Expired,
    NotYetValid,
    Json(serde_json::Error),
    Base64(base64::DecodeError),
}

impl From<serde_json::Error> for JwtError {
    fn from(err: serde_json::Error) -> Self {
        JwtError::Json(err)
    }
}

impl From<base64::DecodeError> for JwtError {
    fn from(err: base64::DecodeError) -> Self {
        JwtError::Base64(err)
    }
}

fn encode_segment<T: Serialize>(value: &T) -> Result<String, JwtError> {
    let json = serde_json::to_string(value)?;
    Ok(URL_SAFE_NO_PAD.encode(json.as_bytes()))
}

fn decode_segment<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, JwtError> {
    let bytes = URL_SAFE_NO_PAD.decode(value)?;
    let decoded = serde_json::from_slice(&bytes)?;
    Ok(decoded)
}

pub fn issue_token(
    secret: &[u8],
    ctx: DirectLineContext,
    sub: &str,
    conv: Option<String>,
) -> Result<(String, i64), JwtError> {
    let now = Utc::now();
    let iat = now.timestamp();
    let exp = (now + Duration::seconds(TTL_SECONDS)).timestamp();
    let claims = TokenClaims {
        iss: ISS.to_string(),
        aud: AUD.to_string(),
        sub: sub.to_string(),
        iat,
        nbf: iat,
        exp,
        ctx,
        conv,
    };
    let header = serde_json::json!({"alg":"HS256","typ":"JWT"});
    let header_enc = encode_segment(&header)?;
    let payload_enc = encode_segment(&claims)?;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key length valid");
    mac.update(header_enc.as_bytes());
    mac.update(b".");
    mac.update(payload_enc.as_bytes());
    let signature = mac.finalize().into_bytes();
    let signature_enc = URL_SAFE_NO_PAD.encode(signature);
    let token = format!("{header_enc}.{payload_enc}.{signature_enc}");
    Ok((token, exp))
}

pub fn verify_token(secret: &[u8], token: &str) -> Result<TokenClaims, JwtError> {
    let mut parts = token.split('.');
    let header = parts.next().ok_or(JwtError::InvalidFormat)?;
    let payload = parts.next().ok_or(JwtError::InvalidFormat)?;
    let signature = parts.next().ok_or(JwtError::InvalidFormat)?;
    if parts.next().is_some() {
        return Err(JwtError::InvalidFormat);
    }
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key length valid");
    mac.update(header.as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    let expected = mac.finalize().into_bytes();
    let decoded_sig = URL_SAFE_NO_PAD.decode(signature)?;
    if expected.as_slice() != decoded_sig {
        return Err(JwtError::InvalidSignature);
    }
    let claims: TokenClaims = decode_segment(payload)?;
    let now = Utc::now().timestamp();
    if now < claims.nbf {
        return Err(JwtError::NotYetValid);
    }
    if now >= claims.exp {
        return Err(JwtError::Expired);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let secret = b"super-secure-key";
        let ctx = DirectLineContext {
            env: "default".into(),
            tenant: "default".into(),
            team: Some("team-a".into()),
        };
        let (token, exp) = issue_token(secret, ctx.clone(), "user-123", None).unwrap();
        assert!(token.split('.').count() == 3);
        assert!(exp > Utc::now().timestamp());
        let claims = verify_token(secret, &token).unwrap();
        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.ctx, ctx);
        assert!(claims.conv.is_none());
    }

    #[test]
    fn token_with_conv_claim() {
        let secret = b"_another-secret-key_";
        let ctx = DirectLineContext {
            env: "prod".into(),
            tenant: "tenant-a".into(),
            team: None,
        };
        let (token, _) =
            issue_token(secret, ctx.clone(), "user-x", Some("conv-99".into())).unwrap();
        let claims = verify_token(secret, &token).unwrap();
        assert_eq!(claims.conv.as_deref(), Some("conv-99"));
        assert_eq!(claims.ctx, ctx);
    }
}
