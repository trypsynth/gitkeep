use anyhow::{Context, Result, anyhow};
use octocrab::{Octocrab, OctocrabBuilder};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A small JSON-over-HTTPS client for a self-hosted forge's REST API. It reuses octocrab's
/// transport (so no extra HTTP dependency is needed) but reads raw responses, so errors report
/// the forge's own status and message rather than octocrab's GitHub-specific parsing.
pub struct Http {
	host: String,
	client: Octocrab,
}

impl Http {
	pub fn new(host: &str, token: Option<&str>) -> Result<Self> {
		let builder = OctocrabBuilder::default()
			.base_uri(format!("https://{host}"))
			.with_context(|| format!("'{host}' is not a valid host"))?;
		let client = match token {
			Some(t) => builder.personal_token(t.to_string()).build(),
			None => builder.build(),
		}
		.with_context(|| format!("Could not create a client for {host}"))?;
		Ok(Self { host: host.to_string(), client })
	}

	pub fn host(&self) -> &str {
		&self.host
	}

	/// GETs `path` (starting with `/`) and returns its HTTP status and body.
	async fn fetch(&self, path: &str) -> Result<(u16, String)> {
		let response =
			self.client._get(path).await.map_err(|e| anyhow!("Could not reach {}: {}", self.host, describe(&e)))?;
		let status = response.status().as_u16();
		let body = self.client.body_to_string(response).await.map_err(|e| anyhow!("{}", describe(&e)))?;
		Ok((status, body))
	}

	/// GETs `path` and parses the JSON body. A 404 is `Ok(None)`, so callers can tell "doesn't exist"
	/// apart from network or permission failures.
	pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
		let (status, body) = self.fetch(path).await?;
		match status {
			200..=299 => serde_json::from_str(&body)
				.map(Some)
				.with_context(|| format!("{} returned an unexpected response for {path}", self.host)),
			404 => Ok(None),
			401 | 403 => Err(anyhow!(
				"{} denied the request ({}). For private repos, add a token under [hosts.\"{}\"] in the config.",
				self.host,
				error_message(status, &body),
				self.host
			)),
			_ => Err(anyhow!("Request to {} failed: {}", self.host, error_message(status, &body))),
		}
	}

	/// GETs `path` and returns only the HTTP status, for probing which API a host speaks.
	pub async fn status(&self, path: &str) -> Result<u16> {
		self.fetch(path).await.map(|(status, _)| status)
	}
}

/// Pulls a readable message out of an error response: the JSON `message` or `error` field most
/// forges send, or the status code alone.
fn error_message(status: u16, body: &str) -> String {
	serde_json::from_str::<Value>(body)
		.ok()
		.and_then(|v| ["message", "error"].iter().find_map(|k| v.get(k).and_then(Value::as_str).map(str::to_string)))
		.unwrap_or_else(|| format!("HTTP {status}"))
}

/// octocrab displays some transport errors as just "GitHub"; prefer the underlying message.
fn describe(e: &octocrab::Error) -> String {
	match e {
		octocrab::Error::GitHub { source, .. } => source.message.clone(),
		other => other.to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn error_message_prefers_json_message() {
		assert_eq!(error_message(401, r#"{"message":"401 Unauthorized"}"#), "401 Unauthorized");
	}

	#[test]
	fn error_message_falls_back_to_error_field() {
		assert_eq!(error_message(401, r#"{"error":"invalid_token"}"#), "invalid_token");
	}

	#[test]
	fn error_message_uses_status_for_non_json() {
		assert_eq!(error_message(502, "<html>Bad Gateway</html>"), "HTTP 502");
	}
}
