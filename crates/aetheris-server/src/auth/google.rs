use openidconnect::core::{
    CoreClient, CoreIdTokenClaims, CoreIdTokenVerifier, CoreProviderMetadata,
};
use openidconnect::{
    ClientId, ClientSecret, EndpointMaybeSet, EndpointNotSet, EndpointSet, IssuerUrl, Nonce,
};
use std::str::FromStr;
use tonic::Status;

pub struct GoogleOidcClient {
    client: CoreClient<
        EndpointSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointMaybeSet,
        EndpointMaybeSet,
    >,
}

impl GoogleOidcClient {
    /// Creates a new Google OIDC client by performing discovery.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `GOOGLE_CLIENT_ID` or `GOOGLE_CLIENT_SECRET` environment variables are malformed.
    /// - The reqwest HTTP client cannot be initialized.
    /// - OIDC discovery fails (e.g., network issues, Google's discovery endpoint is unreachable).
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let client_id =
            std::env::var("GOOGLE_CLIENT_ID").map_err(|_| "GOOGLE_CLIENT_ID missing")?;
        let client_secret =
            std::env::var("GOOGLE_CLIENT_SECRET").map_err(|_| "GOOGLE_CLIENT_SECRET missing")?;

        let http_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let provider_metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new("https://accounts.google.com".to_string())?,
            &|req: openidconnect::HttpRequest| {
                let client = http_client.clone();
                async move {
                    let resp = client
                        .execute(req.try_into().map_err(|e| {
                            openidconnect::HttpClientError::Other(format!("Reqwest error: {e}"))
                        })?)
                        .await
                        .map_err(|e| openidconnect::HttpClientError::Reqwest(Box::new(e)))?;

                    let status = resp.status();
                    let headers = resp.headers().clone();
                    let body = resp
                        .bytes()
                        .await
                        .map_err(|e| openidconnect::HttpClientError::Reqwest(Box::new(e)))?;

                    let mut http_resp = openidconnect::HttpResponse::new(body.to_vec());
                    *http_resp.status_mut() = status;
                    *http_resp.headers_mut() = headers;
                    Ok::<_, openidconnect::HttpClientError<reqwest::Error>>(http_resp)
                }
            },
        )
        .await?;

        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        );

        Ok(Self { client })
    }

    /// Verifies a Google ID token and returns its claims.
    ///
    /// # Errors
    ///
    /// Returns a `tonic::Status` with `Unauthenticated` if:
    /// - The ID token is malformed and cannot be parsed.
    /// - The ID token signature is invalid.
    /// - The ID token claims (issuer, audience, expiry) are invalid.
    pub fn verify_token(&self, id_token: &str, nonce: &str) -> Result<CoreIdTokenClaims, Status> {
        let id_token = openidconnect::core::CoreIdToken::from_str(id_token)
            .map_err(|e| Status::unauthenticated(format!("Malformed ID token: {e}")))?;

        let verifier: CoreIdTokenVerifier = self.client.id_token_verifier();

        let claims = id_token
            .claims(&verifier, &Nonce::new(nonce.to_string()))
            .map_err(|e| Status::unauthenticated(format!("Invalid ID token claims: {e}")))?;

        Ok(claims.clone())
    }
}
